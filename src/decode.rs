//! Streaming decoder over any [`Read`].

use std::io::{BufReader, Read};

use crate::colorspace::Colorspace;
use crate::{Error, Interlace, Ratio};

/// Whole header line budget in bytes, including magic and the terminating LF.
/// Applies to the stream header and to every frame header.
pub(crate) const MAX_HEADER_LINE: usize = 4096;

const STREAM_MAGIC: &[u8; 10] = b"YUV4MPEG2 ";
const FRAME_MAGIC: &[u8; 5] = b"FRAME";

/// Resource budgets enforced by the decoder before allocating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Upper bound on the decoder's total peak frame-buffer memory in
    /// bytes: the wire-byte buffer holding one frame's plane data plus,
    /// for streams deeper than 8 bits, the equally sized host-order `u16`
    /// sample buffer. Exceeding it is [`Error::Limits`], checked before
    /// either buffer is allocated. Default: 1 GiB.
    pub max_frame_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_frame_bytes: 1 << 30,
        }
    }
}

/// Stream header fields parsed from raw parameter bytes.
pub(crate) struct StreamParams {
    pub width: usize,
    pub height: usize,
    /// `None` when the `F` tag is absent. Readers default to 25:1 without
    /// re-emitting the tag.
    pub framerate: Option<Ratio>,
    /// `Ratio { num: 0, den: 0 }` means unknown.
    pub pixel_aspect: Ratio,
    pub interlace: Interlace,
    pub colorspace: Colorspace,
}

/// Parses a decimal unsigned integer: ASCII digits only, no sign, leading
/// zeros tolerated. A value that does not fit `u64` is a parse error.
fn parse_uint(bytes: &[u8]) -> Result<u64, Error> {
    if bytes.is_empty() {
        return Err(Error::Parse("empty integer value"));
    }
    let mut value: u64 = 0;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return Err(Error::Parse("non-digit in integer value"));
        }
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add(u64::from(b - b'0')))
            .ok_or(Error::Parse("integer value too large"))?;
    }
    Ok(value)
}

/// Parses `num:den` into a [`Ratio`]. `zero_den_ok` admits the `A0:0`
/// unknown-aspect sentinel. Otherwise `den` must be positive.
fn parse_ratio(bytes: &[u8], zero_den_ok: bool) -> Result<Ratio, Error> {
    let colon = bytes
        .iter()
        .position(|&b| b == b':')
        .ok_or(Error::Parse("ratio value missing ':'"))?;
    let num = parse_uint(&bytes[..colon])?;
    let den = parse_uint(&bytes[colon + 1..])?;
    let num = u32::try_from(num).map_err(|_| Error::Parse("ratio numerator too large"))?;
    let den = u32::try_from(den).map_err(|_| Error::Parse("ratio denominator too large"))?;
    if den == 0 && !(zero_den_ok && num == 0) {
        return Err(Error::Parse("ratio denominator is zero"));
    }
    Ok(Ratio { num, den })
}

/// Bytes that can never appear inside a header parameter: the line
/// terminator, CR (an illegal byte per the format contract), and NUL.
/// SP is excluded separately since it is the parameter separator.
pub(crate) fn is_forbidden_param_byte(b: u8) -> bool {
    matches!(b, b'\n' | b'\r' | 0)
}

/// Checks a SP-separated parameter list (stream header bytes after the
/// magic, or frame header bytes after `"FRAME "`) against the grammar
/// `parameter := tag-char value`, SP-separated, with at least one parameter:
///
/// - no empty parameter (no leading, trailing, or doubled SP)
/// - the tag is a single ASCII letter
/// - the value is at least one byte, so a parameter is two bytes or more
/// - no LF, CR, or NUL byte anywhere.
///
/// Both the decoder and the writer apply this grammar, so every accepted
/// input is re-encodable and every writer-supplied parameter list is a
/// well-formed single line.
pub(crate) fn validate_param_list(raw: &[u8]) -> Result<(), Error> {
    if raw.is_empty() {
        return Err(Error::Parse("empty header parameter"));
    }
    for param in raw.split(|&b| b == b' ') {
        let (tag, value) = match param.split_first() {
            Some(pair) => pair,
            None => return Err(Error::Parse("empty header parameter")),
        };
        if !tag.is_ascii_alphabetic() {
            return Err(Error::Parse("parameter tag is not an ASCII letter"));
        }
        if value.is_empty() {
            return Err(Error::Parse("parameter has no value"));
        }
        if value.iter().copied().any(is_forbidden_param_byte) {
            return Err(Error::Parse("LF, CR, or NUL byte inside a parameter"));
        }
    }
    Ok(())
}

