//! What the gateway strips from and adds to a fixed-name request (3.11.3).

pub use hermit_api::headers::{REQUEST, SANDBOX_CONVERSATION, SANDBOX_ID, SANDBOX_PERSON};

/// Client headers removed from every fixed-name request, besides every `Aviary-*` header but [`FORWARDED`].
pub const STRIPPED: [&str; 3] = ["authorization", "proxy-authorization", "x-api-key"];

/// The client `Aviary-*` headers forwarded unchanged, the ones the sandbox role may set (platform 3.5):
///
/// - `Aviary-Protocol` says which surface number the client speaks, which the callee requires on every call
///   (platform 3.4; hermit answers 400 `unsupported_protocol` without it).
/// - `Aviary-Lease` is the supervisor's single-writer lease, which magpie requires on every state-changing
///   `/v1/sandbox/**` call and on the command poll (magpie 6.9) and checks against hermit's current sandbox.
///
/// Neither names an identity, so a sandbox setting one gains nothing. `Aviary-Request` is stripped and added again
/// by the gateway ([`keep_request_id`]).
pub const FORWARDED: [&str; 2] = ["aviary-protocol", "aviary-lease"];

/// Whether a client header must be removed before forwarding to a fixed name.
#[must_use]
pub fn is_stripped(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    STRIPPED.contains(&lower.as_str())
        || (lower.starts_with(hermit_api::headers::PREFIX) && !FORWARDED.contains(&lower.as_str()))
}

/// The three identity headers the gateway adds, from the verified token: `(name, value)`.
#[must_use]
pub fn identity(claims: &hermit_api::token::Claims) -> [(reqwest::header::HeaderName, String); 3] {
    [
        (SANDBOX_ID, claims.sandbox_id.to_string()),
        (SANDBOX_CONVERSATION, claims.conversation_id.to_string()),
        (SANDBOX_PERSON, claims.person.trim().to_lowercase()),
    ]
}

/// Whether a client-sent `Aviary-Request` may be kept: a lowercase RFC 4122 uuid v7 (platform 3.5, 3.6), else the
/// gateway mints a new one.
#[must_use]
pub fn keep_request_id(value: &str) -> bool {
    let b = value.as_bytes();
    // 8-4-4-4-12 lowercase hex, version nibble 7, variant bits 10xx
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_digit() || (b'a'..=b'f').contains(c),
        })
        && b[14] == b'7'
        && matches!(b[19], b'8' | b'9' | b'a' | b'b')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stripping() {
        for h in [
            "Authorization",
            "proxy-authorization",
            "X-Api-Key",
            "Aviary-Sandbox-Id",
            "aviary-request",
        ] {
            assert!(is_stripped(h), "{h}");
        }
        assert!(!is_stripped("content-type"));
        assert!(!is_stripped("Aviary-Protocol"));
        assert!(!is_stripped("Aviary-Lease"));
        assert!(is_stripped("Aviary-Person"));
        assert!(is_stripped("aviary-admin"));
        assert!(!is_stripped("anthropic-version"));
    }

    #[test]
    fn request_ids() {
        assert!(keep_request_id("0190f6c2-0000-7000-8000-00000000000a"));
        assert!(!keep_request_id("0190f6c2-0000-4000-8000-00000000000a"));
        assert!(!keep_request_id("not-a-uuid"));
        // uppercase, or not the RFC 4122 variant
        assert!(!keep_request_id("0190F6C2-0000-7000-8000-00000000000A"));
        assert!(!keep_request_id("0190f6c2-0000-7000-c000-00000000000a"));
        assert!(!keep_request_id("0190f6c2-0000-7000-7000-00000000000a"));
        assert!(keep_request_id(&uuid::Uuid::now_v7().to_string()));
    }

    #[test]
    fn identity_lowercases_the_person() {
        let c = hermit_api::token::Claims {
            sandbox_id: uuid::Uuid::from_u128(1),
            conversation_id: uuid::Uuid::from_u128(2),
            source: std::net::Ipv4Addr::UNSPECIFIED,
            expires_at: 1,
            person: " Staff@Example.ORG ".into(),
        };
        let h = identity(&c);
        assert_eq!(
            (h[2].0.as_str(), h[2].1.as_str()),
            ("aviary-sandbox-person", "staff@example.org")
        );
        assert_eq!(h[0].0.as_str(), "aviary-sandbox-id");
    }
}
