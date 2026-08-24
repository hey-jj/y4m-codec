//! Replay of the frozen YUV4MPEG2 conformance vectors
//! (`tests/conformance-vectors.json`) against the fixtures.
//!
//! Covers CI gates (a) byte-exact round-trip, (b) `be_wire_bytes`,
//! (c) plane-size correctness, and (d) typed errors on malformed input.

mod common;

use std::io::Cursor;

use y4m_codec::{Colorspace, Decoder, EncoderBuilder, Error, Interlace, Ratio};

fn error_class(e: &Error) -> &'static str {
    match e {
        Error::EndOfStream => "eof",
        Error::Truncated => "truncated",
        Error::Parse(_) => "parse",
        Error::Unsupported(_) => "unsupported",
        Error::Limits => "limits",
        Error::Io(_) => "io",
        Error::BadInput => "badinput",
        _ => "other",
    }
}

fn expected_colorspace(spec: &str) -> Colorspace {
    let tag = spec
        .strip_prefix('C')
        .expect("expect.colorspace starts with C");
    Colorspace::from_tag(tag).expect("expect.colorspace is a recognized tag")
}

fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// Gates (a) and (c): every valid vector decodes to the manifest's header
/// fields, plane geometry, and frame count, and re-encodes byte-for-byte
/// with the manifest's sha256.
#[test]
fn valid_vectors_decode_and_roundtrip_byte_exact() {
    let manifest = common::manifest();
    let mut checked = 0usize;

    for vector in manifest["vectors"].as_array().unwrap() {
        if vector["kind"].as_str().unwrap() != "valid" {
            continue;
        }
        let file = vector["file"].as_str().unwrap();
        let expect = &vector["expect"];
        let bytes = common::fixture_bytes(file);

        // Fixture integrity against the manifest.
        assert_eq!(
            bytes.len() as u64,
            vector["size_bytes"].as_u64().unwrap(),
            "{file}: size"
        );
        let file_sha = vector["sha256"].as_str().unwrap();
        assert_eq!(
            common::sha256_hex(&bytes),
            file_sha,
            "{file}: fixture sha256"
        );

        let mut decoder = Decoder::new(Cursor::new(&bytes)).unwrap_or_else(|e| {
            panic!("{file}: stream header rejected: {e}");
        });

        assert_eq!(
            decoder.width() as u64,
            expect["width"].as_u64().unwrap(),
            "{file}: W"
        );
        assert_eq!(
            decoder.height() as u64,
            expect["height"].as_u64().unwrap(),
            "{file}: H"
        );

        let expected_cs = expected_colorspace(expect["colorspace"].as_str().unwrap());
        let cs = decoder.colorspace();
        if cs != expected_cs {
            // The ffmpeg-emitted C420 fixture is normalized to its family in
            // the manifest while the wire tag is `C420jpeg`; tag preservation
            // is still pinned by the byte-exact round-trip below.
            assert_eq!(
                file, "fixtures/ff_testsrc2_c420_32x18_2f.y4m",
                "{file}: colorspace tag"
            );
            assert_eq!(cs.tag(), "420jpeg", "{file}: wire tag");
            assert_eq!(
                cs.subsampling(),
                expected_cs.subsampling(),
                "{file}: family"
            );
        }
        assert_eq!(
            u64::from(cs.bit_depth()),
            expect["bit_depth"].as_u64().unwrap(),
            "{file}: depth"
        );
        assert_eq!(
            cs.bytes_per_sample() as u64,
            expect["bytes_per_sample"].as_u64().unwrap(),
            "{file}: bytes per sample"
        );

        let sizes = decoder.plane_sizes();
        let expect_sizes = expect["plane_sizes_bytes"].as_array().unwrap();
        for (i, s) in expect_sizes.iter().enumerate() {
            assert_eq!(
                sizes[i] as u64,
                s.as_u64().unwrap(),
                "{file}: plane {i} bytes"
            );
        }
        if let Some(dims) = expect["plane_dims_samples"].as_array() {
            for (i, wh) in dims.iter().enumerate() {
                let w = wh[0].as_u64().unwrap();
                let h = wh[1].as_u64().unwrap();
                assert_eq!(
                    sizes[i] as u64,
                    w * h * cs.bytes_per_sample() as u64,
                    "{file}: plane {i} dims {w}x{h}"
                );
            }
        }

        // Re-encode with preserved raw parameters while decoding.
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
        let mut encoder =
            EncoderBuilder::new(decoder.width(), decoder.height(), decoder.framerate())
                .raw_params(decoder.raw_params())
                .unwrap()
                .write_header(&mut out)
                .unwrap();

        let mut frames = 0u64;
        let first_payload = vector["payload_first_bytes_hex"].as_str().map(hex_decode);
        loop {
            match decoder.read_frame() {
                Ok(frame) => {
                    for (i, &size) in sizes.iter().enumerate() {
                        assert_eq!(
                            frame.plane(i).len(),
                            size,
                            "{file}: frame {frames} plane {i} len"
                        );
                        match frame.plane_u16(i) {
                            Some(p16) => {
                                assert_eq!(cs.bytes_per_sample(), 2, "{file}: u16 view on 8-bit");
                                assert_eq!(p16.len(), size / 2, "{file}: plane {i} samples");
                            }
                            None => {
                                assert_eq!(cs.bytes_per_sample(), 1, "{file}: missing u16 view")
                            }
                        }
                    }
                    if frames == 0 {
                        if let Some(prefix) = &first_payload {
                            assert!(
                                frame.y().starts_with(prefix),
                                "{file}: first payload bytes mismatch"
                            );
                        }
                    }
                    encoder
                        .write_frame_with_params(
                            [frame.y(), frame.u(), frame.v()],
                            frame.raw_params(),
                        )
                        .unwrap();
                    frames += 1;
                }
                Err(Error::EndOfStream) => break,
                Err(e) => panic!("{file}: frame {frames} failed: {e}"),
            }
        }
        assert_eq!(
            frames,
            expect["frames"].as_u64().unwrap(),
            "{file}: frame count"
        );

        assert_eq!(out, bytes, "{file}: round-trip bytes differ");
        assert_eq!(
            common::sha256_hex(&out),
            file_sha,
            "{file}: round-trip sha256"
        );
        checked += 1;
    }

    assert_eq!(checked, 21, "expected 21 valid vectors");
}