/// Parses the stream header parameter bytes (everything after `"YUV4MPEG2 "`,
/// LF excluded). Duplicate tags: last one wins. Unknown tag letters are
/// tolerated and survive round-trips through the raw parameter bytes.
pub(crate) fn parse_stream_params(raw: &[u8]) -> Result<StreamParams, Error> {
    validate_param_list(raw)?;

    let mut width: Option<usize> = None;
    let mut height: Option<usize> = None;
    let mut framerate: Option<Ratio> = None;
    let mut pixel_aspect = Ratio { num: 0, den: 0 };
    let mut interlace = Interlace::Unknown;
    let mut colorspace: Option<Colorspace> = None;

    for param in raw.split(|&b| b == b' ') {
        let (tag, value) = match param.split_first() {
            Some(pair) => pair,
            None => return Err(Error::Parse("empty header parameter")),
        };
        match tag {
            b'W' => {
                let w = parse_uint(value)?;
                if w == 0 {
                    return Err(Error::Parse("width must be positive"));
                }
                width = Some(usize::try_from(w).map_err(|_| Error::Limits)?);
            }
            b'H' => {
                let h = parse_uint(value)?;
                if h == 0 {
                    return Err(Error::Parse("height must be positive"));
                }
                height = Some(usize::try_from(h).map_err(|_| Error::Limits)?);
            }
            b'F' => framerate = Some(parse_ratio(value, false)?),
            b'A' => pixel_aspect = parse_ratio(value, true)?,
            b'I' => {
                interlace = match value {
                    b"p" => Interlace::Progressive,
                    b"t" => Interlace::TopFirst,
                    b"b" => Interlace::BottomFirst,
                    b"m" => Interlace::Mixed,
                    b"?" => Interlace::Unknown,
                    _ => return Err(Error::Parse("invalid interlace value")),
                };
            }
            b'C' => colorspace = Some(Colorspace::from_tag_bytes(value)?),
            _ => {
                // `X` vendor extensions are carried, not interpreted. Unknown
                // tag letters are tolerated for forward compatibility. Both
                // survive verbatim in the raw parameter bytes.
            }
        }
    }

    Ok(StreamParams {
        width: width.ok_or(Error::Parse("missing W tag"))?,
        height: height.ok_or(Error::Parse("missing H tag"))?,
        framerate,
        pixel_aspect,
        interlace,
        colorspace: colorspace.unwrap_or(Colorspace::C420),
    })
}

fn read_byte<R: Read>(r: &mut R) -> Result<Option<u8>, Error> {
    let mut byte = [0u8; 1];
    loop {
        match r.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => return Ok(Some(byte[0])),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::Io(e)),
        }
    }
}

/// A streaming YUV4MPEG2 decoder.
///
/// Owns one reusable frame buffer sized at header parse time.
/// [`Decoder::read_frame`] fills it and returns borrowed plane slices.
pub struct Decoder<R: Read> {
    inner: BufReader<R>,
    params: StreamParams,
    raw_params: Vec<u8>,
    plane_sizes: [usize; 3],
    frame_total: usize,
    /// Wire bytes of the current frame's planes.
    buf: Vec<u8>,
    /// Host-order samples of the current frame, empty for 8-bit streams.
    samples: Vec<u16>,
    /// Current frame header bytes after `FRAME`, LF excluded, leading SP
    /// included. Capacity is reserved once and bounded by the line budget.
    frame_params: Vec<u8>,
}

impl<R: Read> Decoder<R> {
    /// Reads and parses the stream header with the default [`Limits`].
    pub fn new(reader: R) -> Result<Self, Error> {
        Decoder::with_limits(reader, Limits::default())
    }

