# Changelog

## 0.1.0 - 2026-08-21

First release: a streaming YUV4MPEG2 (.y4m) reader and writer with
borrowed frame planes, defined little-endian high-bit-depth wire order,
and byte-exact re-encodes.

### Added

- `Decoder<R: Read>` with `read_frame` returning a borrowed `FrameRef`
  backed by one internal frame buffer, held at zero per-frame heap
  allocations by the counting-allocator test `tests/zero_alloc.rs`.
- `FrameRef::plane_u16`: host-order `u16` view of >8-bit planes.
- `EncoderBuilder` / `Encoder<W: Write>` with canonical header emission
  (`W H F I A C X*`), a verbatim `raw_params` path for byte-exact
  re-encodes, and `write_frame_u16` with little-endian output on every
  host.
- Colorspace tags: the mjpegtools core set plus ffmpeg depth extensions
  (`420p9`–`420p16`, `422p9`–`422p16`, `444p9`–`444p16`,
  `mono9`–`mono16`).
- Typed `Error` enum separating clean end of stream (`EndOfStream`) from
  mid-frame truncation (`Truncated`), with `Parse`, `Unsupported`,
  `Limits`, `Io`, and `BadInput` variants.
- `Limits` frame byte budget (default 1 GiB) checked before allocation,
  a 4096-byte header line bound, and checked plane-size arithmetic.
- Test suite: byte-exact round-trip of all 21 valid fixtures with
  sha256 pins, error-class replay of all 16 malformed fixtures, the
  `be_wire_bytes` endianness pin, and counting-allocator tests holding the
  decode and encode loops at zero allocations.
- MSRV 1.70.0, checked by building and running an external consumer crate
  on that toolchain. No runtime dependencies, and `#![forbid(unsafe_code)]`
  at the crate root.
