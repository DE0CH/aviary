//! Images (magpie 5.5, 10.2): the sniffed type, dimensions from the header bytes (no decode), EXIF orientation, and
//! the three derived copies: the model copy made at upload, and the lazily made thumbnail and display copy.
//!
//! Every function here is CPU-bound and synchronous; callers run them on the blocking pool under
//! [`super::Blobs`]'s decode permit (at most [`limits::DECODES`] at once per replica).

use std::borrow::Cow;
use std::io::Cursor;

use fast_image_resize as fr;
use image::{DynamicImage, ImageDecoder as _, ImageFormat, RgbImage, RgbaImage};

use crate::limits;

/// Why an image could not be read or made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImageError {
    /// Not an image type magpie reads (or a damaged one).
    #[error("not a supported image: {0}")]
    Unsupported(String),
    /// More pixels than [`limits::DECODE_MAX_PIXELS`].
    #[error("image of {0} pixels is over the decode limit")]
    TooLarge(u64),
}

/// The image types (by sniffed type, then extension; magpie 10.2). SVG is a file.
pub const IMAGE_MIMES: [&str; 9] = [
    "image/jpeg",
    "image/png",
    "image/gif",
    "image/webp",
    "image/heic",
    "image/heif",
    "image/avif",
    "image/tiff",
    "image/bmp",
];

/// The types the model takes as they are.
pub const MODEL_NATIVE: [&str; 4] = ["image/jpeg", "image/png", "image/gif", "image/webp"];

/// Whether `mime` is an image type magpie reads.
#[must_use]
pub fn is_image(mime: &str) -> bool {
    IMAGE_MIMES.contains(&mime)
}

/// The type of `bytes` by their magic numbers: an image type of [`IMAGE_MIMES`], `application/pdf`, or `None`.
#[must_use]
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    let b = bytes;
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if b.starts_with(b"II*\0") || b.starts_with(b"MM\0*") {
        return Some("image/tiff");
    }
    if b.starts_with(b"BM") && b.len() >= 26 {
        return Some("image/bmp");
    }
    if b.starts_with(b"%PDF-") {
        return Some("application/pdf");
    }
    if b.len() >= 12 && &b[4..8] == b"ftyp" {
        // the major brand, then the compatible brands of the ftyp box
        let size = usize::try_from(u32::from_be_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0);
        let end = size.clamp(12, b.len().min(64));
        let mut brands = vec![&b[8..12]];
        let mut i = 16;
        while i + 4 <= end {
            brands.push(&b[i..i + 4]);
            i += 4;
        }
        if brands.iter().any(|x| *x == b"avif" || *x == b"avis") {
            return Some("image/avif");
        }
        if brands
            .iter()
            .any(|x| matches!(*x, b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis"))
        {
            return Some("image/heic");
        }
        if brands.iter().any(|x| matches!(*x, b"mif1" | b"msf1")) {
            return Some("image/heif");
        }
    }
    None
}

fn heif_family(mime: &str) -> bool {
    matches!(mime, "image/heic" | "image/heif" | "image/avif")
}

fn format_of(mime: &str) -> Option<ImageFormat> {
    Some(match mime {
        "image/jpeg" => ImageFormat::Jpeg,
        "image/png" => ImageFormat::Png,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::WebP,
        "image/tiff" => ImageFormat::Tiff,
        "image/bmp" => ImageFormat::Bmp,
        _ => return None,
    })
}

/// Width and height from the header bytes, without decoding the pixels.
///
/// # Errors
/// [`ImageError::Unsupported`] when the header does not parse.
pub fn dimensions(bytes: &[u8], mime: &str) -> Result<(u32, u32), ImageError> {
    if heif_family(mime) {
        let ctx = libheif_rs::HeifContext::read_from_bytes(bytes)
            .map_err(|e| ImageError::Unsupported(format!("heif: {e}")))?;
        let h = ctx
            .primary_image_handle()
            .map_err(|e| ImageError::Unsupported(format!("heif: {e}")))?;
        return Ok((h.width(), h.height()));
    }
    let format = format_of(mime).ok_or_else(|| ImageError::Unsupported(mime.to_owned()))?;
    image::ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|e| ImageError::Unsupported(e.to_string()))
}