    /// Reads and parses the stream header, enforcing `limits` before the
    /// frame buffer is allocated.
    ///
    /// EOF while reading the stream header is [`Error::EndOfStream`]. A
    /// wrong magic or malformed parameter is [`Error::Parse`].
    pub fn with_limits(reader: R, limits: Limits) -> Result<Self, Error> {
        let mut inner = BufReader::new(reader);

        let mut magic = [0u8; 10];
        let mut filled = 0;
        while filled < magic.len() {
            match read_byte(&mut inner)? {
                Some(b) => {
                    magic[filled] = b;
                    filled += 1;
                }
                None => return Err(Error::EndOfStream),
            }
        }
        if &magic != STREAM_MAGIC {
            return Err(Error::Parse("bad stream magic"));
        }

        let mut raw_params = Vec::with_capacity(64);
        loop {
            match read_byte(&mut inner)? {
                Some(b'\n') => break,
                Some(b) => {
                    // magic (10) + params + LF (1) must fit the line budget.
                    if STREAM_MAGIC.len() + raw_params.len() + 1 >= MAX_HEADER_LINE {
                        return Err(Error::Limits);
                    }
                    raw_params.push(b);
                }
                None => return Err(Error::EndOfStream),
            }
        }

        let params = parse_stream_params(&raw_params)?;
        let plane_sizes = params.colorspace.plane_sizes(params.width, params.height)?;
        let frame_total = plane_sizes
            .iter()
            .try_fold(0usize, |acc, &s| acc.checked_add(s))
            .ok_or(Error::Limits)?;
        // Peak frame memory: the wire-byte buffer plus, for >8-bit
        // streams, the u16 sample buffer of the same byte size. The
        // budget bounds that total, checked before either allocation.
        let peak_bytes = if params.colorspace.bytes_per_sample() == 2 {
            frame_total.checked_mul(2).ok_or(Error::Limits)?
        } else {
            frame_total
        };
        if peak_bytes > limits.max_frame_bytes {
            return Err(Error::Limits);
        }

        let samples = if params.colorspace.bytes_per_sample() == 2 {
            vec![0u16; frame_total / 2]
        } else {
            Vec::new()
        };

        Ok(Decoder {
            inner,
            params,
            raw_params,
            plane_sizes,
            frame_total,
            buf: vec![0u8; frame_total],
            samples,
            frame_params: Vec::with_capacity(MAX_HEADER_LINE),
        })
    }

    /// Frame width in luma samples.
    pub fn width(&self) -> usize {
        self.params.width
    }

    /// Frame height in luma samples.
    pub fn height(&self) -> usize {
        self.params.height
    }

    /// Frame rate. Defaults to 25:1 when the `F` tag is absent. The default
    /// stays out of [`Decoder::raw_params`].
    pub fn framerate(&self) -> Ratio {
        self.params.framerate.unwrap_or(Ratio { num: 25, den: 1 })
    }

    /// Pixel (sample) aspect ratio, where `0:0` means unknown.
    pub fn pixel_aspect(&self) -> Ratio {
        self.params.pixel_aspect
    }

    /// Interlacing mode, [`Interlace::Unknown`] when the `I` tag is absent.
    pub fn interlace(&self) -> Interlace {
        self.params.interlace
    }

    /// The stream colorspace, plain 4:2:0 8-bit when the `C` tag is absent.
    pub fn colorspace(&self) -> Colorspace {
        self.params.colorspace
    }

    /// Stream header parameter bytes after the `"YUV4MPEG2 "` prefix,
    /// verbatim, LF excluded. Feed to
    /// [`EncoderBuilder::raw_params`](crate::EncoderBuilder::raw_params) for
    /// a byte-exact re-encode.
    pub fn raw_params(&self) -> &[u8] {
        &self.raw_params
    }

    /// Per-plane byte sizes `[Y, U, V]`. Chroma entries are 0 for mono.
    pub fn plane_sizes(&self) -> [usize; 3] {
        self.plane_sizes
    }

    /// Borrows the next frame. Planes point into the decoder's reusable
    /// internal buffer, so each call reuses the same storage. The
    /// counting-allocator test `tests/zero_alloc.rs` pins the decode loop
    /// at zero heap allocations.
    ///
    /// Returns [`Error::EndOfStream`] at a clean end (EOF exactly where the
    /// next `FRAME` magic would start) and [`Error::Truncated`] when EOF
    /// falls inside a frame header or payload.
    pub fn read_frame(&mut self) -> Result<FrameRef<'_>, Error> {
        // Frame magic, detecting clean EOF on the first byte only.
        match read_byte(&mut self.inner)? {
            None => return Err(Error::EndOfStream),
            Some(b) if b == FRAME_MAGIC[0] => {}
            Some(_) => return Err(Error::Parse("bad frame magic")),
        }
        for &expected in &FRAME_MAGIC[1..] {
            match read_byte(&mut self.inner)? {
                None => return Err(Error::Truncated),
                Some(b) if b == expected => {}
                Some(_) => return Err(Error::Parse("bad frame magic")),
            }
        }

