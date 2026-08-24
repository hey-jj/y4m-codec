//! Colorspace tags, subsampling families, and plane geometry.

use crate::Error;

/// Chroma subsampling family of a colorspace tag.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Subsampling {
    /// 4:2:0: chroma planes are `ceil(W/2) x ceil(H/2)` samples.
    C420,
    /// 4:2:2: chroma planes are `ceil(W/2) x H` samples.
    C422,
    /// 4:4:4: chroma planes are `W x H` samples.
    C444,
    /// Grayscale: no chroma planes.
    Mono,
}

/// Chroma sample siting carried by the 8-bit 4:2:0 tag variants.
///
/// Siting is positioning metadata only. Plane sizes and byte layout are
/// identical across all 4:2:0 variants.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChromaSiting {
    /// No siting stated (`C420`, all `p<depth>` tags, and non-4:2:0 tags).
    Unspecified,
    /// Centered siting (`C420jpeg`).
    Jpeg,
    /// Left-cosited siting (`C420mpeg2`).
    Mpeg2,
    /// PAL-DV siting (`C420paldv`).
    PalDv,
}

/// A recognized colorspace: subsampling family, chroma siting, and bit depth.
///
/// Values exist only for the tags in the YUV4MPEG2 contract (mjpegtools core
/// plus the ffmpeg high-bit-depth extension). Parsing an unknown `C` value
/// returns [`Error::Unsupported`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Colorspace {
    sub: Subsampling,
    siting: ChromaSiting,
    depth: u8,
    tag: &'static str,
}

macro_rules! colorspaces {
    ($($name:ident => $tag:literal, $sub:ident, $sit:ident, $depth:literal;)*) => {
        impl Colorspace {
            $(
                #[doc = concat!("The `C", $tag, "` colorspace tag.")]
                pub const $name: Colorspace = Colorspace {
                    sub: Subsampling::$sub,
                    siting: ChromaSiting::$sit,
                    depth: $depth,
                    tag: $tag,
                };
            )*

            /// Every recognized colorspace, in tag-table order.
            pub const ALL: &'static [Colorspace] = &[$(Colorspace::$name),*];
        }
    };
}

colorspaces! {
    C420JPEG => "420jpeg", C420, Jpeg, 8;
    C420MPEG2 => "420mpeg2", C420, Mpeg2, 8;
    C420PALDV => "420paldv", C420, PalDv, 8;
    C420 => "420", C420, Unspecified, 8;
    C420P9 => "420p9", C420, Unspecified, 9;
    C420P10 => "420p10", C420, Unspecified, 10;
    C420P12 => "420p12", C420, Unspecified, 12;
    C420P14 => "420p14", C420, Unspecified, 14;
    C420P16 => "420p16", C420, Unspecified, 16;
    C422 => "422", C422, Unspecified, 8;
    C422P9 => "422p9", C422, Unspecified, 9;
    C422P10 => "422p10", C422, Unspecified, 10;
    C422P12 => "422p12", C422, Unspecified, 12;
    C422P14 => "422p14", C422, Unspecified, 14;
    C422P16 => "422p16", C422, Unspecified, 16;
    C444 => "444", C444, Unspecified, 8;
    C444P9 => "444p9", C444, Unspecified, 9;
    C444P10 => "444p10", C444, Unspecified, 10;
    C444P12 => "444p12", C444, Unspecified, 12;
    C444P14 => "444p14", C444, Unspecified, 14;
    C444P16 => "444p16", C444, Unspecified, 16;
    MONO => "mono", Mono, Unspecified, 8;
    MONO9 => "mono9", Mono, Unspecified, 9;
    MONO10 => "mono10", Mono, Unspecified, 10;
    MONO12 => "mono12", Mono, Unspecified, 12;
    MONO16 => "mono16", Mono, Unspecified, 16;
}

