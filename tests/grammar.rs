//! Header grammar regressions. Gate (f): writer-side parameter validation,
//! so no public call can emit a header the reader cannot parse back. Gate
//! (g): the frame header grammar `"FRAME" *( SP parameter ) LF` read
//! strictly, and the byte-exact round-trip contract over every accepted
//! input in `tests/fixtures/` and the fuzz corpus.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use y4m_codec::{Colorspace, Decoder, EncoderBuilder, Error, Interlace, Limits, Ratio};

/// The bytes the writer refuses inside any parameter: the separator, the
/// line terminator, CR, and NUL.
const FORBIDDEN: [u8; 4] = [b' ', b'\n', b'\r', 0];

/// Frame byte budget of the fuzz target, applied to fuzz corpus entries so
/// "accepted" means the same thing here as it does under the fuzzer.
const FUZZ_LIMITS: Limits = Limits {
    max_frame_bytes: 1 << 16,
};

/// Decodes `input` fully and re-encodes it with preserved raw parameters.
/// Returns the frame count and the re-encoded bytes.
fn roundtrip(input: &[u8], limits: Limits) -> Result<(usize, Vec<u8>), Error> {
    let mut decoder = Decoder::with_limits(Cursor::new(input), limits)?;
    let mut out = Vec::with_capacity(input.len());
    let mut encoder = EncoderBuilder::new(decoder.width(), decoder.height(), decoder.framerate())
        .raw_params(decoder.raw_params())?
        .write_header(&mut out)?;
    let mut frames = 0;
    loop {
        match decoder.read_frame() {
            Ok(frame) => {
                encoder.write_frame_with_params(
                    [frame.y(), frame.u(), frame.v()],
                    frame.raw_params(),
                )?;
                frames += 1;
            }
            Err(Error::EndOfStream) => break,
            Err(e) => return Err(e),
        }
    }
    Ok((frames, out))
}

fn builder() -> EncoderBuilder {
    EncoderBuilder::new(2, 2, Ratio::new(25, 1))
}

/// Every writer-side parameter input (`raw_params`, `extension`,
/// `write_frame_with_params`) rejects SP, LF, CR, and NUL inside a
/// parameter and rejects a parameter with no value, with `BadInput` and
/// nothing written to the sink.
#[test]
fn header_injection_rejected() {
    // A tag with no value: the parameter is shorter than two bytes.
    let valueless: &[&[u8]] = &[b"", b"Q", b"W2 H2 F25:1 Q", b"W2 H2 F25:1 X"];

    for &b in &FORBIDDEN {
        // Injection points for the stream header parameter list. For SP the
        // middle forms double a separator or leave a one-byte parameter.
        let raw_cases: Vec<Vec<u8>> = vec![
            [b"W2 H2 F25:1 XA".as_slice(), &[b], b"B"].concat(),
            [b"W2".as_slice(), &[b], b" H2 F25:1"].concat(),
            [&[b], b"W2 H2 F25:1".as_slice()].concat(),
            [b"W2 H2 F25:1".as_slice(), &[b]].concat(),
            [b"W2 H2 F25:1 X".as_slice(), &[b]].concat(),
        ];
        for raw in raw_cases
            .iter()
            .map(Vec::as_slice)
            .chain(valueless.iter().copied())
        {
            let result = builder().raw_params(raw);
            assert!(
                matches!(result, Err(Error::BadInput)),
                "raw_params({raw:?}) must be BadInput, got {:?}",
                result.map(|_| ())
            );
        }

        // Extension values.
        let ext_cases: Vec<Vec<u8>> = vec![
            [&[b], b"a".as_slice()].concat(),
            [b"a".as_slice(), &[b], b"b"].concat(),
            [b"a".as_slice(), &[b]].concat(),
            vec![b],
        ];
        for value in ext_cases.iter().map(Vec::as_slice).chain([b"".as_slice()]) {
            let result = builder().extension(value);
            assert!(
                matches!(result, Err(Error::BadInput)),
                "extension({value:?}) must be BadInput, got {:?}",
                result.map(|_| ())
            );
        }

        // Frame parameters: the header is already on the wire, and the
        // rejected frame adds zero bytes after it. For SP the middle form
        // leaves a one-byte parameter behind the separator.
        let frame_cases: Vec<Vec<u8>> = vec![
            [b"X1".as_slice(), &[b], b"Q"].concat(),
            [&[b], b"X1".as_slice()].concat(),
            [b"X1".as_slice(), &[b]].concat(),
            [b"X".as_slice(), &[b]].concat(),
        ];
        for params in
            frame_cases
                .iter()
                .map(Vec::as_slice)
                .chain([b"X".as_slice(), b"X1 Q", b"Q X1"])
        {
            let mut encoder = builder().write_header(Vec::new()).unwrap();
            let result = encoder.write_frame_with_params([&[0u8; 4], &[0u8; 1], &[0u8; 1]], params);
            assert!(
                matches!(result, Err(Error::BadInput)),
                "write_frame_with_params({params:?}) must be BadInput"
            );
            let out = encoder.into_inner().unwrap();
            assert_eq!(
                out, b"YUV4MPEG2 W2 H2 F25:1 C420\n",
                "{params:?}: the sink holds only the header after the rejection"
            );
        }
    }

    // A non-letter tag is outside the grammar too.
    for raw in [b"W2 H2 F25:1 9x".as_slice(), b"W2 H2 F25:1 =1"] {
        assert!(matches!(builder().raw_params(raw), Err(Error::BadInput)));
    }

    // The well-formed list still works, and geometry is derived from it.
    let mut out = Vec::new();
    let mut encoder = EncoderBuilder::new(9, 9, Ratio::new(1, 1))
        .raw_params(b"W2 H2 F25:1 Q9 XA=1")
        .unwrap()
        .write_header(&mut out)
        .unwrap();
    encoder
        .write_frame([&[0u8; 4], &[0u8; 1], &[0u8; 1]])
        .unwrap();
    assert_eq!(out, b"YUV4MPEG2 W2 H2 F25:1 Q9 XA=1\nFRAME\n\0\0\0\0\0\0");
}

