//! Streaming encoder over any [`Write`].

use std::io::Write;

use crate::colorspace::Colorspace;
use crate::decode::{
    is_forbidden_param_byte, parse_stream_params, validate_param_list, MAX_HEADER_LINE,
};
use crate::{Error, Interlace, Ratio};

/// Builds a stream header and produces an [`Encoder`].
///
/// Canonical emit order: `W`, `H`, `F`, `I`, `A`, `C`, then `X*` in
/// insertion order. Tags with no value set are omitted. `A0:0` is the one
/// legal sentinel and is emitted when set explicitly.
#[derive(Debug, Clone)]
pub struct EncoderBuilder {
    width: usize,
    height: usize,
    framerate: Ratio,
    colorspace: Colorspace,
    pixel_aspect: Option<Ratio>,
    interlace: Option<Interlace>,
    extensions: Vec<Vec<u8>>,
    raw: Option<Vec<u8>>,
}

impl EncoderBuilder {
    /// Starts a builder with the three required stream parameters.
    pub fn new(width: usize, height: usize, framerate: Ratio) -> Self {
        EncoderBuilder {
            width,
            height,
            framerate,
            colorspace: Colorspace::C420,
            pixel_aspect: None,
            interlace: None,
            extensions: Vec::new(),
            raw: None,
        }
    }

    /// Sets the colorspace tag. Default: plain `C420`.
    pub fn colorspace(mut self, cs: Colorspace) -> Self {
        self.colorspace = cs;
        self
    }

    /// Sets the pixel aspect ratio, including the `0:0` unknown sentinel.
    /// Default: the `A` tag is omitted.
    pub fn pixel_aspect(mut self, aspect: Ratio) -> Self {
        self.pixel_aspect = Some(aspect);
        self
    }

    /// Sets the interlacing mode. Default: the `I` tag is omitted.
    /// [`Interlace::Unknown`] is treated as no value and omitted.
    pub fn interlace(mut self, interlace: Interlace) -> Self {
        self.interlace = Some(interlace);
        self
    }

    /// Appends an `X` vendor extension (value bytes after the `X`).
    /// Repeatable, with insertion order preserved on the wire. The value must
    /// be non-empty and free of SP, LF, CR, and NUL bytes, else
    /// [`Error::BadInput`].
    pub fn extension(mut self, value: &[u8]) -> Result<Self, Error> {
        if value.is_empty()
            || value
                .iter()
                .any(|&b| b == b' ' || is_forbidden_param_byte(b))
        {
            return Err(Error::BadInput);
        }
        self.extensions.push(value.to_vec());
        Ok(self)
    }

    /// Byte-exact re-encode path: emit `raw` verbatim as the header
    /// parameter bytes (everything after `"YUV4MPEG2 "`, LF excluded)
    /// instead of building canonical tags.
    ///
    /// `raw` must be a well-formed single-line parameter list: SP-separated
    /// parameters, each a single ASCII letter tag followed by a value of one
    /// byte or more, with no empty parameter (no leading, trailing, or
    /// doubled SP) and no LF, CR, or NUL byte. A violation is
    /// [`Error::BadInput`], returned before anything is written, so the
    /// header can never span two lines. The list is then parsed with the
    /// decoder's grammar, so a missing `W` or `H`, a zero dimension, or an
    /// unknown `C` value returns the decoder's error for it. Frame geometry
    /// for plane validation is derived from the list, overriding the
    /// builder's width, height, and colorspace. Pass
    /// [`Decoder::raw_params`](crate::Decoder::raw_params) to reproduce a
    /// decoded stream byte-for-byte.
    pub fn raw_params(mut self, raw: &[u8]) -> Result<Self, Error> {
        validate_param_list(raw).map_err(|_| Error::BadInput)?;
        let params = parse_stream_params(raw)?;
        self.width = params.width;
        self.height = params.height;
        self.colorspace = params.colorspace;
        self.raw = Some(raw.to_vec());
        Ok(self)
    }

    /// Writes the stream header and returns the frame encoder.
    ///
    /// Zero dimensions or a zero framerate denominator are
    /// [`Error::BadInput`]. Plane-size overflow and an over-budget header
    /// line are [`Error::Limits`]. The high-bit-depth scratch buffer is
    /// allocated here, once.
    pub fn write_header<W: Write>(self, mut writer: W) -> Result<Encoder<W>, Error> {
        if self.width == 0 || self.height == 0 || self.framerate.den == 0 {
            return Err(Error::BadInput);
        }
        let plane_sizes = self.colorspace.plane_sizes(self.width, self.height)?;

        let mut header: Vec<u8> = Vec::with_capacity(64);
        header.extend_from_slice(b"YUV4MPEG2 ");
        match &self.raw {
            Some(raw) => header.extend_from_slice(raw),
            None => {
                write!(
                    header,
                    "W{} H{} F{}:{}",
                    self.width, self.height, self.framerate.num, self.framerate.den
                )
                .map_err(Error::Io)?;
                let interlace_char = match self.interlace {
                    Some(Interlace::Progressive) => Some('p'),
                    Some(Interlace::TopFirst) => Some('t'),
                    Some(Interlace::BottomFirst) => Some('b'),
                    Some(Interlace::Mixed) => Some('m'),
                    _ => None,
                };
                if let Some(c) = interlace_char {
                    write!(header, " I{c}").map_err(Error::Io)?;
                }
                if let Some(aspect) = self.pixel_aspect {
                    write!(header, " A{}:{}", aspect.num, aspect.den).map_err(Error::Io)?;
                }
                write!(header, " C{}", self.colorspace.tag()).map_err(Error::Io)?;
                for ext in &self.extensions {
                    header.extend_from_slice(b" X");
                    header.extend_from_slice(ext);
                }
            }
        }
        header.push(b'\n');
        if header.len() > MAX_HEADER_LINE {
            return Err(Error::Limits);
        }
        writer.write_all(&header).map_err(Error::from)?;

        let scratch = if self.colorspace.bytes_per_sample() == 2 {
            vec![0u8; plane_sizes[0]]
        } else {
            Vec::new()
        };

        Ok(Encoder {
            writer,
            plane_sizes,
            bit_depth: self.colorspace.bit_depth(),
            scratch,
        })
    }
}

