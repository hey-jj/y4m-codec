//! Gate (d): decoder fuzz target over whole streams (header and frame
//! mutation). Any accepted input must also satisfy the byte-exact
//! round-trip contract: a stream that decodes to a clean end of stream
//! re-encodes, with preserved raw parameters, to the input bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;
use y4m_codec::{Decoder, EncoderBuilder, Error, Limits};

fuzz_target!(|data: &[u8]| {
    // Small frame budget so hostile W/H values never allocate much.
    let limits = Limits {
        max_frame_bytes: 1 << 16,
    };
    let mut decoder = match Decoder::with_limits(data, limits) {
        Ok(d) => d,
        Err(_) => return,
    };
    let mut out = Vec::with_capacity(data.len());
    let mut encoder = EncoderBuilder::new(decoder.width(), decoder.height(), decoder.framerate())
        .raw_params(decoder.raw_params())
        .expect("accepted stream header must be re-encodable")
        .write_header(&mut out)
        .expect("accepted stream header must be writable");
    loop {
        match decoder.read_frame() {
            Ok(frame) => {
                for i in 0..3 {
                    if let Some(p) = frame.plane_u16(i) {
                        assert_eq!(p.len() * 2, frame.plane(i).len());
                    }
                }
                encoder
                    .write_frame_with_params([frame.y(), frame.u(), frame.v()], frame.raw_params())
                    .expect("accepted frame must be re-encodable");
            }
            Err(Error::EndOfStream) => {
                assert_eq!(out, data, "fully accepted input must round-trip byte-exact");
                break;
            }
            Err(_) => break,
        }
    }
});