/// The EXIF orientation (1-8; 1 when there is none). HEIF's own rotation is applied by its decoder, so its EXIF
/// value is not read (applying both would turn the picture twice).
#[must_use]
pub fn orientation(bytes: &[u8], mime: &str) -> u32 {
    if heif_family(mime) || mime == "image/gif" || mime == "image/bmp" {
        return 1;
    }
    let Ok(exif) = exif::Reader::new().read_from_container(&mut Cursor::new(bytes)) else {
        return 1;
    };
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .filter(|o| (1..=8).contains(o))
        .unwrap_or(1)
}

fn pixels(w: u32, h: u32) -> u64 {
    u64::from(w) * u64::from(h)
}

/// The memory a decode of `bytes` holds at its peak, estimated from the header (for the decode memory budget,
/// [`limits::DECODE_MEMORY_BYTES`]): two full frames at the decoded pixel size (the decoded image and the one
/// turned upright or the decoder's own planes) and one 8-bit RGBA frame (the conversion of a 16-bit or grey image
/// before resizing). An image over [`limits::DECODE_MAX_PIXELS`] or one whose header does not parse costs only its
/// bytes: it is refused before anything is decoded.
#[must_use]
pub fn decode_memory(bytes: &[u8], mime: &str) -> u64 {
    let small = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let Ok((w, h)) = dimensions(bytes, mime) else {
        return small;
    };
    let px = pixels(w, h);
    if px > limits::DECODE_MAX_PIXELS {
        return small;
    }
    let per_pixel: u64 = if heif_family(mime) {
        4
    } else {
        format_of(mime)
            .and_then(|f| {
                image::ImageReader::with_format(Cursor::new(bytes), f)
                    .into_decoder()
                    .ok()
            })
            .map_or(8, |d| u64::from(d.color_type().bytes_per_pixel()))
    };
    px.saturating_mul(per_pixel.saturating_mul(2) + 4).saturating_add(small)
}

/// Decodes `bytes` (at most [`limits::DECODE_MAX_PIXELS`]), turned upright.
///
/// # Errors
/// [`ImageError::TooLarge`]; [`ImageError::Unsupported`].
pub fn decode(bytes: &[u8], mime: &str) -> Result<DynamicImage, ImageError> {
    let (w, h) = dimensions(bytes, mime)?;
    if pixels(w, h) > limits::DECODE_MAX_PIXELS {
        return Err(ImageError::TooLarge(pixels(w, h)));
    }
    let img = if heif_family(mime) {
        decode_heif(bytes)?
    } else {
        let format = format_of(mime).ok_or_else(|| ImageError::Unsupported(mime.to_owned()))?;
        let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
        let mut limits_ = image::Limits::default();
        limits_.max_image_width = Some(w);
        limits_.max_image_height = Some(h);
        reader.limits(limits_);
        reader.decode().map_err(|e| ImageError::Unsupported(e.to_string()))?
    };
    Ok(orient(img, orientation(bytes, mime)))
}

fn decode_heif(bytes: &[u8]) -> Result<DynamicImage, ImageError> {
    use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};
    let err = |e: libheif_rs::HeifError| ImageError::Unsupported(format!("heif: {e}"));
    let lib = LibHeif::new();
    let ctx = HeifContext::read_from_bytes(bytes).map_err(err)?;
    let handle = ctx.primary_image_handle().map_err(err)?;
    let alpha = handle.has_alpha_channel();
    let chroma = if alpha { RgbChroma::Rgba } else { RgbChroma::Rgb };
    let img = lib.decode(&handle, ColorSpace::Rgb(chroma), None).map_err(err)?;
    let (w, h) = (img.width(), img.height());
    let planes = img.planes();
    let plane = planes
        .interleaved
        .ok_or_else(|| ImageError::Unsupported("heif: no interleaved plane".into()))?;
    let channels: usize = if alpha { 4 } else { 3 };
    let row = usize::try_from(w).unwrap_or(0) * channels;
    let mut out = Vec::with_capacity(row * usize::try_from(h).unwrap_or(0));
    for y in 0..usize::try_from(h).unwrap_or(0) {
        let start = y * plane.stride;
        out.extend_from_slice(
            plane
                .data
                .get(start..start + row)
                .ok_or_else(|| ImageError::Unsupported("heif: plane shorter than its size".into()))?,
        );
    }
    let bad = || ImageError::Unsupported("heif: buffer size".into());
    Ok(if alpha {
        DynamicImage::ImageRgba8(RgbaImage::from_raw(w, h, out).ok_or_else(bad)?)
    } else {
        DynamicImage::ImageRgb8(RgbImage::from_raw(w, h, out).ok_or_else(bad)?)
    })
}

