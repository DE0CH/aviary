//! The app bundle at `/srv/app` (spec 5.2.1): the file list is loaded once into memory (paths, sizes, precompressed
//! siblings) and bodies are read from disk; lookups are in that list only, so no request path is ever joined to a
//! filesystem path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::App;
use crate::http::text;

#[derive(Debug, Clone)]
struct Variant {
    path: PathBuf,
    size: u64,
}

#[derive(Debug, Clone)]
struct File {
    path: PathBuf,
    size: u64,
    etag: String,
    content_type: &'static str,
    cache: &'static str,
    br: Option<Variant>,
    gz: Option<Variant>,
}

/// The loaded bundle. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Bundle(Arc<BundleInner>);

#[derive(Debug)]
struct BundleInner {
    files: HashMap<String, File>,
    index: Vec<u8>,
}

/// The content type of a file name (spec 5.2.1's map, from today's router).
#[must_use]
pub fn content_type(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "webmanifest" => "application/manifest+json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// The content hash in a hashed file name (`[.-][0-9a-f]{16,}\.[a-z0-9]+$`), if any.
#[must_use]
pub fn name_hash(name: &str) -> Option<&str> {
    let (stem, ext) = name.rsplit_once('.')?;
    if ext.is_empty() || !ext.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) {
        return None;
    }
    let start = stem.rfind(['.', '-'])?;
    let hash = &stem[start + 1..];
    (hash.len() >= 16 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))).then_some(hash)
}

/// Spec 5.2.1's `cache-control` for a URL path.
#[must_use]
pub fn cache_control(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.to_ascii_lowercase().ends_with(".html") {
        "no-store"
    } else if path.starts_with("/_expo/static/") || name_hash(name).is_some() {
        "public, max-age=31536000, immutable"
    } else if path == "/sw.js" {
        "no-cache"
    } else {
        "public, max-age=300"
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_dir() {
            walk(&path, out)?;
        } else if kind.is_file() || (kind.is_symlink() && path.is_file()) {
            out.push(path);
        }
    }
    Ok(())
}

impl Bundle {
    /// Loads the list under `root`; the bundle must have an `index.html`.
    ///
    /// # Errors
    /// When the directory cannot be read or has no `index.html`.
    pub fn load(root: &Path) -> std::io::Result<Self> {
        let mut paths = Vec::new();
        walk(root, &mut paths)?;
        let rel = |p: &Path| -> Option<String> {
            let r = p.strip_prefix(root).ok()?.to_str()?.replace('\\', "/");
            Some(format!("/{r}"))
        };
        let sizes: HashMap<String, (PathBuf, u64, u64)> = paths
            .iter()
            .filter_map(|p| {
                let meta = std::fs::metadata(p).ok()?;
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs());
                Some((rel(p)?, (p.clone(), meta.len(), mtime)))
            })
            .collect();
        let mut files = HashMap::new();
        for (url, (path, size, mtime)) in &sizes {
            // a precompressed sibling of a file that exists is a variant, not a file of its own
            if let Some(base) = url.strip_suffix(".br").or_else(|| url.strip_suffix(".gz"))
                && sizes.contains_key(base)
            {
                continue;
            }
            let name = url.rsplit('/').next().unwrap_or(url);
            let variant = |suffix: &str| {
                sizes.get(&format!("{url}{suffix}")).map(|(p, s, _)| Variant {
                    path: p.clone(),
                    size: *s,
                })
            };
            let etag = match name_hash(name) {
                Some(h) => format!("\"{h}\""),
                None => format!("\"{size:x}-{mtime:x}\""),
            };
            files.insert(
                url.clone(),
                File {
                    path: path.clone(),
                    size: *size,
                    etag,
                    content_type: content_type(name),
                    cache: cache_control(url),
                    br: variant(".br"),
                    gz: variant(".gz"),
                },
            );
        }
        let index = std::fs::read(root.join("index.html"))?;
        tracing::info!(files = files.len(), "app bundle loaded");
        Ok(Self(Arc::new(BundleInner { files, index })))
    }

    /// The bundle's `index.html` as built.
    #[must_use]
    pub fn index(&self) -> &[u8] {
        &self.0.index
    }

    /// The number of files.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.files.len()
    }

    /// Whether the bundle has no files.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.files.is_empty()
    }
}

fn accepts(headers: &HeaderMap, coding: &str) -> bool {
    headers
        .get_all(header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|part| {
            let mut it = part.split(';');
            let name = it.next().unwrap_or("").trim();
            let q_zero = it.any(|p| {
                p.trim()
                    .strip_prefix("q=")
                    .and_then(|q| q.trim().parse::<f32>().ok())
                    .is_some_and(|q| q <= 0.0)
            });
            name.eq_ignore_ascii_case(coding) && !q_zero
        })
}

/// Serves a file of the bundle (spec 5.2.1): anyone may fetch it, no identity, no upstream. A missing file is a
/// `no-store` 404.
pub async fn serve(app: &App, path: &str, headers: &HeaderMap) -> Response {
    let Some(f) = app.0.bundle.0.files.get(path) else {
        return text(StatusCode::NOT_FOUND, "Not found");
    };
    let (variant, encoding) = if let Some(v) = f.br.as_ref().filter(|_| accepts(headers, "br")) {
        (Some(v), Some("br"))
    } else if let Some(v) = f.gz.as_ref().filter(|_| accepts(headers, "gzip")) {
        (Some(v), Some("gzip"))
    } else {
        (None, None)
    };
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(f.content_type));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(f.cache));
    let etag = match encoding {
        Some(e) => format!("{}-{e}\"", f.etag.trim_end_matches('"')),
        None => f.etag.clone(),
    };
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    if f.br.is_some() || f.gz.is_some() {
        h.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    }
    if let Some(e) = encoding {
        h.insert(header::CONTENT_ENCODING, HeaderValue::from_static(e));
    }
    let not_modified = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"));
    if not_modified {
        return (StatusCode::NOT_MODIFIED, h).into_response();
    }
    let (disk, size) = variant.map_or((&f.path, f.size), |v| (&v.path, v.size));
    match tokio::fs::read(disk).await {
        Ok(bytes) => {
            h.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
            let mut r = Response::new(Body::from(bytes));
            *r.headers_mut() = h;
            r
        }
        Err(e) => {
            tracing::warn!(error = %e, "bundle file unreadable");
            text(StatusCode::NOT_FOUND, "Not found")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_and_caching() {
        assert_eq!(name_hash("entry-0123456789abcdef.js"), Some("0123456789abcdef"));
        assert_eq!(name_hash("a.0123456789abcdef0.css"), Some("0123456789abcdef0"));
        assert_eq!(name_hash("short-0123.js"), None);
        assert_eq!(name_hash("x-0123456789ABCDEF.js"), None, "uppercase is not a hash");
        assert_eq!(cache_control("/index.html"), "no-store");
        assert_eq!(
            cache_control("/_expo/static/js/web/x.js"),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            cache_control("/fonts/a-0123456789abcdef.woff2"),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(cache_control("/sw.js"), "no-cache");
        assert_eq!(cache_control("/manifest.json"), "public, max-age=300");
        assert_eq!(content_type("a.MJS"), "text/javascript; charset=utf-8");
        assert_eq!(content_type("a.bin"), "application/octet-stream");
    }

    #[test]
    fn encodings() {
        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT_ENCODING, "gzip, deflate, br;q=0".parse().unwrap());
        assert!(accepts(&h, "gzip"));
        assert!(!accepts(&h, "br"));
        h.insert(header::ACCEPT_ENCODING, "br".parse().unwrap());
        assert!(accepts(&h, "br"));
    }
}
