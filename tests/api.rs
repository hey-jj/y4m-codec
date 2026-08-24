//! API-level behavior beyond the conformance vectors: defaults, limits,
//! validation, canonical emit order, and error surface.

mod common;

use std::io::Cursor;

use y4m_codec::{
    ChromaSiting, Colorspace, Decoder, EncoderBuilder, Error, Interlace, Limits, Ratio, Subsampling,
};

#[test]
fn absent_framerate_defaults_without_reemitting() {
    let input = b"YUV4MPEG2 W2 H2\nFRAME\n\x01\x02\x03\x04\x05\x06";
    let mut decoder = Decoder::new(Cursor::new(&input[..])).unwrap();
    assert_eq!(decoder.framerate(), Ratio::new(25, 1));
    assert_eq!(decoder.raw_params(), b"W2 H2");

    let mut out = Vec::new();
    let mut encoder = EncoderBuilder::new(decoder.width(), decoder.height(), decoder.framerate())
        .raw_params(decoder.raw_params())
        .unwrap()
        .write_header(&mut out)
        .unwrap();
    let frame = decoder.read_frame().unwrap();
    encoder
        .write_frame([frame.y(), frame.u(), frame.v()])
        .unwrap();
    assert_eq!(out, input, "raw round-trip must not invent the F tag");
}

#[test]
fn absent_colorspace_defaults_to_420_8bit() {
    let input = b"YUV4MPEG2 W2 H2 F25:1\n";
    let decoder = Decoder::new(Cursor::new(&input[..])).unwrap();
    let cs = decoder.colorspace();
    assert_eq!(cs.subsampling(), Subsampling::C420);
    assert_eq!(cs.bit_depth(), 8);
    assert_eq!(cs.chroma_siting(), ChromaSiting::Unspecified);
    assert_eq!(decoder.pixel_aspect(), Ratio::new(0, 0));
    assert_eq!(decoder.interlace(), Interlace::Unknown);
}

#[test]
fn duplicate_tags_last_one_wins() {
    let input = b"YUV4MPEG2 W2 W4 H2 F25:1\n";
    let decoder = Decoder::new(Cursor::new(&input[..])).unwrap();
    assert_eq!(decoder.width(), 4);
}

#[test]
fn unknown_tag_letter_is_tolerated_and_preserved() {
    let input = b"YUV4MPEG2 W2 H2 F25:1 Q9\n";
    let decoder = Decoder::new(Cursor::new(&input[..])).unwrap();
    assert_eq!(decoder.raw_params(), b"W2 H2 F25:1 Q9");
}

#[test]
fn interlace_values_parse() {
    for (value, expected) in [
        (&b"Ip"[..], Interlace::Progressive),
        (b"It", Interlace::TopFirst),
        (b"Ib", Interlace::BottomFirst),
        (b"Im", Interlace::Mixed),
        (b"I?", Interlace::Unknown),
    ] {
        let mut input = b"YUV4MPEG2 W2 H2 F25:1 ".to_vec();
        input.extend_from_slice(value);
        input.push(b'\n');
        let decoder = Decoder::new(Cursor::new(input)).unwrap();
        assert_eq!(decoder.interlace(), expected);
    }
    let bad = b"YUV4MPEG2 W2 H2 F25:1 Ix\n";
    assert!(matches!(
        Decoder::new(Cursor::new(&bad[..])),
        Err(Error::Parse(_))
    ));
}

#[test]
fn mono_streams_have_empty_chroma() {
    let bytes = common::fixture_bytes("fixtures/mono12_3x3_1f.y4m");
    let mut decoder = Decoder::new(Cursor::new(&bytes)).unwrap();
    assert_eq!(decoder.plane_sizes(), [18, 0, 0]);
    let frame = decoder.read_frame().unwrap();
    assert!(frame.u().is_empty());
    assert!(frame.v().is_empty());
    assert_eq!(frame.plane_u16(0).unwrap().len(), 9);
    assert_eq!(frame.plane_u16(1).unwrap().len(), 0);
}

#[test]
fn eight_bit_streams_have_no_u16_view() {
    let bytes = common::fixture_bytes("fixtures/c420_8bit_4x4_2f.y4m");
    let mut decoder = Decoder::new(Cursor::new(&bytes)).unwrap();
    let frame = decoder.read_frame().unwrap();
    assert!(frame.plane_u16(0).is_none());
}

