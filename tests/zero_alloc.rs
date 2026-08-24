//! Gate (e): after construction, the decode and encode hot paths perform
//! zero heap allocations, measured with a counting global allocator.
//!
//! Scoping. The counter is thread-local: the global allocator increments
//! the counter of the allocating thread only, so allocations by the libtest
//! harness threads (output capture, thread spawning) and by any other test
//! never land in a measured window. Each measurement runs on a dedicated
//! thread, between two marks, and the measurements are serialized through
//! a mutex. This file is its own test binary, and CI runs it as
//! `cargo test --test zero_alloc -- --test-threads=1`.
//!
//! The library itself is `#![forbid(unsafe_code)]`; the `unsafe` here is the
//! allocator shim this test harness needs and lives only in the test crate.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::Cursor;
use std::sync::Mutex;

struct CountingAlloc;

thread_local! {
    /// Allocations made by the current thread. A `const` initializer over a
    /// `Cell<usize>` (no destructor) makes access itself allocation-free.
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn count_one() {
    // `try_with` rather than `with`: a thread in teardown must not panic
    // from inside the allocator.
    let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
}

fn allocations() -> usize {
    ALLOCATIONS.with(|c| c.get())
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_one();
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_one();
        System.alloc_zeroed(layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count_one();
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

use y4m_codec::{Decoder, EncoderBuilder, Error};

/// Serializes the measurements, so two windows never overlap even when the
/// harness runs tests in parallel.
static SERIAL: Mutex<()> = Mutex::new(());

/// Runs `f` on a dedicated thread and returns its result, so the measured
/// window lives on a thread that does nothing else.
fn on_measurement_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::thread::Builder::new()
        .name("zero-alloc-measurement".into())
        .spawn(f)
        .expect("spawn measurement thread")
        .join()
        .expect("measurement thread panicked")
}

/// The counter must see allocations on its own thread, else a zero reading
/// proves nothing.
#[test]
fn counter_detects_allocations() {
    on_measurement_thread(|| {
        let before = allocations();
        let v: Vec<u8> = std::hint::black_box(vec![1u8; 64]);
        let after = allocations();
        drop(v);
        assert!(after > before, "thread-local counter missed an allocation");
    });
}

/// Allocations on another thread never reach this thread's counter.
#[test]
fn counter_is_thread_local() {
    on_measurement_thread(|| {
        let before = allocations();
        std::thread::spawn(|| {
            let v: Vec<u8> = std::hint::black_box(vec![1u8; 4096]);
            drop(v);
        })
        .join()
        .unwrap();
        // The spawn above allocates on this thread too (thread handle, name),
        // so compare against an identical spawn that allocates nothing extra:
        // the child's 4096-byte vector must not add to this thread's count
        // beyond what spawning itself costs.
        let middle = allocations();
        std::thread::spawn(|| {}).join().unwrap();
        let after = allocations();
        assert_eq!(
            middle - before,
            after - middle,
            "child-thread allocation leaked into this thread's counter"
        );
    });
}

/// Decode every frame of the fixture and re-encode it, asserting zero
/// allocations inside the loop and a byte-exact result outside it.
fn assert_zero_alloc_roundtrip(fixture: &str) {
    let bytes = common::fixture_bytes(fixture);

    // Construction phase: allocations here are expected (frame buffer,
    // BufReader, header capture, encoder scratch, output capacity).
    let mut decoder = Decoder::new(Cursor::new(&bytes)).unwrap();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut encoder = EncoderBuilder::new(decoder.width(), decoder.height(), decoder.framerate())
        .raw_params(decoder.raw_params())
        .unwrap()
        .write_header(&mut out)
        .unwrap();

    let before = allocations();
    let mut frames = 0usize;
    loop {
        match decoder.read_frame() {
            Ok(frame) => {
                std::hint::black_box(frame.y());
                encoder
                    .write_frame_with_params([frame.y(), frame.u(), frame.v()], frame.raw_params())
                    .unwrap();
                frames += 1;
            }
            Err(Error::EndOfStream) => break,
            Err(e) => panic!("{fixture}: {e}"),
        }
    }
    let after = allocations();

    assert_eq!(
        after - before,
        0,
        "{fixture}: hot path allocated over {frames} frames"
    );
    assert!(frames > 0, "{fixture}: no frames decoded");
    assert_eq!(out, bytes, "{fixture}: round-trip bytes differ");
}

#[test]
fn zero_alloc_hot_path() {
    on_measurement_thread(|| {
        // Parametered frames: each frame header is parsed into a reusable buffer.
        assert_zero_alloc_roundtrip("fixtures/frame_params_2x2_2f.y4m");
        // High bit depth: exercises the u16 sample view and LE handling.
        assert_zero_alloc_roundtrip("fixtures/c420p10_2x2_2f_LE.y4m");
    });
}

/// The u16 encode path also stays allocation-free per frame: samples go
/// through the scratch buffer allocated once at header time.
#[test]
fn zero_alloc_u16_encode_path() {
    on_measurement_thread(|| {
        let bytes = common::fixture_bytes("fixtures/c420p10_2x2_2f_LE.y4m");
        let mut decoder = Decoder::new(Cursor::new(&bytes)).unwrap();
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
        let mut encoder = EncoderBuilder::new(2, 2, decoder.framerate())
            .interlace(y4m_codec::Interlace::Progressive)
            .pixel_aspect(y4m_codec::Ratio::new(1, 1))
            .colorspace(y4m_codec::Colorspace::C420P10)
            .write_header(&mut out)
            .unwrap();

        let before = allocations();
        let mut frames = 0usize;
        loop {
            match decoder.read_frame() {
                Ok(frame) => {
                    encoder
                        .write_frame_u16([
                            frame.plane_u16(0).unwrap(),
                            frame.plane_u16(1).unwrap(),
                            frame.plane_u16(2).unwrap(),
                        ])
                        .unwrap();
                    frames += 1;
                }
                Err(Error::EndOfStream) => break,
                Err(e) => panic!("decode: {e}"),
            }
        }
        let after = allocations();

        assert_eq!(after - before, 0, "u16 encode path allocated");
        assert_eq!(frames, 2);
        assert_eq!(out, bytes);
    });
}
