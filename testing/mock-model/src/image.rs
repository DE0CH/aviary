//! Image dimensions from the header bytes (JPEG, PNG, GIF, WebP), enough to assert the size of an image Claude
//! Code sent (platform 7.8 E5) without decoding it.

fn be16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?])))
}

fn le16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?])))
}

fn le24(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(*b.get(at)?) | (u32::from(*b.get(at + 1)?) << 8) | (u32::from(*b.get(at + 2)?) << 16))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *b.get(at)?,
        *b.get(at + 1)?,
        *b.get(at + 2)?,
        *b.get(at + 3)?,
    ]))
}

fn jpeg(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let marker = b[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        if matches!(marker, 0xD8 | 0x01 | 0xD0..=0xD7) {
            i += 2;
            continue;
        }
        let len = usize::try_from(be16(b, i + 2)?).ok()?;
        // SOF0-SOF15 except DHT (C4), JPG (C8), DAC (CC)
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            return Some((be16(b, i + 7)?, be16(b, i + 5)?));
        }
        i += 2 + len;
    }
    None
}

fn webp(b: &[u8]) -> Option<(u32, u32)> {
    match b.get(12..16)? {
        b"VP8 " => Some((le16(b, 26)? & 0x3FFF, le16(b, 28)? & 0x3FFF)),
        b"VP8L" => {
            let bits = u32::from_le_bytes([*b.get(21)?, *b.get(22)?, *b.get(23)?, *b.get(24)?]);
            Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
        }
        b"VP8X" => Some((le24(b, 24)? + 1, le24(b, 27)? + 1)),
        _ => None,
    }
}

/// `(width, height)` of a JPEG, PNG, GIF or WebP image, from its header.
#[must_use]
pub fn dimensions(b: &[u8]) -> Option<(u32, u32)> {
    if b.starts_with(&[0xFF, 0xD8]) {
        jpeg(b)
    } else if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some((be32(b, 16)?, be32(b, 20)?))
    } else if b.starts_with(b"GIF8") {
        Some((le16(b, 6)?, le16(b, 8)?))
    } else if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        webp(b)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::dimensions;

    #[test]
    fn png_gif_jpeg_webp_headers() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&6000u32.to_be_bytes());
        png.extend_from_slice(&4000u32.to_be_bytes());
        assert_eq!(dimensions(&png), Some((6000, 4000)));
        let gif = b"GIF89a\x40\x01\xf0\x00";
        assert_eq!(dimensions(gif), Some((320, 240)));
        // SOI, an APP0 segment, then SOF0 with height 1333 and width 2000
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0x05, 0x35, 0x07, 0xD0, 0x03]);
        assert_eq!(dimensions(&jpeg), Some((2000, 1333)));
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\0\0\0\0".to_vec();
        webp.extend_from_slice(&[0x7F, 0x07, 0x00, 0x34, 0x05, 0x00]);
        assert_eq!(dimensions(&webp), Some((1920, 1333)));
        assert_eq!(dimensions(b"not an image"), None);
    }
}
