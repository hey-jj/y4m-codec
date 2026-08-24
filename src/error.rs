//! Error type for stream parsing, limits, and writer validation.

use std::fmt;
use std::io;

/// Errors reported by the decoder and encoder.
///
/// Every malformed, truncated, or over-budget input maps to a typed variant,
/// so callers can branch on truncation versus clean end of stream.
#[non_exhaustive]
#[derive(Debug)]
pub enum Error {
    /// Clean end of stream: EOF exactly where a `FRAME` header would start,
    /// or EOF while reading the stream header before any frame was decoded.
    EndOfStream,
    /// EOF inside a frame header or frame payload: the stream was cut off
    /// mid-frame.
    Truncated,
    /// A header byte sequence violates the YUV4MPEG2 grammar. The payload
    /// names the offending location or rule.
    Parse(&'static str),
    /// A `C` (colorspace) tag with a value this crate does not recognize.
    /// The payload holds the tag value as read from the wire.
    Unsupported(String),
    /// A header line or frame byte budget was exceeded, or plane size
    /// arithmetic overflowed.
    Limits,
    /// An I/O error other than `UnexpectedEof` from the underlying reader or
    /// writer.
    Io(io::Error),
    /// Writer input validation failure: plane length mismatch, invalid
    /// extension bytes, or an out-of-range sample.
    BadInput,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::EndOfStream => write!(f, "end of stream"),
            Error::Truncated => write!(f, "unexpected EOF inside a frame"),
            Error::Parse(detail) => write!(f, "parse error: {detail}"),
            Error::Unsupported(tag) => write!(f, "unsupported colorspace tag: C{tag}"),
            Error::Limits => write!(f, "header or frame size limit exceeded"),
            Error::Io(e) => write!(f, "i/o error: {e}"),
            Error::BadInput => write!(f, "writer input does not match the stream header"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    /// `UnexpectedEof` maps to [`Error::Truncated`]. The decoder reports
    /// [`Error::EndOfStream`] itself when EOF falls on a frame boundary.
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            Error::Truncated
        } else {
            Error::Io(e)
        }
    }
}
