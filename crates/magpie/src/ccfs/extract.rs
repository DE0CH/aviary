//! The extraction rules (magpie 6.4): which strings of a JSONL line become blobs, and the line's stub.
//!
//! 1. Media: content of at least 1,024 bytes, only base64 alphabet characters, whose first bytes decode to a PNG,
//!    JPEG, GIF, WebP or PDF signature: kind `media`, encoding `base64-json`, mime from the signature, dimensions
//!    from the header bytes (no decode).
//! 2. Text: content of at least 16,384 bytes that is not an assistant line's `message.content[*].text` or
//!    `.thinking`: kind `text`, encoding `json-string` (the escaped bytes), with a preview and a line count.
//!
//! The stub is the line with each extracted string's content removed (`"data":"iVBORw0…"` becomes `"data":""`);
//! `spans` records where (an offset in the stub) and which blob. Reconstruction ([`splice`]) puts the blob's bytes
//! back at each offset, so the output equals the input byte for byte. A line that is not valid JSON is stored whole.

use std::ops::Range;

use base64::Engine as _;
use sha2::{Digest, Sha256};

use super::scan::{Seg, scan};

/// Media: at least this many content bytes.
pub const MEDIA_MIN: usize = 1024;
/// Text: at least this many content bytes.
pub const TEXT_MIN: usize = 16 * 1024;
/// Text preview: the first this many characters (of the unescaped first [`PREVIEW_SCAN`] bytes).
pub const PREVIEW_CHARS: usize = 4000;
/// Text preview: how much of the escaped content is unescaped for it.
pub const PREVIEW_SCAN: usize = 16 * 1024;
/// How much base64 is decoded to find an image's dimensions (a JPEG's SOF marker can follow large EXIF data).
const DIMENSIONS_SCAN: usize = 256 * 1024;

/// A SHA-256.
pub type Sha = [u8; 32];

/// The SHA-256 of `b`.
#[must_use]
pub fn sha(b: &[u8]) -> Sha {
    Sha256::digest(b).into()
}

/// Lowercase hex.
#[must_use]
pub fn hex(sha: &[u8]) -> String {
    use std::fmt::Write as _;
    sha.iter().fold(String::with_capacity(sha.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// 64 lowercase hex characters to a sha.
#[must_use]
pub fn unhex(s: &str) -> Option<Sha> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// What a blob is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// base64 media (`base64-json`).
    Media,
    /// Long text (`json-string`).
    Text,
}

impl Kind {
    /// `blobs.kind`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Media => "media",
            Kind::Text => "text",
        }
    }

    /// `blobs.encoding`.
    #[must_use]
    pub fn encoding(self) -> &'static str {
        match self {
            Kind::Media => "base64-json",
            Kind::Text => "json-string",
        }
    }
}

/// A string the rules extract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob {
    /// The SHA-256 of the stored bytes (the content, escaped, between the quotes).
    pub sha: Sha,
    /// Where those bytes are in the line.
    pub range: Range<usize>,
    /// Media or text.
    pub kind: Kind,
    /// Media: from the signature.
    pub mime: Option<&'static str>,
    /// Media: from the header bytes, when they say.
    pub width: Option<u32>,
    /// See `width`.
    pub height: Option<u32>,
}

/// A line split into its stub and blobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    /// The line with each blob's content removed.
    pub stub: Vec<u8>,
    /// `(offset in the stub, blob sha)`, ascending.
    pub spans: Vec<(usize, Sha)>,
    /// The blobs, in line order (a sha may repeat).
    pub blobs: Vec<Blob>,
    /// The SHA-256 of the whole line.
    pub sha: Sha,
    /// The line parsed as JSON.
    pub json: bool,
}

