//! Streaming reader and writer for the YUV4MPEG2 (`.y4m`) uncompressed
//! planar video format.
//!
//! The decoder works over any [`std::io::Read`] and the encoder over any
//! [`std::io::Write`]. Design points:
//!
//! - **Borrowed frames.** [`Decoder::read_frame`] returns a [`FrameRef`]
//!   borrowing one reusable internal buffer, so the decode and encode hot
//!   paths reuse the same storage for every frame. The counting-allocator
//!   test `tests/zero_alloc.rs` pins both loops at zero heap allocations.
//! - **Defined endianness.** Samples deeper than 8 bits are little-endian on
//!   the wire on every host. [`FrameRef::plane_u16`] yields host-order
//!   samples, and [`Encoder::write_frame_u16`] converts host-order samples to
//!   little-endian wire bytes.
//! - **Byte-exact round-trips.** Raw header and frame parameter bytes are
//!   preserved verbatim, so decode followed by re-encode reproduces the
//!   input byte-for-byte.
//! - **Typed errors.** Malformed, truncated, or over-budget input yields a
//!   matching [`Error`] variant, and all size arithmetic is checked.
//!
//! # Reading
//!
//! ```
//! use y4m_codec::{Decoder, Error};
//!
//! let data: &[u8] = b"YUV4MPEG2 W2 H2 F25:1 C420\nFRAME\n\x00\x01\x02\x03\x80\x80";
//! let mut decoder = Decoder::new(data)?;
//! assert_eq!((decoder.width(), decoder.height()), (2, 2));
//! let frame = decoder.read_frame()?;
//! assert_eq!(frame.y(), &[0x00, 0x01, 0x02, 0x03]);
//! assert!(matches!(decoder.read_frame(), Err(Error::EndOfStream)));
//! # Ok::<(), y4m_codec::Error>(())
//! ```
//!
//! # Writing
//!
//! ```
//! use y4m_codec::{EncoderBuilder, Ratio};
//!
//! let mut out = Vec::new();
//! let mut encoder = EncoderBuilder::new(2, 2, Ratio::new(25, 1)).write_header(&mut out)?;
//! encoder.write_frame([&[0, 1, 2, 3], &[0x80], &[0x80]])?;
//! assert!(out.starts_with(b"YUV4MPEG2 W2 H2 F25:1 C420\nFRAME\n"));
//! # Ok::<(), y4m_codec::Error>(())
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod colorspace;
mod decode;
mod encode;
mod error;

pub use colorspace::{ChromaSiting, Colorspace, Subsampling};
pub use decode::{Decoder, FrameRef, Limits};
pub use encode::{Encoder, EncoderBuilder};
pub use error::Error;

/// An exact rational value, used for the `F` (framerate) and `A` (pixel
/// aspect) header tags. `0:0` is the unknown-aspect sentinel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ratio {
    /// Numerator.
    pub num: u32,
    /// Denominator.
    pub den: u32,
}

impl Ratio {
    /// Builds a ratio from numerator and denominator.
    pub const fn new(num: u32, den: u32) -> Self {
        Ratio { num, den }
    }
}

impl std::fmt::Display for Ratio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.num, self.den)
    }
}

/// Interlacing mode from the stream header `I` tag.
///
/// Metadata only: plane layout is unaffected, and an absent tag reads as
/// [`Interlace::Unknown`], treated as progressive.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Interlace {
    /// `Ip`: progressive.
    Progressive,
    /// `It`: top field first.
    TopFirst,
    /// `Ib`: bottom field first.
    BottomFirst,
    /// `Im`: mixed, with per-frame `I` parameters describing each frame.
    Mixed,
    /// `I?` or tag absent: unknown.
    Unknown,
}