#[test]
fn frame_budget_is_enforced_before_allocation() {
    let bytes = common::fixture_bytes("fixtures/c420_8bit_4x4_2f.y4m");
    let result = Decoder::with_limits(Cursor::new(&bytes), Limits { max_frame_bytes: 4 });
    assert!(
        matches!(result, Err(Error::Limits)),
        "24-byte frame over a 4-byte budget"
    );
}

#[test]
fn hi_depth_frame_budget_counts_both_buffers() {
    // C420p16 2x2: 12 wire bytes per frame plus a 12-byte u16 sample
    // buffer, so peak frame memory is 24 bytes.
    let header = b"YUV4MPEG2 W2 H2 F25:1 C420p16\n";

    let at_peak = Decoder::with_limits(
        Cursor::new(header.to_vec()),
        Limits {
            max_frame_bytes: 24,
        },
    );
    assert!(at_peak.is_ok(), "budget equal to the two-buffer total");

    let under_peak = Decoder::with_limits(
        Cursor::new(header.to_vec()),
        Limits {
            max_frame_bytes: 23,
        },
    );
    assert!(
        matches!(under_peak, Err(Error::Limits)),
        "budget one byte under the two-buffer total"
    );

    // Under the default 1 GiB budget, a 16384x16384 C420p16 header asks
    // for 768 MiB of wire bytes plus 768 MiB of u16 samples: rejected
    // with nothing allocated.
    let bomb = b"YUV4MPEG2 W16384 H16384 F25:1 C420p16\n";
    assert!(matches!(
        Decoder::new(Cursor::new(bomb.to_vec())),
        Err(Error::Limits)
    ));
}

#[test]
fn end_of_stream_is_stable_across_calls() {
    let bytes = common::fixture_bytes("fixtures/zero_frames_header_only.y4m");
    let mut decoder = Decoder::new(Cursor::new(&bytes)).unwrap();
    assert!(matches!(decoder.read_frame(), Err(Error::EndOfStream)));
    assert!(matches!(decoder.read_frame(), Err(Error::EndOfStream)));
}

#[test]
fn canonical_emit_order_and_sentinel_aspect() {
    let mut out = Vec::new();
    let encoder = EncoderBuilder::new(32, 18, Ratio::new(30, 1))
        .interlace(Interlace::Progressive)
        .pixel_aspect(Ratio::new(0, 0))
        .colorspace(Colorspace::C420P10)
        .extension(b"YSCSS=420P10")
        .unwrap()
        .extension(b"COLORRANGE=FULL")
        .unwrap()
        .write_header(&mut out)
        .unwrap();
    drop(encoder);
    assert_eq!(
        out,
        b"YUV4MPEG2 W32 H18 F30:1 Ip A0:0 C420p10 XYSCSS=420P10 XCOLORRANGE=FULL\n"
    );
}

#[test]
fn omitted_tags_stay_omitted() {
    let mut out = Vec::new();
    EncoderBuilder::new(2, 2, Ratio::new(25, 1))
        .write_header(&mut out)
        .unwrap();
    assert_eq!(out, b"YUV4MPEG2 W2 H2 F25:1 C420\n");

    let mut out2 = Vec::new();
    EncoderBuilder::new(2, 2, Ratio::new(25, 1))
        .interlace(Interlace::Unknown)
        .write_header(&mut out2)
        .unwrap();
    assert_eq!(
        out2, b"YUV4MPEG2 W2 H2 F25:1 C420\n",
        "Unknown interlace is omitted"
    );
}