/// Applies the rules to one complete line (its final `\n` included).
#[must_use]
pub fn extract(line: &[u8]) -> Extracted {
    struct Candidate {
        range: Range<usize>,
        prose: bool,
    }
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut type_range: Option<Range<usize>> = None;
    let json = scan(line, |path, r| {
        if path.len() == 1 && path[0] == Seg::Key(b"type") {
            type_range = Some(r.clone());
        }
        if r.len() >= MEDIA_MIN {
            let prose = matches!(
                path,
                [
                    Seg::Key(b"message"),
                    Seg::Key(b"content"),
                    Seg::Index(_),
                    Seg::Key(b"text" | b"thinking")
                ]
            );
            candidates.push(Candidate { range: r, prose });
        }
    });
    let whole = sha(line);
    if !json {
        return Extracted {
            stub: line.to_vec(),
            spans: Vec::new(),
            blobs: Vec::new(),
            sha: whole,
            json: false,
        };
    }
    let assistant = type_range.is_some_and(|r| &line[r] == b"assistant");
    let mut blobs = Vec::new();
    for c in candidates {
        let content = &line[c.range.clone()];
        if let Some((mime, dims)) = media(content) {
            blobs.push(Blob {
                sha: sha(content),
                range: c.range,
                kind: Kind::Media,
                mime: Some(mime),
                width: dims.map(|d| d.0),
                height: dims.map(|d| d.1),
            });
        } else if content.len() >= TEXT_MIN && !(assistant && c.prose) {
            blobs.push(Blob {
                sha: sha(content),
                range: c.range,
                kind: Kind::Text,
                mime: None,
                width: None,
                height: None,
            });
        }
    }
    let removed: usize = blobs.iter().map(|b| b.range.len()).sum();
    let mut stub = Vec::with_capacity(line.len() - removed);
    let mut spans = Vec::with_capacity(blobs.len());
    let mut at = 0;
    for b in &blobs {
        stub.extend_from_slice(&line[at..b.range.start]);
        spans.push((stub.len(), b.sha));
        at = b.range.end;
    }
    stub.extend_from_slice(&line[at..]);
    Extracted {
        stub,
        spans,
        blobs,
        sha: whole,
        json: true,
    }
}

/// Puts each span's blob bytes back into `stub`: the original line. `blob(sha)` gives the stored bytes.
///
/// # Errors
/// When `blob` has no bytes for a sha, or a span is out of order or past the stub.
pub fn splice<'b>(
    stub: &[u8],
    spans: &[(usize, Sha)],
    mut blob: impl FnMut(&Sha) -> Option<&'b [u8]>,
) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(stub.len());
    let mut at = 0;
    for (off, sha) in spans {
        if *off < at || *off > stub.len() {
            return Err(format!("span at {off} out of order (after {at}, stub {})", stub.len()));
        }
        out.extend_from_slice(&stub[at..*off]);
        out.extend_from_slice(blob(sha).ok_or_else(|| format!("no bytes for blob {}", hex(sha)))?);
        at = *off;
    }
    out.extend_from_slice(&stub[at..]);
    Ok(out)
}

fn is_base64(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'+' || c == b'/' || c == b'='
}

/// Decodes a prefix of base64 text (at most `max` characters, whole quads, stopping at padding).
fn decode_prefix(content: &[u8], max: usize) -> Vec<u8> {
    let mut n = content.len().min(max);
    if let Some(p) = content[..n].iter().position(|&c| c == b'=') {
        // the padding ends the data; decode through the end of its quad
        n = (p + 4 - p % 4).min(content.len());
    } else {
        n -= n % 4;
    }
    base64::engine::general_purpose::STANDARD
        .decode(&content[..n])
        .unwrap_or_default()
}

/// Media rule 1: the mime and, when the header says, the dimensions.
fn media(content: &[u8]) -> Option<(&'static str, Option<(u32, u32)>)> {
    if content.len() < MEDIA_MIN || !content.iter().all(|&c| is_base64(c)) {
        return None;
    }
    let head = decode_prefix(content, 64);
    let mime = if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        "image/png"
    } else if head.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        "image/gif"
    } else if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        "image/webp"
    } else if head.starts_with(b"%PDF-") {
        "application/pdf"
    } else {
        // what a harness that gives its model sound and video keeps in its transcript (Qwen Code)
        sound_or_video(&head)?
    };
    let dims = match mime {
        "image/png" => png(&head),
        "image/gif" => gif(&head),
        "image/webp" => webp(&head),
        "image/jpeg" => jpeg(&decode_prefix(content, DIMENSIONS_SCAN)),
        _ => None,
    };
    Some((mime, dims))
}

/// The mime of a sound or video file by its first bytes: MP4 and its kin (`ftyp`: `M4A ` is sound), `WebM`, WAV,
/// Ogg, FLAC, MP3 (an `ID3` tag or a frame sync).
fn sound_or_video(head: &[u8]) -> Option<&'static str> {
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        return Some(if &head[8..12] == b"M4A " {
            "audio/mp4"
        } else {
            "video/mp4"
        });
    }
    if head.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        return Some("video/webm");
    }
    if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WAVE" {
        return Some("audio/wav");
    }
    if head.starts_with(b"OggS") {
        return Some("audio/ogg");
    }
    if head.starts_with(b"fLaC") {
        return Some("audio/flac");
    }
    if head.starts_with(b"ID3") || (head.len() >= 2 && head[0] == 0xff && head[1] & 0xe0 == 0xe0) {
        return Some("audio/mpeg");
    }
    None
}