/// Deterministic xorshift generator for the property tests.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// One byte from the allowed parameter set: anything except SP, LF,
    /// CR, and NUL.
    fn allowed_byte(&mut self) -> u8 {
        loop {
            let b = self.below(256) as u8;
            if !FORBIDDEN.contains(&b) {
                return b;
            }
        }
    }

    fn value(&mut self, max_len: u64) -> Vec<u8> {
        let len = 1 + self.below(max_len);
        (0..len).map(|_| self.allowed_byte()).collect()
    }

    fn ratio(&mut self) -> Ratio {
        Ratio::new(1 + self.below(100_000) as u32, 1 + self.below(1_000) as u32)
    }
}

const INTERLACES: [(Interlace, &[u8]); 5] = [
    (Interlace::Progressive, b"Ip"),
    (Interlace::TopFirst, b"It"),
    (Interlace::BottomFirst, b"Ib"),
    (Interlace::Mixed, b"Im"),
    (Interlace::Unknown, b"I?"),
];

/// Property: every header the writer emits re-parses to the parameter set
/// it was built from. Two generators over the allowed byte set: the
/// canonical builder path (typed fields plus `X` extensions) and the
/// `raw_params` path (a shuffled parameter list with unknown tags).
#[test]
fn emitted_header_reparses() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);

    for case in 0..2000 {
        // Canonical builder path.
        let width = 1 + rng.below(64) as usize;
        let height = 1 + rng.below(64) as usize;
        let framerate = rng.ratio();
        let interlace = match rng.below(6) {
            0 => None,
            i => Some(INTERLACES[i as usize - 1].0),
        };
        let aspect = match rng.below(3) {
            0 => None,
            1 => Some(Ratio::new(0, 0)),
            _ => Some(rng.ratio()),
        };
        let colorspace = Colorspace::ALL[rng.below(Colorspace::ALL.len() as u64) as usize];
        let extensions: Vec<Vec<u8>> = (0..rng.below(4)).map(|_| rng.value(12)).collect();

        let mut builder = EncoderBuilder::new(width, height, framerate).colorspace(colorspace);
        if let Some(i) = interlace {
            builder = builder.interlace(i);
        }
        if let Some(a) = aspect {
            builder = builder.pixel_aspect(a);
        }
        for ext in &extensions {
            builder = builder.extension(ext).unwrap();
        }
        let mut out = Vec::new();
        builder.write_header(&mut out).unwrap();

        let decoder = Decoder::new(Cursor::new(&out))
            .unwrap_or_else(|e| panic!("case {case}: emitted header {out:?} failed to parse: {e}"));
        assert_eq!(decoder.width(), width, "case {case}: width");
        assert_eq!(decoder.height(), height, "case {case}: height");
        assert_eq!(decoder.framerate(), framerate, "case {case}: framerate");
        assert_eq!(
            decoder.pixel_aspect(),
            aspect.unwrap_or(Ratio::new(0, 0)),
            "case {case}: aspect"
        );
        assert_eq!(
            decoder.interlace(),
            interlace.unwrap_or(Interlace::Unknown),
            "case {case}: interlace"
        );
        assert_eq!(decoder.colorspace(), colorspace, "case {case}: colorspace");
        assert_eq!(
            decoder.raw_params(),
            &out[10..out.len() - 1],
            "case {case}: raw parameter bytes"
        );
        let seen: Vec<&[u8]> = decoder
            .raw_params()
            .split(|&b| b == b' ')
            .filter_map(|p| p.strip_prefix(b"X"))
            .collect();
        assert_eq!(seen, extensions, "case {case}: X extensions in order");

        // Feeding the decoded raw parameters back reproduces the header.
        let mut again = Vec::new();
        EncoderBuilder::new(1, 1, Ratio::new(1, 1))
            .raw_params(decoder.raw_params())
            .unwrap()
            .write_header(&mut again)
            .unwrap();
        assert_eq!(again, out, "case {case}: raw re-emit");

        // raw_params path: mandatory W and H, optional F/I/A/C, unknown
        // tags with arbitrary allowed values, in a shuffled order.
        let mut params: Vec<Vec<u8>> = Vec::new();
        let rw = 1 + rng.below(64) as usize;
        let rh = 1 + rng.below(64) as usize;
        params.push(format!("W{rw}").into_bytes());
        params.push(format!("H{rh}").into_bytes());
        let rf = if rng.below(2) == 0 {
            let f = rng.ratio();
            params.push(format!("F{f}").into_bytes());
            f
        } else {
            Ratio::new(25, 1)
        };
        let ri = if rng.below(2) == 0 {
            let (i, tag) = INTERLACES[rng.below(5) as usize];
            params.push(tag.to_vec());
            i
        } else {
            Interlace::Unknown
        };
        let ra = if rng.below(2) == 0 {
            let a = rng.ratio();
            params.push(format!("A{a}").into_bytes());
            a
        } else {
            Ratio::new(0, 0)
        };
        let rc = if rng.below(2) == 0 {
            let cs = Colorspace::ALL[rng.below(Colorspace::ALL.len() as u64) as usize];
            params.push(format!("C{}", cs.tag()).into_bytes());
            cs
        } else {
            Colorspace::C420
        };
        for _ in 0..rng.below(4) {
            let tag = loop {
                let t = b'A' + rng.below(26) as u8;
                if !b"WHFIAC".contains(&t) {
                    break t;
                }
            };
            let tag = if rng.below(2) == 0 {
                tag
            } else {
                tag.to_ascii_lowercase()
            };
            let mut p = vec![tag];
            p.extend(rng.value(16));
            params.push(p);
        }
        for i in (1..params.len()).rev() {
            let j = rng.below(i as u64 + 1) as usize;
            params.swap(i, j);
        }
        let raw = params.join(&b' ');

        let mut out = Vec::new();
        EncoderBuilder::new(1, 1, Ratio::new(1, 1))
            .raw_params(&raw)
            .unwrap_or_else(|e| panic!("case {case}: raw {raw:?} rejected: {e}"))
            .write_header(&mut out)
            .unwrap();
        assert_eq!(out, [b"YUV4MPEG2 ".as_slice(), &raw, b"\n"].concat());
        let decoder = Decoder::new(Cursor::new(&out))
            .unwrap_or_else(|e| panic!("case {case}: raw header {out:?} failed to parse: {e}"));
        assert_eq!(decoder.raw_params(), raw.as_slice(), "case {case}: raw");
        assert_eq!(decoder.width(), rw, "case {case}: raw width");
        assert_eq!(decoder.height(), rh, "case {case}: raw height");
        assert_eq!(decoder.framerate(), rf, "case {case}: raw framerate");
        assert_eq!(decoder.interlace(), ri, "case {case}: raw interlace");
        assert_eq!(decoder.pixel_aspect(), ra, "case {case}: raw aspect");
        assert_eq!(decoder.colorspace(), rc, "case {case}: raw colorspace");
    }
}