/// Writes frames for one stream. Produced by [`EncoderBuilder::write_header`].
///
/// The hot path is `write_all` calls only. The optional `u16`-to-LE scratch
/// buffer is allocated once at header time, and the counting-allocator test
/// `tests/zero_alloc.rs` pins frame writes at zero heap allocations.
pub struct Encoder<W: Write> {
    writer: W,
    plane_sizes: [usize; 3],
    bit_depth: u8,
    scratch: Vec<u8>,
}

impl<W: Write> Encoder<W> {
    /// Writes one frame with a bare `FRAME` header from wire-format byte
    /// planes. Plane byte lengths must match the header-derived sizes, else
    /// [`Error::BadInput`].
    pub fn write_frame(&mut self, planes: [&[u8]; 3]) -> Result<(), Error> {
        self.write_frame_with_params(planes, b"")
    }

    /// Writes one frame carrying verbatim frame header parameters (the
    /// bytes after `"FRAME "`, LF excluded, or empty for a bare header).
    ///
    /// Parameters must follow the frame header grammar: SP-separated,
    /// each a single ASCII letter tag plus a value of one byte or more, no
    /// empty piece, and no LF, CR, or NUL byte, else [`Error::BadInput`]
    /// before anything is written. An over-budget frame header line is
    /// [`Error::Limits`].
    pub fn write_frame_with_params(
        &mut self,
        planes: [&[u8]; 3],
        params: &[u8],
    ) -> Result<(), Error> {
        for (plane, &size) in planes.iter().zip(self.plane_sizes.iter()) {
            if plane.len() != size {
                return Err(Error::BadInput);
            }
        }
        self.write_frame_header(params)?;
        for plane in planes {
            if !plane.is_empty() {
                self.writer.write_all(plane).map_err(Error::from)?;
            }
        }
        Ok(())
    }

    /// Writes one frame from host-order `u16` sample planes, encoding each
    /// sample little-endian on the wire regardless of host endianness.
    ///
    /// Requires a >8-bit colorspace. Plane sample counts must match the
    /// header-derived sizes and every sample must fit the stream bit depth,
    /// else [`Error::BadInput`].
    pub fn write_frame_u16(&mut self, planes: [&[u16]; 3]) -> Result<(), Error> {
        if self.scratch.is_empty() {
            return Err(Error::BadInput);
        }
        let max = if self.bit_depth >= 16 {
            u16::MAX
        } else {
            (1u16 << self.bit_depth) - 1
        };
        for (plane, &size) in planes.iter().zip(self.plane_sizes.iter()) {
            if plane.len() != size / 2 {
                return Err(Error::BadInput);
            }
            if plane.iter().any(|&s| s > max) {
                return Err(Error::BadInput);
            }
        }
        self.write_frame_header(b"")?;
        for plane in planes {
            if plane.is_empty() {
                continue;
            }
            let out = &mut self.scratch[..plane.len() * 2];
            for (sample, wire) in plane.iter().zip(out.chunks_exact_mut(2)) {
                wire.copy_from_slice(&sample.to_le_bytes());
            }
            self.writer.write_all(out).map_err(Error::from)?;
        }
        Ok(())
    }

    fn write_frame_header(&mut self, params: &[u8]) -> Result<(), Error> {
        if !params.is_empty() {
            validate_param_list(params).map_err(|_| Error::BadInput)?;
            if 5 + 1 + params.len() + 1 > MAX_HEADER_LINE {
                return Err(Error::Limits);
            }
        }
        self.writer.write_all(b"FRAME").map_err(Error::from)?;
        if !params.is_empty() {
            self.writer.write_all(b" ").map_err(Error::from)?;
            self.writer.write_all(params).map_err(Error::from)?;
        }
        self.writer.write_all(b"\n").map_err(Error::from)?;
        Ok(())
    }

    /// Flushes and returns the underlying writer.
    pub fn into_inner(mut self) -> Result<W, Error> {
        self.writer.flush().map_err(Error::from)?;
        Ok(self.writer)
    }
}

impl<W: Write> std::fmt::Debug for Encoder<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Encoder")
            .field("plane_sizes", &self.plane_sizes)
            .field("bit_depth", &self.bit_depth)
            .finish_non_exhaustive()
    }
}