/// `img` turned by EXIF orientation `o`: flips and half turns in place, quarter turns into one new frame (the old
/// one is freed as soon as it is made).
#[must_use]
pub fn orient(mut img: DynamicImage, o: u32) -> DynamicImage {
    if let Some(o) = u8::try_from(o).ok().and_then(image::metadata::Orientation::from_exif) {
        img.apply_orientation(o);
    }
    img
}

/// The size fitting `(w, h)` into a long edge of `edge` (unchanged when it already fits).
#[must_use]
pub fn fitted(w: u32, h: u32, edge: u32) -> (u32, u32) {
    let long = w.max(h);
    if long <= edge || long == 0 {
        return (w, h);
    }
    let scale = |x: u32| {
        let v = (u64::from(x) * u64::from(edge) + u64::from(long) / 2) / u64::from(long);
        u32::try_from(v.max(1)).unwrap_or(1)
    };
    (scale(w), scale(h))
}

fn has_alpha(img: &DynamicImage) -> bool {
    img.color().has_alpha()
}

/// Whether any pixel of `img` is not opaque (read in place for 8-bit images).
#[must_use]
pub fn is_transparent(img: &DynamicImage) -> bool {
    match img {
        DynamicImage::ImageRgba8(b) => b.as_raw().as_chunks::<4>().0.iter().any(|p| p[3] < 255),
        DynamicImage::ImageLumaA8(b) => b.as_raw().as_chunks::<2>().0.iter().any(|p| p[1] < 255),
        _ => has_alpha(img) && img.to_rgba8().pixels().any(|p| p.0[3] < 255),
    }
}

/// `img` as 8-bit RGB or RGBA: itself when it is one already.
fn eight_bit(img: &DynamicImage) -> Cow<'_, DynamicImage> {
    match img {
        DynamicImage::ImageRgb8(_) | DynamicImage::ImageRgba8(_) => Cow::Borrowed(img),
        _ if has_alpha(img) => Cow::Owned(DynamicImage::ImageRgba8(img.to_rgba8())),
        _ => Cow::Owned(DynamicImage::ImageRgb8(img.to_rgb8())),
    }
}

/// `img` as 8-bit RGB or RGBA fitted into a long edge of `edge` with Lanczos3 (`img` itself when it is 8-bit and
/// fits already: no copy).
///
/// # Errors
/// [`ImageError::Unsupported`] when the resizer refuses.
pub fn fit(img: &DynamicImage, edge: u32) -> Result<Cow<'_, DynamicImage>, ImageError> {
    let src = eight_bit(img);
    let (w, h) = fitted(src.width(), src.height(), edge);
    if (w, h) == (src.width(), src.height()) {
        return Ok(src);
    }
    let mut dst = DynamicImage::new(w, h, src.color());
    let opts = fr::ResizeOptions::new().resize_alg(fr::ResizeAlg::Convolution(fr::FilterType::Lanczos3));
    fr::Resizer::new()
        .resize(src.as_ref(), &mut dst, &opts)
        .map_err(|e| ImageError::Unsupported(format!("resize: {e}")))?;
    Ok(Cow::Owned(dst))
}

/// `img` on white, without alpha (itself when it is 8-bit RGB).
#[must_use]
pub fn on_white(img: &DynamicImage) -> Cow<'_, RgbImage> {
    if let DynamicImage::ImageRgb8(b) = img {
        return Cow::Borrowed(b);
    }
    if !has_alpha(img) {
        return Cow::Owned(img.to_rgb8());
    }
    let rgba = img.to_rgba8();
    let mut out = RgbImage::new(rgba.width(), rgba.height());
    for (o, p) in out.pixels_mut().zip(rgba.pixels()) {
        let a = u16::from(p.0[3]);
        for c in 0..3 {
            let v = (u16::from(p.0[c]) * a + 255 * (255 - a) + 127) / 255;
            o.0[c] = u8::try_from(v).unwrap_or(255);
        }
    }
    Cow::Owned(out)
}