/// The decoder applies the same grammar: CR or NUL inside a parameter, a
/// non-letter tag, and a tag with no value are `Parse` errors, so no
/// accepted input carries bytes the writer would refuse to re-emit.
#[test]
fn decoder_rejects_control_bytes_in_params() {
    let cases: &[&[u8]] = &[
        b"YUV4MPEG2 W2 H2 F25:1\r\n",
        b"YUV4MPEG2 W2 H2 F25:1 XA\rB\n",
        b"YUV4MPEG2 W2 H2 F25:1 X\0\n",
        b"YUV4MPEG2 W2 H2 F25:1 Q\0\n",
        b"YUV4MPEG2 W2 H2 F25:1 9x\n",
        b"YUV4MPEG2 W2 H2 F25:1 Q\n",
        b"YUV4MPEG2 W2 H2 F25:1 X\n",
        b"YUV4MPEG2 W2 H2 F25:1 \n",
        b"YUV4MPEG2 \n",
    ];
    for input in cases {
        let result = Decoder::new(Cursor::new(*input));
        assert!(
            matches!(result, Err(Error::Parse(_))),
            "{input:?} must be Parse, got {:?}",
            result.map(|_| ())
        );
    }
    for params in [&b" X1\r"[..], b" X\0", b" 1x", b" X", b" X1 Q", b" X1 X2 "] {
        let mut input = b"YUV4MPEG2 W2 H2 F25:1\nFRAME".to_vec();
        input.extend_from_slice(params);
        input.extend_from_slice(b"\n\0\0\0\0\0\0");
        let mut decoder = Decoder::new(Cursor::new(input)).unwrap();
        assert!(
            matches!(decoder.read_frame(), Err(Error::Parse(_))),
            "frame params {params:?} must be Parse"
        );
    }
}

