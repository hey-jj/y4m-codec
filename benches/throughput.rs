//! Gate (e): 1080p throughput bench with a pinned regression floor.
//!
//! Synthesizes in-memory 1920x1080 C420 and C420p10 streams, then times a
//! full decode plus byte-exact re-encode of every frame. Criterion reports
//! the measured throughput in input bytes per second. After the Criterion
//! run, a release build also checks a best-of-five pass against the pinned
//! floor below and exits non-zero when a case falls under it, so a
//! regression such as byte-at-a-time payload reads or per-frame allocation
//! fails `cargo bench`. `cargo test --benches` (Criterion test mode) and
//! debug builds run the cases once without enforcing the floor.

use std::io::Cursor;
use std::time::{Duration, Instant};

use criterion::{Criterion, Throughput};
use y4m_codec::{Colorspace, Decoder, EncoderBuilder, Error, Ratio};

const WIDTH: usize = 1920;
const HEIGHT: usize = 1080;
const FRAMES: usize = 4;

/// Pinned floors in MiB of input per second (decode plus re-encode). Set
/// far below a healthy run so they catch order-of-magnitude regressions
/// rather than machine-to-machine variance.
const CASES: [(&str, Colorspace, f64); 2] = [
    ("1080p_c420", Colorspace::C420, 400.0),
    ("1080p_c420p10", Colorspace::C420P10, 150.0),
];

fn synth_stream(cs: Colorspace, frames: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = EncoderBuilder::new(WIDTH, HEIGHT, Ratio::new(30, 1))
        .colorspace(cs)
        .write_header(&mut out)
        .expect("header");
    let bps = cs.bytes_per_sample();
    let chroma = (WIDTH / 2) * (HEIGHT / 2) * bps;
    let y: Vec<u8> = (0..WIDTH * HEIGHT * bps).map(|i| (i % 251) as u8).collect();
    let u: Vec<u8> = (0..chroma).map(|i| (i % 241) as u8).collect();
    let v: Vec<u8> = (0..chroma).map(|i| (i % 239) as u8).collect();
    for _ in 0..frames {
        encoder.write_frame([&y, &u, &v]).expect("frame");
    }
    out
}

/// One timed decode plus re-encode pass over `stream`. Returns the elapsed
/// time after asserting the output is byte-exact.
fn pass(stream: &[u8], out: &mut Vec<u8>) -> Duration {
    out.clear();
    let start = Instant::now();
    let mut decoder = Decoder::new(Cursor::new(stream)).expect("decode header");
    let mut encoder = EncoderBuilder::new(decoder.width(), decoder.height(), decoder.framerate())
        .raw_params(decoder.raw_params())
        .expect("raw params")
        .write_header(&mut *out)
        .expect("encode header");
    loop {
        match decoder.read_frame() {
            Ok(frame) => {
                if let Some(p) = frame.plane_u16(0) {
                    std::hint::black_box(p[0]);
                }
                encoder
                    .write_frame_with_params([frame.y(), frame.u(), frame.v()], frame.raw_params())
                    .expect("encode frame");
            }
            Err(Error::EndOfStream) => break,
            Err(e) => panic!("decode: {e}"),
        }
    }
    let elapsed = start.elapsed();
    assert!(out == stream, "round-trip bytes differ");
    elapsed
}

fn criterion_cases(c: &mut Criterion) {
    let mut group = c.benchmark_group("1080p_decode_reencode");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(4));
    for (name, cs, _) in CASES {
        let stream = synth_stream(cs, FRAMES);
        let mut out = Vec::with_capacity(stream.len());
        group.throughput(Throughput::Bytes(stream.len() as u64));
        group.bench_function(name, |b| b.iter(|| pass(&stream, &mut out)));
    }
    group.finish();
}

/// Best-of-five pass per case against the pinned floor. Returns whether
/// every case held.
fn enforce_floor() -> bool {
    let mut held = true;
    for (name, cs, floor) in CASES {
        let stream = synth_stream(cs, FRAMES);
        let mut out = Vec::with_capacity(stream.len());
        pass(&stream, &mut out);
        let best = (0..5)
            .map(|_| pass(&stream, &mut out))
            .min()
            .expect("five runs");
        let mibs = stream.len() as f64 / (1024.0 * 1024.0) / best.as_secs_f64();
        let verdict = if mibs >= floor {
            "ok"
        } else {
            held = false;
            "BELOW FLOOR"
        };
        println!("floor check {name:<16} {mibs:>8.1} MiB/s  (floor {floor:>6.1})  {verdict}");
    }
    held
}

fn main() {
    let mut criterion = Criterion::default().configure_from_args();
    criterion_cases(&mut criterion);
    criterion.final_summary();

    let test_mode = std::env::args().any(|a| a == "--test" || a == "--list");
    if test_mode || cfg!(debug_assertions) {
        println!("floor check skipped: test mode or debug build");
        return;
    }
    if !enforce_floor() {
        eprintln!("throughput regression: a case fell below its pinned floor");
        std::process::exit(1);
    }
}
