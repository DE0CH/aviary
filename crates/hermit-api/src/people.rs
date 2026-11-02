//! People (hermit 3.4) and the warm hint (3.5).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::time::Timestamp;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Staff,
}

/// H2 `GET /v1/people/{email}`: 200 for every well-formed address. `allowed == role.is_some()`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    pub email: String,
    pub allowed: bool,
    pub role: Option<Role>,
}

/// `GET /v1/people`: the catalogue as loaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct People {
    /// SHA-256 hex of the catalogue file.
    pub generation: String,
    pub loaded_at: Timestamp,
    pub explicit: Vec<ExplicitPerson>,
    /// `@domain` entries without the `@`: everyone at such a domain is `staff`.
    pub domains: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplicitPerson {
    pub email: String,
    pub role: Role,
}

/// H3 `POST /v1/warm` body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarmRequest {
    pub person: String,
    #[serde(default)]
    pub conversation_id: Option<Uuid>,
}

/// H3 answer, always 202.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarmReply {
    pub hinted: bool,
}

/// The H2 address rules: trimmed and lowercased, `local@domain` with exactly one `@`, both parts non-empty, at most
/// 254 bytes, no whitespace or control characters. Returns the normalised address, or `None` (400 `bad_email`).
#[must_use]
pub fn normalise_email(raw: &str) -> Option<String> {
    let email = raw.trim().to_lowercase();
    if email.is_empty() || email.len() > 254 {
        return None;
    }
    if email.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    if local.is_empty() || domain.is_empty() {
        return None;
    }
    Some(email)
}

#[cfg(test)]
mod tests {
    use super::normalise_email;

    #[test]
    fn email_rules() {
        assert_eq!(
            normalise_email("  Staff@Example.ORG "),
            Some("staff@example.org".into())
        );
        assert_eq!(normalise_email("a@b@c"), None);
        assert_eq!(normalise_email("nobody"), None);
        assert_eq!(normalise_email("a b@example.org"), None);
        assert_eq!(normalise_email("a\u{7}@example.org"), None);
        assert_eq!(normalise_email("@example.org"), None);
        assert_eq!(normalise_email(&format!("{}@example.org", "a".repeat(250))), None);
    }
}
