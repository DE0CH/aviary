//! The sandbox token and control token keys (hermit 6.1-6.2): `HERMIT_TOKEN_KEYS` = `kid:base64url(32 bytes)`
//! entries; each yields an Ed25519 signing key and an HMAC control key. Tokens are never stored: Ed25519 is
//! deterministic, so any replica re-derives a sandbox's tokens from its row and the key, which is what makes a start
//! resumable.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};

use hermit_api::token::{Claims, ControlKey, MasterKey, SigningKey, TokenError, TokenKeys, sha256_hex};
use uuid::Uuid;

/// The keys of one replica.
#[derive(Debug)]
pub struct Keys {
    signing: BTreeMap<u32, SigningKey>,
    control: BTreeMap<u32, ControlKey>,
    published: TokenKeys,
    newest: u32,
}

/// What a sandbox token says, from a row.
#[derive(Clone, Debug)]
pub struct TokenFacts<'a> {
    pub sandbox_id: Uuid,
    pub conversation_id: Uuid,
    pub ip: Option<IpAddr>,
    /// Unix seconds (the row's hard expiry).
    pub expires_at: i64,
    pub person: &'a str,
}

impl Keys {
    /// From the list in `HERMIT_TOKEN_KEYS`.
    ///
    /// # Errors
    /// A malformed list, a duplicate kid, or an empty one.
    pub fn parse(list: &str) -> Result<Self, TokenError> {
        let masters = MasterKey::parse_list(list)?;
        let mut signing = BTreeMap::new();
        let mut control = BTreeMap::new();
        for m in &masters {
            signing.insert(m.kid, m.signing_key()?);
            control.insert(m.kid, m.control_key()?);
        }
        let published = TokenKeys {
            keys: signing.values().map(SigningKey::public_key).collect(),
        };
        let newest = *signing.keys().max().ok_or(TokenError::BadMasterKey)?;
        Ok(Self {
            signing,
            control,
            published,
            newest,
        })
    }

    /// The kid new sandboxes use.
    #[must_use]
    pub fn newest(&self) -> u32 {
        self.newest
    }

    #[must_use]
    pub fn has(&self, kid: u32) -> bool {
        self.signing.contains_key(&kid)
    }

    /// `GET /v1/sandbox-token-keys`.
    #[must_use]
    pub fn published(&self) -> &TokenKeys {
        &self.published
    }

    /// The sandbox token of a row signed with `kid`; `None` when the kid is not loaded or the person is malformed.
    #[must_use]
    pub fn sandbox_token(&self, kid: u32, f: &TokenFacts<'_>) -> Option<String> {
        let source = match f.ip {
            Some(IpAddr::V4(v4)) => v4,
            _ => Ipv4Addr::UNSPECIFIED,
        };
        let claims = Claims {
            sandbox_id: f.sandbox_id,
            conversation_id: f.conversation_id,
            source,
            expires_at: u64::try_from(f.expires_at).unwrap_or(0),
            person: f.person.to_owned(),
        };
        self.signing.get(&kid)?.sign(&claims).ok()
    }

    /// The control token hermit presents to the sandbox's :7070.
    #[must_use]
    pub fn control_token(&self, kid: u32, sandbox_id: Uuid) -> Option<String> {
        Some(self.control.get(&kid)?.control_token(sandbox_id))
    }

    /// The descriptor's `control.tokenSha256` (one entry in YYC).
    #[must_use]
    pub fn control_hashes(&self, kid: u32, sandbox_id: Uuid) -> Vec<String> {
        self.control_token(kid, sandbox_id)
            .map(|t| sha256_hex(&t))
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::net::IpAddr;

    use hermit_api::token::{KeySet, VerifyError};

    use super::*;

    const K1: &str = "1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const K2: &str = "2:AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE";

    fn facts(person: &str) -> TokenFacts<'_> {
        TokenFacts {
            sandbox_id: Uuid::from_u128(1),
            conversation_id: Uuid::from_u128(2),
            ip: Some(IpAddr::V4(Ipv4Addr::new(198, 18, 0, 10))),
            expires_at: 4_000_000_000,
            person,
        }
    }

    #[test]
    fn minted_tokens_verify_and_rotation_keeps_old_ones() {
        let one = Keys::parse(K1).unwrap();
        let both = Keys::parse(&format!("{K1},{K2}")).unwrap();
        assert_eq!(both.newest(), 2);
        let old = one.sandbox_token(1, &facts("a@example.org")).unwrap();
        // deterministic: any replica derives the same token
        assert_eq!(both.sandbox_token(1, &facts("a@example.org")).unwrap(), old);
        let set = KeySet::from_keys(both.published());
        let from = IpAddr::V4(Ipv4Addr::new(198, 18, 0, 10));
        let c = set.verify(&old, 1_000, from).unwrap();
        assert_eq!(c.person, "a@example.org");
        let new = both.sandbox_token(2, &facts("a@example.org")).unwrap();
        assert!(set.verify(&new, 1_000, from).is_ok());
        assert_eq!(
            KeySet::from_keys(one.published()).verify(&new, 1_000, from),
            Err(VerifyError::UnknownKid(2))
        );
        // wrong source, expired, tampered
        assert_eq!(
            set.verify(&old, 1_000, IpAddr::V4(Ipv4Addr::new(198, 18, 0, 11))),
            Err(VerifyError::WrongSource)
        );
        assert_eq!(set.verify(&old, 4_000_000_000, from), Err(VerifyError::Expired));
        let mut parts: Vec<&str> = old.split('.').collect();
        let other = one.sandbox_token(1, &facts("b@example.org")).unwrap();
        let other_payload = other.split('.').nth(2).unwrap().to_owned();
        parts[2] = &other_payload;
        assert_eq!(
            set.verify(&parts.join("."), 1_000, from),
            Err(VerifyError::BadSignature)
        );
        assert!(one.sandbox_token(9, &facts("a@example.org")).is_none());
    }

    #[test]
    fn control_tokens() {
        let k = Keys::parse(K1).unwrap();
        let t = k.control_token(1, Uuid::from_u128(1)).unwrap();
        assert!(t.starts_with("hct_"));
        assert_ne!(t, k.control_token(1, Uuid::from_u128(2)).unwrap());
        assert!(hermit_api::token::control_token_matches(
            &t,
            &k.control_hashes(1, Uuid::from_u128(1))
        ));
    }
}