fn be16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?])))
}

fn le16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?])))
}

fn le24(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(*b.get(at)?) | u32::from(*b.get(at + 1)?) << 8 | u32::from(*b.get(at + 2)?) << 16)
}

fn png(b: &[u8]) -> Option<(u32, u32)> {
    if b.get(12..16)? != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(b.get(16..20)?.try_into().ok()?);
    let h = u32::from_be_bytes(b.get(20..24)?.try_into().ok()?);
    Some((w, h))
}

fn gif(b: &[u8]) -> Option<(u32, u32)> {
    Some((le16(b, 6)?, le16(b, 8)?))
}

fn webp(b: &[u8]) -> Option<(u32, u32)> {
    match b.get(12..16)? {
        b"VP8 " => Some((le16(b, 26)? & 0x3fff, le16(b, 28)? & 0x3fff)),
        b"VP8L" => {
            let (b1, b2, b3, b4) = (
                u32::from(*b.get(21)?),
                u32::from(*b.get(22)?),
                u32::from(*b.get(23)?),
                u32::from(*b.get(24)?),
            );
            Some((1 + (b1 | (b2 & 0x3f) << 8), 1 + (b2 >> 6 | b3 << 2 | (b4 & 0x0f) << 10)))
        }
        b"VP8X" => Some((1 + le24(b, 24)?, 1 + le24(b, 27)?)),
        _ => None,
    }
}

fn jpeg(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    loop {
        // markers may be preceded by fill bytes
        while *b.get(i)? == 0xff && *b.get(i + 1)? == 0xff {
            i += 1;
        }
        if *b.get(i)? != 0xff {
            return None;
        }
        let marker = *b.get(i + 1)?;
        match marker {
            0xd8 | 0x01 | 0xd0..=0xd7 => i += 2,
            0xc0..=0xcf if !matches!(marker, 0xc4 | 0xc8 | 0xcc) => {
                return Some((be16(b, i + 7)?, be16(b, i + 5)?));
            }
            0xd9 | 0xda => return None,
            _ => i += 2 + usize::try_from(be16(b, i + 2)?).ok()?,
        }
    }
}

/// A text blob's preview: the first [`PREVIEW_CHARS`] characters of its unescaped first [`PREVIEW_SCAN`] bytes, and
/// whether that is all of it.
#[must_use]
pub fn preview(escaped: &[u8]) -> (String, bool) {
    // cut at an escape and character boundary at most PREVIEW_SCAN bytes in
    let mut i = 0;
    let limit = escaped.len().min(PREVIEW_SCAN);
    while i < limit {
        let step = if escaped[i] == b'\\' {
            if escaped.get(i + 1) == Some(&b'u') { 6 } else { 2 }
        } else {
            utf8_len(escaped[i])
        };
        if i + step > limit {
            break;
        }
        i += step;
    }
    let mut quoted = Vec::with_capacity(i + 2);
    quoted.push(b'"');
    quoted.extend_from_slice(&escaped[..i]);
    quoted.push(b'"');
    // a lone surrogate escape at the cut does not parse: then fall back to a lossy read of the raw bytes
    let text: String =
        serde_json::from_slice(&quoted).unwrap_or_else(|_| String::from_utf8_lossy(&escaped[..i]).into_owned());
    let mut chars = text.char_indices();
    let cut = chars.nth(PREVIEW_CHARS).map(|(at, _)| at);
    match cut {
        Some(at) => (text[..at].to_owned(), true),
        None => (text, i < escaped.len()),
    }
}

fn utf8_len(first: u8) -> usize {
    match first {
        0xf0..=0xff => 4,
        0xe0..=0xef => 3,
        0xc0..=0xdf => 2,
        _ => 1,
    }
}