/// `FRAME \n` (SP, no parameter), `FRAME  Xa\n` (doubled SP), and
/// `FRAME Xa \n` (trailing SP) violate `"FRAME" *( SP parameter ) LF` and
/// return `Parse` from `read_frame`. The frame decoded before the bad
/// header is unaffected, and no accept-and-rewrite happens.
#[test]
fn frame_header_trailing_sp_is_parse_error() {
    let forms: [&[u8]; 3] = [b"FRAME \n", b"FRAME  Xa\n", b"FRAME Xa \n"];
    let frame0: &[u8] = b"\x01\x02\x03\x04\x05\x06";
    for form in forms {
        let mut input = b"YUV4MPEG2 W2 H2 F25:1\nFRAME\n".to_vec();
        input.extend_from_slice(frame0);
        input.extend_from_slice(form);
        input.extend_from_slice(b"\x11\x12\x13\x14\x15\x16");

        let mut decoder = Decoder::new(Cursor::new(&input)).unwrap();
        let first = decoder.read_frame().unwrap();
        let copy = [first.y().to_vec(), first.u().to_vec(), first.v().to_vec()];
        assert_eq!(
            copy,
            [vec![1, 2, 3, 4], vec![5], vec![6]],
            "{form:?}: frame 0"
        );

        let result = decoder.read_frame();
        assert!(
            matches!(result, Err(Error::Parse(_))),
            "{form:?} must be Parse, got {:?}",
            result.map(|_| ())
        );

        // Whole-stream view: the input is not accepted, so nothing is
        // rewritten into a bare header.
        assert!(
            matches!(roundtrip(&input, Limits::default()), Err(Error::Parse(_))),
            "{form:?}: round-trip must fail rather than normalize"
        );
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Property: for every file under `tests/fixtures/` (valid and malformed),
/// every entry of the fuzz corpus when the checkout carries one, and a
/// hand-built edge-case corpus, an input the decoder accepts (every
/// `read_frame` is `Ok` until `EndOfStream`) re-encodes byte-for-byte with
/// raw parameters preserved. Every valid fixture must be accepted.
#[test]
fn accepted_input_roundtrips_byte_exact() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut fixtures = Vec::new();
    walk(&root.join("tests/fixtures"), &mut fixtures);
    assert_eq!(fixtures.len(), 37, "37 conformance fixtures");

    let mut accepted = 0usize;
    let mut rejected = 0usize;
    for path in &fixtures {
        let input = std::fs::read(path).unwrap();
        let is_valid = !path.to_string_lossy().contains("malformed");
        match roundtrip(&input, Limits::default()) {
            Ok((_, out)) => {
                assert_eq!(out, input, "{}: round-trip bytes differ", path.display());
                accepted += 1;
            }
            Err(e) => {
                assert!(!is_valid, "{}: valid fixture rejected: {e}", path.display());
                rejected += 1;
            }
        }
    }
    assert_eq!(accepted, 21, "every valid fixture is accepted");
    assert_eq!(rejected, 16, "every malformed fixture is rejected");

    let corpus_dir = root.join("fuzz/corpus/decode");
    let mut corpus_accepted = 0usize;
    let mut corpus_total = 0usize;
    if corpus_dir.is_dir() {
        let mut entries = Vec::new();
        walk(&corpus_dir, &mut entries);
        for path in &entries {
            let input = std::fs::read(path).unwrap();
            corpus_total += 1;
            if let Ok((_, out)) = roundtrip(&input, FUZZ_LIMITS) {
                assert_eq!(out, input, "{}: round-trip bytes differ", path.display());
                corpus_accepted += 1;
            }
        }
        assert!(corpus_total > 0, "fuzz corpus directory is empty");
    }
    eprintln!(
        "accepted_input_roundtrips_byte_exact: fixtures {accepted} accepted / {rejected} rejected, \
         fuzz corpus {corpus_accepted} accepted of {corpus_total} entries"
    );

    let handmade: &[(&[u8], usize)] = &[
        // Minimal header, no frames.
        (b"YUV4MPEG2 W2 H2\n", 0),
        // Leading zeros and a duplicate tag survive verbatim.
        (b"YUV4MPEG2 W002 H2 W2 F025:001\nFRAME\n\0\0\0\0\0\0", 1),
        // Unknown tag letters, lowercase tag, odd X values.
        (
            b"YUV4MPEG2 W2 H2 F25:1 Q9 z1 Xa=b=c X\x7f\nFRAME\n\0\0\0\0\0\0",
            1,
        ),
        // Non-ASCII bytes inside an X value are carried, not interpreted.
        (b"YUV4MPEG2 W2 H2 F25:1 X\xc3\xa9\nFRAME\n\0\0\0\0\0\0", 1),
        // A0:0 sentinel, I?, mono, several frames with and without params.
        (
            b"YUV4MPEG2 W1 H1 F1:1 I? A0:0 Cmono\nFRAME\n\x10FRAME Ipt\n\x20FRAME Xk=v Z1\n\x30",
            3,
        ),
        // Tabs are not SP, so they sit inside a value.
        (b"YUV4MPEG2 W2 H2 F25:1 X\ta\nFRAME X\t\n\0\0\0\0\0\0", 1),
        // 10-bit payload passes through as wire bytes.
        (
            b"YUV4MPEG2 W1 H1 F25:1 C444p10\nFRAME\n\xff\x03\x00\x01\x01\x02",
            1,
        ),
    ];
    for (input, expected_frames) in handmade {
        let (frames, out) =
            roundtrip(input, Limits::default()).unwrap_or_else(|e| panic!("{input:?}: {e}"));
        assert_eq!(frames, *expected_frames, "{input:?}: frame count");
        assert_eq!(out, *input, "{input:?}: round-trip bytes differ");
    }
}