/// Gate (d): every malformed vector yields the manifest's error class,
/// with no panic.
#[test]
fn malformed_vectors_yield_typed_errors() {
    let manifest = common::manifest();
    let mut checked = 0usize;

    for vector in manifest["vectors"].as_array().unwrap() {
        if vector["kind"].as_str().unwrap() != "malformed" {
            continue;
        }
        let file = vector["file"].as_str().unwrap();
        let expected_class = vector["expect"]["error"].as_str().unwrap();
        let bytes = common::fixture_bytes(file);
        assert_eq!(
            common::sha256_hex(&bytes),
            vector["sha256"].as_str().unwrap(),
            "{file}: sha256"
        );

        let mut frames = 0usize;
        let err = match Decoder::new(Cursor::new(&bytes)) {
            Err(e) => e,
            Ok(mut decoder) => loop {
                match decoder.read_frame() {
                    Ok(_) => frames += 1,
                    Err(e) => break e,
                }
            },
        };
        assert_eq!(
            error_class(&err),
            expected_class,
            "{file}: got {err:?} after {frames} frames"
        );
        if file == "fixtures/malformed/truncated_second_frame.y4m" {
            assert_eq!(frames, 1, "{file}: first frame must decode");
        }
        checked += 1;
    }

    assert_eq!(checked, 16, "expected 16 malformed vectors");
}

/// Gate (b): `write_frame_u16` emits little-endian wire bytes on every host.
/// Encoding the pinned samples reproduces `c420p10_2x2_2f_LE.y4m`
/// byte-for-byte, and decoding yields the pinned host-order values.
#[test]
fn be_wire_bytes() {
    let manifest = common::manifest();
    let vector = manifest["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["file"] == "fixtures/c420p10_2x2_2f_LE.y4m")
        .unwrap();
    let bytes = common::fixture_bytes("fixtures/c420p10_2x2_2f_LE.y4m");

    // Decode side: host-order sample values, independent of host endianness.
    let mut decoder = Decoder::new(Cursor::new(&bytes)).unwrap();
    let mut frames: Vec<[Vec<u16>; 3]> = Vec::new();
    loop {
        match decoder.read_frame() {
            Ok(frame) => {
                assert_eq!(
                    frame.plane_u16(0).unwrap(),
                    &[0x0001, 0x0201, 0x03FF, 0x0100],
                    "pinned Y samples"
                );
                assert_eq!(frame.y(), &[0x01, 0x00, 0x01, 0x02, 0xff, 0x03, 0x00, 0x01]);
                frames.push([
                    frame.plane_u16(0).unwrap().to_vec(),
                    frame.plane_u16(1).unwrap().to_vec(),
                    frame.plane_u16(2).unwrap().to_vec(),
                ]);
            }
            Err(Error::EndOfStream) => break,
            Err(e) => panic!("decode failed: {e}"),
        }
    }
    assert_eq!(frames.len(), 2);

    // Encode side: canonical header plus u16 planes, byte-exact output.
    let mut out: Vec<u8> = Vec::new();
    let mut encoder = EncoderBuilder::new(2, 2, Ratio::new(25, 1))
        .interlace(Interlace::Progressive)
        .pixel_aspect(Ratio::new(1, 1))
        .colorspace(Colorspace::C420P10)
        .write_header(&mut out)
        .unwrap();
    for planes in &frames {
        encoder
            .write_frame_u16([&planes[0], &planes[1], &planes[2]])
            .unwrap();
    }
    assert_eq!(out, bytes, "wire bytes must be little-endian on every host");
    assert_eq!(
        common::sha256_hex(&out),
        vector["sha256"].as_str().unwrap(),
        "round-trip sha256"
    );
}