#[test]
fn writer_validates_inputs() {
    // Zero dimensions and zero framerate denominator.
    assert!(matches!(
        EncoderBuilder::new(0, 2, Ratio::new(25, 1)).write_header(Vec::new()),
        Err(Error::BadInput)
    ));
    assert!(matches!(
        EncoderBuilder::new(2, 2, Ratio::new(25, 0)).write_header(Vec::new()),
        Err(Error::BadInput)
    ));

    // Extensions must be non-empty with no SP or LF.
    let builder = EncoderBuilder::new(2, 2, Ratio::new(25, 1));
    assert!(matches!(
        builder.clone().extension(b""),
        Err(Error::BadInput)
    ));
    assert!(matches!(
        builder.clone().extension(b"a b"),
        Err(Error::BadInput)
    ));
    assert!(matches!(
        builder.clone().extension(b"a\nb"),
        Err(Error::BadInput)
    ));

    // Plane length mismatches.
    let mut encoder = builder.clone().write_header(Vec::new()).unwrap();
    assert!(matches!(
        encoder.write_frame([&[0u8; 3], &[0u8; 1], &[0u8; 1]]),
        Err(Error::BadInput)
    ));
    assert!(
        matches!(
            encoder.write_frame_u16([&[0u16; 2], &[0u16; 1], &[0u16; 1]]),
            Err(Error::BadInput),
        ),
        "u16 path on an 8-bit stream is rejected"
    );

    // Frame params: no LF, no empty pieces.
    assert!(matches!(
        encoder.write_frame_with_params([&[0u8; 4], &[0u8; 1], &[0u8; 1]], b"X1\nX2"),
        Err(Error::BadInput)
    ));
    assert!(matches!(
        encoder.write_frame_with_params([&[0u8; 4], &[0u8; 1], &[0u8; 1]], b"X1  X2"),
        Err(Error::BadInput)
    ));

    // u16 path: sample-count mismatch and out-of-range samples.
    let mut hi = EncoderBuilder::new(2, 2, Ratio::new(25, 1))
        .colorspace(Colorspace::C420P10)
        .write_header(Vec::new())
        .unwrap();
    assert!(matches!(
        hi.write_frame_u16([&[0u16; 3], &[0u16; 1], &[0u16; 1]]),
        Err(Error::BadInput)
    ));
    assert!(
        matches!(
            hi.write_frame_u16([&[0x0400u16, 0, 0, 0], &[0u16; 1], &[0u16; 1]]),
            Err(Error::BadInput)
        ),
        "0x400 exceeds 10-bit range"
    );
    hi.write_frame_u16([&[0x03FFu16, 0, 0, 0], &[0u16; 1], &[0u16; 1]])
        .unwrap();
}

#[test]
fn raw_params_override_builder_geometry() {
    let mut out = Vec::new();
    let mut encoder = EncoderBuilder::new(999, 999, Ratio::new(1, 1))
        .raw_params(b"W2 H2 F25:1 Cmono")
        .unwrap()
        .write_header(&mut out)
        .unwrap();
    encoder.write_frame([&[0u8; 4], &[], &[]]).unwrap();
    assert_eq!(out, b"YUV4MPEG2 W2 H2 F25:1 Cmono\nFRAME\n\x00\x00\x00\x00");
}

#[test]
fn error_display_and_source() {
    let err = Error::Unsupported("999".to_owned());
    assert_eq!(err.to_string(), "unsupported colorspace tag: C999");
    let io_err = Error::from(std::io::Error::new(std::io::ErrorKind::Other, "boom"));
    assert!(std::error::Error::source(&io_err).is_some());
    let eof = Error::from(std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        "eof",
    ));
    assert!(matches!(eof, Error::Truncated));
}

#[test]
fn header_line_budget_is_4096_bytes() {
    // Exactly 4096 bytes including LF: accepted.
    let mut input = b"YUV4MPEG2 W2 H2 F25:1 X".to_vec();
    let pad = 4096 - input.len() - 1;
    input.extend(std::iter::repeat(b'a').take(pad));
    input.push(b'\n');
    assert_eq!(input.len(), 4096);
    assert!(Decoder::new(Cursor::new(input.clone())).is_ok());

    // One byte more: Limits.
    let mut over = b"YUV4MPEG2 W2 H2 F25:1 X".to_vec();
    over.extend(std::iter::repeat(b'a').take(pad + 1));
    over.push(b'\n');
    assert!(matches!(
        Decoder::new(Cursor::new(over)),
        Err(Error::Limits)
    ));
}

#[test]
fn out_of_range_samples_are_tolerated_on_read() {
    // 10-bit stream carrying 0xFFFF: read succeeds, value preserved.
    let mut input = b"YUV4MPEG2 W1 H1 F25:1 Cmono10\n".to_vec();
    input.extend_from_slice(b"FRAME\n\xff\xff");
    let mut decoder = Decoder::new(Cursor::new(input)).unwrap();
    let frame = decoder.read_frame().unwrap();
    assert_eq!(frame.plane_u16(0).unwrap(), &[0xFFFF]);
}
