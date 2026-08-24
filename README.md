# y4m-codec

Streaming reader and writer for the YUV4MPEG2 (`.y4m`) uncompressed planar
video format, over any `std::io::Read` / `std::io::Write`.

- Zero runtime dependencies, `#![forbid(unsafe_code)]`, MSRV 1.70.
- Borrowed-buffer decode: `read_frame` returns plane slices into one
  reusable buffer, and after construction the decode and encode loops run
  without allocating. The counting-allocator test `tests/zero_alloc.rs`
  holds both loops at zero heap allocations.
- Defined wire endianness: samples deeper than 8 bits are little-endian on
  the wire on every host. `plane_u16` returns host-order samples, and
  `write_frame_u16` converts host-order samples to little-endian bytes.
- Byte-exact round-trips: raw header and frame parameter bytes are kept
  verbatim, so decode followed by re-encode reproduces the input exactly.
  The test suite replays 21 valid and 16 malformed YUV4MPEG2 fixtures,
  including two streams emitted by ffmpeg 8.1.2.
- Malformed or truncated input maps to a typed `Error`: checked size
  arithmetic, a 4096-byte header line bound, and a configurable frame byte
  budget (default 1 GiB).

## Format coverage

Colorspace tags: `420jpeg`, `420mpeg2`, `420paldv`, `420`, `422`, `444`,
`mono`, and the ffmpeg depth extensions `p9`–`p16` / `mono9`–`mono16`.
Odd dimensions round chroma up (`ceil(W/2)`), matching ffmpeg and
mjpegtools. Unknown header tags are tolerated and preserved. An unknown
`C` value is a typed `Unsupported` error, and `C444alpha` is out of scope.

## Reading

```rust
use y4m_codec::{Decoder, Error};

let file = std::fs::File::open("input.y4m")?;
let mut decoder = Decoder::new(file)?;
println!("{}x{} {}", decoder.width(), decoder.height(), decoder.framerate());

loop {
    match decoder.read_frame() {
        Ok(frame) => process(frame.y(), frame.u(), frame.v()),
        Err(Error::EndOfStream) => break,
        Err(e) => return Err(e.into()),
    }
}
```

For streams deeper than 8 bits, `frame.plane_u16(i)` yields the same plane
as host-order `u16` samples.

## Writing

```rust
use y4m_codec::{Colorspace, EncoderBuilder, Ratio};

let out = std::fs::File::create("out.y4m")?;
let mut encoder = EncoderBuilder::new(1920, 1080, Ratio::new(30, 1))
    .colorspace(Colorspace::C420P10)
    .write_header(out)?;
encoder.write_frame_u16([y_samples, u_samples, v_samples])?;
```

`write_frame` takes wire-format byte planes. `write_frame_u16` takes
host-order samples and handles the little-endian conversion. Plane lengths
are validated against the header, and a mismatch is `Error::BadInput`.

## Byte-exact re-encode

`Decoder::raw_params` and `FrameRef::raw_params` expose the header bytes
verbatim. Feeding them back reproduces the input byte-for-byte:

```rust
let mut encoder = EncoderBuilder::new(dec.width(), dec.height(), dec.framerate())
    .raw_params(dec.raw_params())?
    .write_header(&mut out)?;
// per frame:
encoder.write_frame_with_params([f.y(), f.u(), f.v()], f.raw_params())?;
```

## Relation to the `y4m` crate

This crate covers the same role with two behavioral changes. The 16-bit
write path has defined endianness: the incumbent's `Frame::from_u16` emits
host byte order, so its output comes out byte-swapped on big-endian
machines. And the error surface splits truncation from clean EOF: mid-frame
truncation reports `Truncated`, while a stream ending exactly at a frame
boundary reports `EndOfStream`. It also adds the `p9`/`p14`/`p16` and
`mono9`/`mono10`/`mono16` tags and drops per-frame heap allocation.

## License

MIT. Copyright (c) 2026 Justin Jones.