fn encode(img: &DynamicImage, format: ImageFormat) -> Result<Vec<u8>, ImageError> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, format)
        .map_err(|e| ImageError::Unsupported(format!("encode: {e}")))?;
    Ok(out.into_inner())
}

fn jpeg(img: &RgbImage, quality: u8) -> Result<Vec<u8>, ImageError> {
    let mut out = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
    img.write_with_encoder(enc)
        .map_err(|e| ImageError::Unsupported(format!("jpeg: {e}")))?;
    Ok(out)
}

/// A copy made for the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copy {
    /// The bytes.
    pub bytes: Vec<u8>,
    /// `image/png` or `image/jpeg`.
    pub mime: &'static str,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

/// The model copy of an uploaded image (magpie 10.2): `None` when the original is used as it is (a model-native type,
/// long edge <= 2,000 px, <= 3,932,160 bytes, no EXIF rotation); otherwise decoded, turned upright, fitted to
/// 2,000 px, PNG for a transparent PNG/WebP when it fits, else JPEG at quality 85, 70, then 55 on white (and, should
/// even that not fit, at half the size again, until it does).
///
/// # Errors
/// [`ImageError`] from the decode.
pub fn model_copy(bytes: &[u8], mime: &str) -> Result<Option<Copy>, ImageError> {
    let (w, h) = dimensions(bytes, mime)?;
    let cap = limits::MODEL_IMAGE_MAX_BYTES;
    if MODEL_NATIVE.contains(&mime)
        && w.max(h) <= limits::MODEL_IMAGE_EDGE
        && bytes.len() <= cap
        && orientation(bytes, mime) == 1
    {
        return Ok(None);
    }
    let img = decode(bytes, mime)?;
    let mut edge = limits::MODEL_IMAGE_EDGE;
    loop {
        let fit_ = fit(&img, edge)?;
        if matches!(mime, "image/png" | "image/webp") && is_transparent(&fit_) {
            let png = encode(&fit_, ImageFormat::Png)?;
            if png.len() <= cap {
                return Ok(Some(Copy {
                    bytes: png,
                    mime: "image/png",
                    width: fit_.width(),
                    height: fit_.height(),
                }));
            }
        }
        let white = on_white(&fit_);
        let white = white.as_ref();
        for q in limits::MODEL_JPEG_QUALITIES {
            let j = jpeg(white, q)?;
            if j.len() <= cap {
                return Ok(Some(Copy {
                    bytes: j,
                    mime: "image/jpeg",
                    width: white.width(),
                    height: white.height(),
                }));
            }
        }
        if edge <= 64 {
            return Err(ImageError::Unsupported("no model copy fits".into()));
        }
        edge /= 2;
    }
}

/// A lossy WebP (quality 80) of `bytes` fitted into a long edge of `edge` (a thumbnail or a display copy).
///
/// # Errors
/// [`ImageError`] from the decode.
pub fn webp_fitted(bytes: &[u8], mime: &str, edge: u32) -> Result<Vec<u8>, ImageError> {
    let decoded = decode(bytes, mime)?;
    let img = fit(&decoded, edge)?;
    let enc = webp::Encoder::from_image(&img).map_err(|e| ImageError::Unsupported(format!("webp: {e}")))?;
    Ok(enc.encode(limits::WEBP_QUALITY).to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_the_long_edge() {
        assert_eq!(fitted(4032, 3024, 2000), (2000, 1500));
        assert_eq!(fitted(3024, 4032, 512), (384, 512));
        assert_eq!(fitted(100, 50, 512), (100, 50));
        assert_eq!(fitted(10_000, 1, 512), (512, 1));
    }

    #[test]
    fn sniffs() {
        assert_eq!(sniff(b"\xFF\xD8\xFF\xE0rest"), Some("image/jpeg"));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(b"\0\0\0\x18ftypheic\0\0\0\0mif1heic"), Some("image/heic"));
        assert_eq!(sniff(b"\0\0\0\x1cftypavif\0\0\0\0avifmif1miaf"), Some("image/avif"));
        assert_eq!(sniff(b"%PDF-1.7"), Some("application/pdf"));
        assert_eq!(sniff(b"<svg xmlns"), None);
    }
}