/// The number of lines of a text blob's unescaped text (newlines plus one).
#[must_use]
pub fn line_count(escaped: &[u8]) -> u64 {
    let mut n = 1;
    let mut i = 0;
    while i < escaped.len() {
        if escaped[i] == b'\\' {
            match escaped.get(i + 1) {
                Some(b'n') => n += 1,
                Some(b'u')
                    if escaped
                        .get(i + 2..i + 6)
                        .is_some_and(|h| h.eq_ignore_ascii_case(b"000a")) =>
                {
                    n += 1;
                }
                _ => {}
            }
            i += if escaped.get(i + 1) == Some(&b'u') { 6 } else { 2 };
        } else {
            i += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::HashMap;

    use proptest::prelude::*;

    use super::*;

    fn roundtrip(line: &[u8]) -> Extracted {
        let x = extract(line);
        let by_sha: HashMap<Sha, &[u8]> = x.blobs.iter().map(|b| (b.sha, &line[b.range.clone()])).collect();
        let back = splice(&x.stub, &x.spans, |s| by_sha.get(s).copied()).unwrap();
        assert_eq!(back, line, "reconstruct(extract(x)) == x");
        assert_eq!(x.sha, sha(line));
        x
    }

    fn png_b64(w: u32, h: u32, len: usize) -> String {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&w.to_be_bytes());
        bytes.extend_from_slice(&h.to_be_bytes());
        bytes.resize(len, 7);
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn media_is_extracted_with_mime_and_dimensions() {
        let img = png_b64(1600, 1200, 3000);
        let line = format!(
            r#"{{"type":"user","message":{{"content":[{{"type":"image","source":{{"data":"{img}"}}}}]}},"toolUseResult":{{"file":{{"base64":"{img}"}}}}}}"#
        );
        let line = format!("{line}\n");
        let x = roundtrip(line.as_bytes());
        assert_eq!(x.blobs.len(), 2);
        assert_eq!(x.blobs[0].sha, x.blobs[1].sha, "the duplicate is one blob");
        assert_eq!(x.blobs[0].kind, Kind::Media);
        assert_eq!(x.blobs[0].mime, Some("image/png"));
        assert_eq!((x.blobs[0].width, x.blobs[0].height), (Some(1600), Some(1200)));
        assert!(String::from_utf8(x.stub).unwrap().contains(r#""data":"""#));
    }

    #[test]
    fn jpeg_gif_webp_pdf_signatures() {
        let enc = |b: Vec<u8>| base64::engine::general_purpose::STANDARD.encode(b);
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 0, 16];
        jpeg.extend_from_slice(&[0; 14]);
        jpeg.extend_from_slice(&[0xff, 0xc0, 0, 17, 8, 0x01, 0x2c, 0x01, 0x90]);
        jpeg.resize(2000, 1);
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&[0x40, 0x01, 0xf0, 0x00]);
        gif.resize(2000, 1);
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        webp.extend_from_slice(&[10, 0, 0, 0, 0, 0, 0, 0]);
        webp.extend_from_slice(&[0x1f, 0x03, 0x00, 0xdf, 0x01, 0x00]);
        webp.resize(2000, 1);
        let mut pdf = b"%PDF-1.7".to_vec();
        pdf.resize(2000, 1);
        for (bytes, mime, dims) in [
            (jpeg, "image/jpeg", Some((400, 300))),
            (gif, "image/gif", Some((320, 240))),
            (webp, "image/webp", Some((800, 480))),
            (pdf, "application/pdf", None),
        ] {
            let line = format!(r#"{{"data":"{}"}}"#, enc(bytes));
            let x = roundtrip(line.as_bytes());
            assert_eq!(x.blobs.len(), 1, "{mime}");
            assert_eq!(x.blobs[0].mime, Some(mime));
            assert_eq!(x.blobs[0].width.zip(x.blobs[0].height), dims, "{mime}");
        }
    }

    /// Sound and video a harness keeps in its transcript are media too: stored once as a blob with their kind,
    /// never as a text with a preview of base64.
    #[test]
    fn sound_and_video_signatures() {
        let enc = |b: Vec<u8>| base64::engine::general_purpose::STANDARD.encode(b);
        let file = |head: &[u8]| {
            let mut b = head.to_vec();
            b.resize(2000, 1);
            b
        };
        for (bytes, mime) in [
            (file(b"\0\0\0\x20ftypisom\0\0\x02\0"), "video/mp4"),
            (file(b"\0\0\0\x20ftypM4A \0\0\x02\0"), "audio/mp4"),
            (file(&[0x1a, 0x45, 0xdf, 0xa3, 1, 2, 3, 4]), "video/webm"),
            (file(b"RIFF\0\0\0\0WAVEfmt "), "audio/wav"),
            (file(b"OggS\0\x02"), "audio/ogg"),
            (file(b"fLaC\0\0\0\x22"), "audio/flac"),
            (file(b"ID3\x04\0\0"), "audio/mpeg"),
            (file(&[0xff, 0xfb, 0x90, 0x64]), "audio/mpeg"),
        ] {
            let line = format!(r#"{{"inlineData":{{"data":"{}","mimeType":"x"}}}}"#, enc(bytes));
            let x = roundtrip(line.as_bytes());
            assert_eq!(x.blobs.len(), 1, "{mime}");
            assert_eq!(x.blobs[0].mime, Some(mime));
            assert_eq!(x.blobs[0].width, None, "{mime}");
        }
    }

    #[test]
    fn short_or_unknown_media_stays() {
        let small = png_b64(1, 1, 500);
        let x = roundtrip(format!(r#"{{"data":"{small}"}}"#).as_bytes());
        assert_eq!(x.blobs.len(), 0);
        let other = base64::engine::general_purpose::STANDARD.encode(vec![1u8; 3000]);
        let x = roundtrip(format!(r#"{{"data":"{other}"}}"#).as_bytes());
        assert!(x.blobs.is_empty(), "base64 that is no known media and under 16 KiB");
    }

    #[test]
    fn long_text_is_extracted_except_assistant_prose() {
        let long = "line\\n".repeat(4000);
        let user =
            format!(r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","content":"{long}"}}]}}}}"#);
        let x = roundtrip(user.as_bytes());
        assert_eq!(x.blobs.len(), 1);
        assert_eq!(x.blobs[0].kind, Kind::Text);
        let assistant = format!(
            r#"{{"message":{{"content":[{{"type":"text","text":"{long}"}},{{"type":"thinking","thinking":"{long}"}},{{"type":"tool_use","input":{{"content":"{long}"}}}}]}},"type":"assistant"}}"#
        );
        let x = roundtrip(assistant.as_bytes());
        assert_eq!(
            x.blobs.len(),
            1,
            "only the tool input, never the prose (type after message)"
        );
        let content = &assistant.as_bytes()[x.blobs[0].range.clone()];
        assert_eq!(line_count(content), 4001);
        let (p, more) = preview(content);
        assert!(more);
        assert_eq!(p.chars().count(), PREVIEW_CHARS);
        assert!(p.starts_with("line\nline\n"));
    }

    #[test]
    fn invalid_json_is_stored_whole() {
        let torn = format!("{{\"type\":\"user\",\"data\":\"{}\n", "A".repeat(40_000));
        let x = roundtrip(torn.as_bytes());
        assert!(!x.json);
        assert_eq!(x.blobs.len(), 0);
        assert_eq!(x.stub, torn.as_bytes());
    }

    #[test]
    fn previews_cut_at_escape_and_character_boundaries() {
        // 2,730 escapes of 6 bytes fill 16,380 bytes; the next one crosses the scan limit and is left out
        let s = "\\u00e9".repeat(PREVIEW_SCAN / 6 + 1);
        let (p, more) = preview(s.as_bytes());
        assert_eq!(p.chars().count(), PREVIEW_SCAN / 6);
        assert!(p.chars().all(|c| c == 'é') && more);
        // a character split by the scan limit does not panic
        let mut s = "a".repeat(PREVIEW_SCAN - 1);
        s.push('é');
        let (p, more) = preview(s.as_bytes());
        assert!(p.chars().all(|c| c == 'a') && more);
        let (p, more) = preview(b"short\\ttext");
        assert_eq!((p.as_str(), more), ("short\ttext", false));
    }

    #[test]
    fn hex_round_trips() {
        let s = sha(b"x");
        assert_eq!(unhex(&hex(&s)), Some(s));
        assert_eq!(unhex("zz"), None);
    }

    fn big_strings() -> impl Strategy<Value = String> {
        prop_oneof![
            ".{0,40}",
            (1000usize..20_000).prop_map(|n| "x".repeat(n)),
            (1000usize..20_000).prop_map(|n| "é\u{1F600}\"\\\n".repeat(n / 8)),
            (300usize..2000).prop_map(|n| png_b64(u32::try_from(n).unwrap(), 7, n * 2)),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// reconstruct(extract(x)) == x for JSON lines with large strings, escapes and surrogate pairs.
        #[test]
        fn json_lines_round_trip(strings in proptest::collection::vec(big_strings(), 0..6), assistant in any::<bool>()) {
            let content: Vec<serde_json::Value> = strings
                .iter()
                .map(|s| serde_json::json!({"type": "text", "text": s, "other": s}))
                .collect();
            let v = serde_json::json!({"type": if assistant {"assistant"} else {"user"}, "message": {"content": content}});
            let mut line = serde_json::to_vec(&v).unwrap();
            line.push(b'\n');
            roundtrip(&line);
        }

        #[test]
        fn any_bytes_round_trip(mut b in proptest::collection::vec(any::<u8>(), 0..3000)) {
            b.push(b'\n');
            roundtrip(&b);
        }
    }
}