impl Colorspace {
    /// Looks up a colorspace by its wire tag value (the bytes after `C`).
    ///
    /// Returns [`Error::Unsupported`] for any value outside the recognized
    /// set, including `444alpha`.
    pub fn from_tag(tag: &str) -> Result<Colorspace, Error> {
        Colorspace::ALL
            .iter()
            .find(|cs| cs.tag == tag)
            .copied()
            .ok_or_else(|| Error::Unsupported(tag.to_owned()))
    }

    pub(crate) fn from_tag_bytes(tag: &[u8]) -> Result<Colorspace, Error> {
        match std::str::from_utf8(tag) {
            Ok(s) => Colorspace::from_tag(s),
            Err(_) => Err(Error::Unsupported(
                String::from_utf8_lossy(tag).into_owned(),
            )),
        }
    }

    /// The chroma subsampling family.
    pub fn subsampling(self) -> Subsampling {
        self.sub
    }

    /// The chroma sample siting carried by the tag.
    pub fn chroma_siting(self) -> ChromaSiting {
        self.siting
    }

    /// Bits per sample, `8..=16`.
    pub fn bit_depth(self) -> u8 {
        self.depth
    }

    /// Bytes per sample on the wire: 1 for depth <= 8, 2 otherwise.
    pub fn bytes_per_sample(self) -> usize {
        if self.depth <= 8 {
            1
        } else {
            2
        }
    }

    /// The wire tag value without the leading `C`, e.g. `"420p10"`.
    pub fn tag(self) -> &'static str {
        self.tag
    }

    /// Per-plane sample dimensions `(width, height)` for a `width x height`
    /// frame. Chroma planes of mono streams are `(0, 0)`.
    pub(crate) fn plane_dims(self, width: usize, height: usize) -> [(usize, usize); 3] {
        // ceil(x / 2) written overflow-free: `x + 1` would wrap for
        // `usize::MAX`, which a hostile header can produce on 64-bit hosts.
        let half_w = width / 2 + width % 2;
        let half_h = height / 2 + height % 2;
        let (cw, ch) = match self.sub {
            Subsampling::C420 => (half_w, half_h),
            Subsampling::C422 => (half_w, height),
            Subsampling::C444 => (width, height),
            Subsampling::Mono => (0, 0),
        };
        [(width, height), (cw, ch), (cw, ch)]
    }

    /// Per-plane byte sizes for a `width x height` frame, using checked
    /// arithmetic. Overflow of any product or of the total is
    /// [`Error::Limits`].
    pub(crate) fn plane_sizes(self, width: usize, height: usize) -> Result<[usize; 3], Error> {
        let bps = self.bytes_per_sample();
        let dims = self.plane_dims(width, height);
        let mut sizes = [0usize; 3];
        let mut total = 0usize;
        for (i, (w, h)) in dims.iter().enumerate() {
            let size = w
                .checked_mul(*h)
                .and_then(|n| n.checked_mul(bps))
                .ok_or(Error::Limits)?;
            total = total.checked_add(size).ok_or(Error::Limits)?;
            sizes[i] = size;
        }
        Ok(sizes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_lookup_round_trips() {
        for cs in Colorspace::ALL {
            let back = Colorspace::from_tag(cs.tag()).unwrap();
            assert_eq!(back, *cs);
        }
    }

    #[test]
    fn alpha_is_unsupported() {
        assert!(matches!(
            Colorspace::from_tag("444alpha"),
            Err(Error::Unsupported(t)) if t == "444alpha"
        ));
    }

    #[test]
    fn odd_dims_round_up() {
        assert_eq!(
            Colorspace::C420.plane_sizes(5, 5).unwrap(),
            [25, 9, 9],
            "5x5 4:2:0 chroma is ceil(5/2)^2 = 9"
        );
        assert_eq!(Colorspace::C422P10.plane_sizes(3, 2).unwrap(), [12, 8, 8]);
        assert_eq!(Colorspace::MONO.plane_sizes(5, 3).unwrap(), [15, 0, 0]);
    }
}