        self.frame_params.clear();
        loop {
            match read_byte(&mut self.inner)? {
                Some(b'\n') => break,
                Some(b) => {
                    if FRAME_MAGIC.len() + self.frame_params.len() + 1 >= MAX_HEADER_LINE {
                        return Err(Error::Limits);
                    }
                    self.frame_params.push(b);
                }
                None => return Err(Error::Truncated),
            }
        }
        // Grammar: "FRAME" *( SP parameter ) LF. A SP must be followed by a
        // parameter, so "FRAME \n" is malformed rather than a bare header.
        if !self.frame_params.is_empty() {
            if self.frame_params[0] != b' ' {
                return Err(Error::Parse("frame parameters must follow a space"));
            }
            validate_param_list(&self.frame_params[1..])?;
        }

        // Payload: `From<io::Error>` maps UnexpectedEof to Truncated.
        self.inner.read_exact(&mut self.buf)?;

        if !self.samples.is_empty() {
            for (sample, wire) in self.samples.iter_mut().zip(self.buf.chunks_exact(2)) {
                *sample = u16::from_le_bytes([wire[0], wire[1]]);
            }
        }

        let (y, rest) = self.buf.split_at(self.plane_sizes[0]);
        let (u, v) = rest.split_at(self.plane_sizes[1]);
        let (planes16, hi_depth) = if self.samples.is_empty() {
            ([&[] as &[u16]; 3], false)
        } else {
            let (y16, rest16) = self.samples.split_at(self.plane_sizes[0] / 2);
            let (u16p, v16) = rest16.split_at(self.plane_sizes[1] / 2);
            ([y16, u16p, v16], true)
        };
        let raw_params = if self.frame_params.is_empty() {
            &[]
        } else {
            &self.frame_params[1..]
        };

        Ok(FrameRef {
            planes: [y, u, v],
            planes16,
            hi_depth,
            raw_params,
        })
    }

    /// The frame byte count implied by the header (sum of plane sizes).
    pub fn frame_bytes(&self) -> usize {
        self.frame_total
    }
}

impl<R: Read> std::fmt::Debug for Decoder<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoder")
            .field("width", &self.params.width)
            .field("height", &self.params.height)
            .field("colorspace", &self.params.colorspace)
            .finish_non_exhaustive()
    }
}

/// A borrowed view of one decoded frame.
///
/// Lives as long as the borrow of the decoder that produced it, so one
/// frame is live at a time. Copy explicitly to retain a frame.
#[derive(Debug, Clone, Copy)]
pub struct FrameRef<'a> {
    planes: [&'a [u8]; 3],
    planes16: [&'a [u16]; 3],
    hi_depth: bool,
    raw_params: &'a [u8],
}

impl<'a> FrameRef<'a> {
    /// Plane `i` as wire-format bytes (Y=0, U=1, V=2, empty chroma for
    /// mono). For >8-bit streams these are the little-endian wire bytes.
    ///
    /// # Panics
    ///
    /// Panics if `i > 2`.
    pub fn plane(&self, i: usize) -> &'a [u8] {
        self.planes[i]
    }

    /// The Y (luma) plane.
    pub fn y(&self) -> &'a [u8] {
        self.planes[0]
    }

    /// The U (Cb) plane, empty for mono streams.
    pub fn u(&self) -> &'a [u8] {
        self.planes[1]
    }

    /// The V (Cr) plane, empty for mono streams.
    pub fn v(&self) -> &'a [u8] {
        self.planes[2]
    }

    /// Plane `i` as host-order `u16` samples for >8-bit streams, or `None`
    /// when the bit depth is 8 or less.
    ///
    /// # Panics
    ///
    /// Panics if `i > 2`.
    pub fn plane_u16(&self, i: usize) -> Option<&'a [u16]> {
        if self.hi_depth {
            Some(self.planes16[i])
        } else {
            None
        }
    }

    /// Frame header parameter bytes verbatim (the bytes after `"FRAME "`,
    /// LF excluded), empty when the frame header was bare. Feed to
    /// [`Encoder::write_frame_with_params`](crate::Encoder::write_frame_with_params)
    /// for a byte-exact re-encode.
    pub fn raw_params(&self) -> &'a [u8] {
        self.raw_params
    }
}
