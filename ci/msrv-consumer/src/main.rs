//! Minimal external consumer: encodes a two-frame 10-bit stream and decodes
//! it back through the public API. Built and run on the MSRV toolchain.

use std::io::Cursor;

use y4m_codec::{Colorspace, Decoder, EncoderBuilder, Error, Interlace, Ratio};

fn main() -> Result<(), Error> {
    let mut bytes = Vec::new();
    let mut encoder = EncoderBuilder::new(2, 2, Ratio::new(25, 1))
        .interlace(Interlace::Progressive)
        .colorspace(Colorspace::C420P10)
        .extension(b"YSCSS=420P10")?
        .write_header(&mut bytes)?;
    let y = [0x0001u16, 0x0201, 0x03FF, 0x0100];
    encoder.write_frame_u16([&y, &[0x0080], &[0x0080]])?;
    encoder.write_frame_u16([&y, &[0x0081], &[0x0081]])?;

    let mut decoder = Decoder::new(Cursor::new(&bytes))?;
    assert_eq!(decoder.colorspace().tag(), "420p10");
    assert_eq!(decoder.plane_sizes(), [8, 2, 2]);
    let mut frames = 0;
    loop {
        match decoder.read_frame() {
            Ok(frame) => {
                assert_eq!(frame.plane_u16(0).unwrap(), &y);
                assert_eq!(frame.y()[..2], [0x01, 0x00], "little-endian wire bytes");
                frames += 1;
            }
            Err(Error::EndOfStream) => break,
            Err(e) => return Err(e),
        }
    }
    assert_eq!(frames, 2);
    println!("msrv-consumer: {} bytes, {frames} frames round-tripped", bytes.len());
    Ok(())
}
