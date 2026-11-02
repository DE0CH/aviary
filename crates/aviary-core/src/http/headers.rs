//! The headers the services define (platform 3.5): one prefix, `Aviary-`.

use axum::http::{HeaderMap, HeaderName};

/// The prefix of every header the services define (lowercase, as header names are compared).
pub const PREFIX: &str = "aviary-";

/// Correlation id (uuid v7), minted at the edge and forwarded unchanged; logged as `request`.
pub const AVIARY_REQUEST: HeaderName = HeaderName::from_static("aviary-request");
/// `<surface>=<n>`, sent by every client (platform 3.4).
pub const AVIARY_PROTOCOL: HeaderName = HeaderName::from_static("aviary-protocol");
/// The signed-in person (set by pigeon).
pub const AVIARY_PERSON: HeaderName = HeaderName::from_static("aviary-person");
/// An admin viewing as the person (set by pigeon).
pub const AVIARY_VIEWER: HeaderName = HeaderName::from_static("aviary-viewer");
/// The view-as session, at most 64 characters (set by pigeon).
pub const AVIARY_VIEW_SESSION: HeaderName = HeaderName::from_static("aviary-view-session");
/// The operator behind an admin route (set by the admin tooling).
pub const AVIARY_ADMIN: HeaderName = HeaderName::from_static("aviary-admin");
/// The verified sandbox behind a call (set by the gateway only).
pub const AVIARY_SANDBOX_ID: HeaderName = HeaderName::from_static("aviary-sandbox-id");
/// The verified sandbox's conversation (set by the gateway only).
pub const AVIARY_SANDBOX_CONVERSATION: HeaderName = HeaderName::from_static("aviary-sandbox-conversation");
/// The verified sandbox's owner, trimmed and lowercased (set by the gateway only).
pub const AVIARY_SANDBOX_PERSON: HeaderName = HeaderName::from_static("aviary-sandbox-person");
/// magpie's single-writer lease (set by the supervisor).
pub const AVIARY_LEASE: HeaderName = HeaderName::from_static("aviary-lease");
/// Upload file name, percent-encoded (set by pigeon).
pub const AVIARY_UPLOAD_NAME: HeaderName = HeaderName::from_static("aviary-upload-name");
/// Upload kind, `file` or `image` (set by pigeon).
pub const AVIARY_UPLOAD_KIND: HeaderName = HeaderName::from_static("aviary-upload-kind");

/// Headers every authenticated caller may send.
pub const ALWAYS_ALLOWED: [HeaderName; 2] = [AVIARY_REQUEST, AVIARY_PROTOCOL];

/// The headers the gateway sets for a verified sandbox.
pub const SANDBOX_IDENTITY: [HeaderName; 3] = [AVIARY_SANDBOX_ID, AVIARY_SANDBOX_CONVERSATION, AVIARY_SANDBOX_PERSON];

/// Whether `name` is one of the services' headers.
#[must_use]
pub fn is_aviary(name: &HeaderName) -> bool {
    name.as_str().starts_with(PREFIX)
}

/// Removes every `Aviary-*` header except [`ALWAYS_ALLOWED`] and `allowed`; returns how many values were removed.
pub fn strip_aviary(headers: &mut HeaderMap, allowed: &[HeaderName]) -> usize {
    strip(headers, |n| ALWAYS_ALLOWED.contains(n) || allowed.contains(n))
}

/// Removes every `Aviary-*` header (a request from outside: a person's browser or app, a sandbox); returns how many
/// values were removed.
pub fn strip_all(headers: &mut HeaderMap) -> usize {
    strip(headers, |_| false)
}

fn strip(headers: &mut HeaderMap, keep: impl Fn(&HeaderName) -> bool) -> usize {
    let names: Vec<HeaderName> = headers.keys().filter(|n| is_aviary(n) && !keep(n)).cloned().collect();
    let mut removed = 0;
    for n in names {
        if let axum::http::header::Entry::Occupied(e) = headers.entry(n) {
            removed += e.remove_entry_mult().1.count();
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
        }
        h
    }

    #[test]
    fn strip_keeps_allowed_and_non_aviary() {
        let mut h = map(&[
            ("Aviary-Person", "a@b.c"),
            ("aviary-sandbox-id", "x"),
            ("Aviary-Sandbox-Id", "y"),
            ("aviary-request", "r"),
            ("aviary-protocol", "p=1"),
            ("content-type", "text/plain"),
            ("x-aviary-thing", "kept: not the prefix"),
        ]);
        assert_eq!(strip_aviary(&mut h, &[AVIARY_PERSON]), 2);
        let mut names: Vec<&str> = h.keys().map(HeaderName::as_str).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "aviary-person",
                "aviary-protocol",
                "aviary-request",
                "content-type",
                "x-aviary-thing"
            ]
        );
    }

    #[test]
    fn strip_all_removes_every_aviary_header() {
        let mut h = map(&[("aviary-request", "r"), ("aviary-person", "p"), ("accept", "*/*")]);
        assert_eq!(strip_all(&mut h), 2);
        assert_eq!(h.len(), 1);
    }
}
