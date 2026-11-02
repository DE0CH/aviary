//! The sandbox token (hermit 6.1, platform R33) and the control token (6.2).
//!
//! Sandbox token: `hst1.<kid>.<payload>.<sig>`, ASCII and URL-userinfo safe. `kid` is decimal; `payload` is
//! base64url (no padding) of the fixed binary
//! `version=1 (1) | sandbox id (16) | conversation id (16) | source IPv4 (4; 0.0.0.0 = none) | expiry (u64 BE Unix
//! seconds) | person (UTF-8, 1-254 bytes)`; `sig` is base64url of the Ed25519 signature over
//! `"hermit-sandbox-token\0" || <the binary payload>`. Signing is deterministic, so any hermit replica re-derives the
//! same token from the row and the key.
//!
//! Keys: each master key (`kid:base64url(32 bytes)` in hermit's key list) yields by HKDF-SHA256 (empty salt) an
//! Ed25519 seed (info `hermit sandbox token ed25519`) and an HMAC-SHA256 key for control tokens (info `hermit control
//! token`). The control token is `hct_` + base64url(HMAC(control key, "control\0" + <sandbox id, lowercase
//! hyphenated>)); the supervisor learns only its SHA-256.

use std::collections::BTreeMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey};
use ring::{hkdf, hmac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

use crate::sandbox::SandboxState;
use crate::sandbox_surface::hex;
use crate::time::Timestamp;

/// The token's leading tag.
pub const PREFIX: &str = "hst1";
/// The payload's version byte.
pub const PAYLOAD_VERSION: u8 = 1;
/// The signed context string (with its NUL).
pub const SIGNING_CONTEXT: &[u8] = b"hermit-sandbox-token\0";
/// HKDF info strings.
pub const INFO_ED25519: &[u8] = b"hermit sandbox token ed25519";
pub const INFO_CONTROL: &[u8] = b"hermit control token";
/// The control token's prefix.
pub const CONTROL_PREFIX: &str = "hct_";
/// The only algorithm in `GET /v1/sandbox-token-keys`.
pub const ALG_ED25519: &str = "Ed25519";

const FIXED_LEN: usize = 1 + 16 + 16 + 4 + 8;
/// Longest accepted token text (a 254-byte person gives about 450 characters).
pub const MAX_TOKEN_LEN: usize = 600;

/// What a token says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claims {
    pub sandbox_id: Uuid,
    pub conversation_id: Uuid,
    /// The only address the token may be presented from; `0.0.0.0` = any.
    pub source: Ipv4Addr,
    /// Unix seconds (the row's hard expiry).
    pub expires_at: u64,
    /// The owner's trimmed, lowercased email (1-254 bytes).
    pub person: String,
}

impl Claims {
    fn encode(&self) -> Result<Vec<u8>, TokenError> {
        let person = self.person.as_bytes();
        if person.is_empty() || person.len() > 254 {
            return Err(TokenError::BadPerson);
        }
        let mut out = Vec::with_capacity(FIXED_LEN + person.len());
        out.push(PAYLOAD_VERSION);
        out.extend_from_slice(self.sandbox_id.as_bytes());
        out.extend_from_slice(self.conversation_id.as_bytes());
        out.extend_from_slice(&self.source.octets());
        out.extend_from_slice(&self.expires_at.to_be_bytes());
        out.extend_from_slice(person);
        Ok(out)
    }

    fn decode(bytes: &[u8]) -> Result<Self, VerifyError> {
        if bytes.len() <= FIXED_LEN || bytes.len() > FIXED_LEN + 254 || bytes[0] != PAYLOAD_VERSION {
            return Err(VerifyError::Malformed);
        }
        let uuid = |at: usize| -> Result<Uuid, VerifyError> {
            let b: [u8; 16] = bytes[at..at + 16].try_into().map_err(|_| VerifyError::Malformed)?;
            Ok(Uuid::from_bytes(b))
        };
        let ip: [u8; 4] = bytes[33..37].try_into().map_err(|_| VerifyError::Malformed)?;
        let exp: [u8; 8] = bytes[37..45].try_into().map_err(|_| VerifyError::Malformed)?;
        let person = std::str::from_utf8(&bytes[FIXED_LEN..]).map_err(|_| VerifyError::Malformed)?;
        Ok(Self {
            sandbox_id: uuid(1)?,
            conversation_id: uuid(17)?,
            source: Ipv4Addr::from(ip),
            expires_at: u64::from_be_bytes(exp),
            person: person.to_owned(),
        })
    }
}

/// Failures while making keys or tokens.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("bad master key entry (want kid:base64url(32 bytes))")]
    BadMasterKey,
    #[error("duplicate kid {0}")]
    DuplicateKid(u32),
    #[error("person must be 1-254 bytes")]
    BadPerson,
    #[error("key derivation failed")]
    Derive,
}

/// Why a token was refused. Every variant is definitive (403 `not_allowed` at the gateway); [`UnknownKid`] means
/// "refetch the key set once, then refuse".
///
/// [`UnknownKid`]: VerifyError::UnknownKid
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    #[error("malformed token")]
    Malformed,
    #[error("unknown kid {0}")]
    UnknownKid(u32),
    #[error("bad signature")]
    BadSignature,
    #[error("expired")]
    Expired,
    #[error("presented from another address")]
    WrongSource,
}

/// A master key from hermit's key list.
#[derive(Clone)]
pub struct MasterKey {
    pub kid: u32,
    secret: [u8; 32],
}

impl fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MasterKey")
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

struct Len32;
impl hkdf::KeyType for Len32 {
    fn len(&self) -> usize {
        32
    }
}

impl MasterKey {
    #[must_use]
    pub fn new(kid: u32, secret: [u8; 32]) -> Self {
        Self { kid, secret }
    }

    /// Parses `kid:base64url(32 bytes)`.
    ///
    /// # Errors
    /// [`TokenError::BadMasterKey`] for anything else.
    pub fn parse(entry: &str) -> Result<Self, TokenError> {
        let (kid, key) = entry.trim().split_once(':').ok_or(TokenError::BadMasterKey)?;
        let kid = parse_kid(kid).ok_or(TokenError::BadMasterKey)?;
        let bytes = B64.decode(key).map_err(|_| TokenError::BadMasterKey)?;
        let secret: [u8; 32] = bytes.try_into().map_err(|_| TokenError::BadMasterKey)?;
        Ok(Self { kid, secret })
    }

    /// Parses a comma-separated list; kids must be distinct.
    ///
    /// # Errors
    /// A bad entry, a duplicate kid, or an empty list.
    pub fn parse_list(list: &str) -> Result<Vec<Self>, TokenError> {
        let mut out: Vec<Self> = Vec::new();
        for entry in list.split(',').filter(|e| !e.trim().is_empty()) {
            let key = Self::parse(entry)?;
            if out.iter().any(|k| k.kid == key.kid) {
                return Err(TokenError::DuplicateKid(key.kid));
            }
            out.push(key);
        }
        if out.is_empty() {
            return Err(TokenError::BadMasterKey);
        }
        Ok(out)
    }

    fn derive(&self, info: &[u8]) -> Result<[u8; 32], TokenError> {
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(&self.secret);
        let info = [info];
        let okm = prk.expand(&info, Len32).map_err(|_| TokenError::Derive)?;
        let mut out = [0u8; 32];
        okm.fill(&mut out).map_err(|_| TokenError::Derive)?;
        Ok(out)
    }

    /// The Ed25519 signing key of this kid.
    ///
    /// # Errors
    /// Only if the crypto library refuses the derived seed (never in practice).
    pub fn signing_key(&self) -> Result<SigningKey, TokenError> {
        let seed = self.derive(INFO_ED25519)?;
        let pair = Ed25519KeyPair::from_seed_unchecked(&seed).map_err(|_| TokenError::Derive)?;
        Ok(SigningKey { kid: self.kid, pair })
    }

    /// The HMAC key for control tokens of this kid.
    ///
    /// # Errors
    /// Only if derivation fails (never in practice).
    pub fn control_key(&self) -> Result<ControlKey, TokenError> {
        Ok(ControlKey(hmac::Key::new(
            hmac::HMAC_SHA256,
            &self.derive(INFO_CONTROL)?,
        )))
    }
}

fn parse_kid(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 10 || !s.bytes().all(|b| b.is_ascii_digit()) || (s.len() > 1 && s.starts_with('0')) {
        return None;
    }
    s.parse().ok()
}

/// Signs sandbox tokens (hermit only).
pub struct SigningKey {
    pub kid: u32,
    pair: Ed25519KeyPair,
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SigningKey")
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

impl SigningKey {
    /// Makes the token for these claims.
    ///
    /// # Errors
    /// [`TokenError::BadPerson`] for an empty or over-long person.
    pub fn sign(&self, claims: &Claims) -> Result<String, TokenError> {
        let payload = claims.encode()?;
        let mut msg = Vec::with_capacity(SIGNING_CONTEXT.len() + payload.len());
        msg.extend_from_slice(SIGNING_CONTEXT);
        msg.extend_from_slice(&payload);
        let sig = self.pair.sign(&msg);
        Ok(format!(
            "{PREFIX}.{}.{}.{}",
            self.kid,
            B64.encode(&payload),
            B64.encode(sig.as_ref())
        ))
    }

    /// This key's entry for `GET /v1/sandbox-token-keys`.
    #[must_use]
    pub fn public_key(&self) -> TokenKey {
        TokenKey {
            kid: self.kid,
            alg: ALG_ED25519.to_owned(),
            public_key: B64.encode(self.pair.public_key().as_ref()),
        }
    }
}

/// The parts of a token, before verification.
struct Parts<'a> {
    kid: u32,
    payload: Vec<u8>,
    sig: Vec<u8>,
    _text: &'a str,
}

fn split(token: &str) -> Result<Parts<'_>, VerifyError> {
    if token.len() > MAX_TOKEN_LEN || !token.is_ascii() {
        return Err(VerifyError::Malformed);
    }
    let mut it = token.split('.');
    let (Some(PREFIX), Some(kid), Some(payload), Some(sig), None) =
        (it.next(), it.next(), it.next(), it.next(), it.next())
    else {
        return Err(VerifyError::Malformed);
    };
    let kid = parse_kid(kid).ok_or(VerifyError::Malformed)?;
    let payload = B64.decode(payload).map_err(|_| VerifyError::Malformed)?;
    let sig = B64.decode(sig).map_err(|_| VerifyError::Malformed)?;
    if sig.len() != 64 {
        return Err(VerifyError::Malformed);
    }
    Ok(Parts {
        kid,
        payload,
        sig,
        _text: token,
    })
}

/// The kid a token names, without verifying anything (to decide whether to refetch the key set).
///
/// # Errors
/// [`VerifyError::Malformed`].
pub fn peek_kid(token: &str) -> Result<u32, VerifyError> {
    split(token).map(|p| p.kid)
}

/// The verifying side: hermit's published keys (the gateway and tools cache them).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeySet {
    keys: BTreeMap<u32, [u8; 32]>,
}

impl KeySet {
    /// From `GET /v1/sandbox-token-keys`; entries of another algorithm or a malformed key are skipped.
    #[must_use]
    pub fn from_keys(keys: &TokenKeys) -> Self {
        let keys = keys
            .keys
            .iter()
            .filter(|k| k.alg == ALG_ED25519)
            .filter_map(|k| {
                let b: [u8; 32] = B64.decode(&k.public_key).ok()?.try_into().ok()?;
                Some((k.kid, b))
            })
            .collect();
        Self { keys }
    }

    #[must_use]
    pub fn contains(&self, kid: u32) -> bool {
        self.keys.contains_key(&kid)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Verifies a token presented from `from` at Unix second `now` (6.3 step 2): it parses, its kid is known, the
    /// signature verifies, it has not expired (`now < expiresAt`), and its source address equals `from` (an
    /// IPv4-mapped IPv6 address counts as its IPv4; skipped when the token carries 0.0.0.0).
    ///
    /// # Errors
    /// The first check that fails.
    pub fn verify(&self, token: &str, now: u64, from: IpAddr) -> Result<Claims, VerifyError> {
        let parts = split(token)?;
        let public = self.keys.get(&parts.kid).ok_or(VerifyError::UnknownKid(parts.kid))?;
        let mut msg = Vec::with_capacity(SIGNING_CONTEXT.len() + parts.payload.len());
        msg.extend_from_slice(SIGNING_CONTEXT);
        msg.extend_from_slice(&parts.payload);
        UnparsedPublicKey::new(&ED25519, public)
            .verify(&msg, &parts.sig)
            .map_err(|_| VerifyError::BadSignature)?;
        let claims = Claims::decode(&parts.payload)?;
        if now >= claims.expires_at {
            return Err(VerifyError::Expired);
        }
        if !claims.source.is_unspecified() {
            let from = match from {
                IpAddr::V4(v4) => Some(v4),
                IpAddr::V6(v6) => v6.to_ipv4_mapped(),
            };
            if from != Some(claims.source) {
                return Err(VerifyError::WrongSource);
            }
        }
        Ok(claims)
    }
}

/// The HMAC key for control tokens.
pub struct ControlKey(hmac::Key);

impl fmt::Debug for ControlKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ControlKey(..)")
    }
}

impl ControlKey {
    /// The control token hermit presents to this sandbox's :7070 (never stored, never leaves hermit).
    #[must_use]
    pub fn control_token(&self, sandbox_id: Uuid) -> String {
        let msg = format!("control\0{sandbox_id}");
        let tag = hmac::sign(&self.0, msg.as_bytes());
        format!("{CONTROL_PREFIX}{}", B64.encode(tag.as_ref()))
    }
}

/// SHA-256 hex of a token (the descriptor's `control.tokenSha256` entries).
#[must_use]
pub fn sha256_hex(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

/// The supervisor's check: does `sha256(presented)` equal one of `hashes` (constant time per entry)?
#[must_use]
pub fn control_token_matches(presented: &str, hashes: &[String]) -> bool {
    let digest = sha256_hex(presented);
    let mut ok = false;
    for h in hashes {
        ok |= bool::from(digest.as_bytes().ct_eq(h.to_ascii_lowercase().as_bytes()));
    }
    ok
}

/// `GET /v1/sandbox-token-keys`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenKeys {
    pub keys: Vec<TokenKey>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenKey {
    pub kid: u32,
    /// `Ed25519`.
    pub alg: String,
    /// base64url, 32 bytes.
    pub public_key: String,
}

/// `GET /v1/sandbox-tokens/{sandboxId}` (3.9): liveness for the gateway and tools.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenLiveness {
    pub sandbox_id: Uuid,
    pub conversation_id: Uuid,
    pub person: String,
    pub state: SandboxState,
    /// Not `gone` and the person allowed by the current catalogue.
    pub live: bool,
    pub expires_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(kid: u32, fill: u8) -> MasterKey {
        MasterKey::new(kid, [fill; 32])
    }

    fn claims() -> Claims {
        Claims {
            sandbox_id: Uuid::from_u128(0x0190_f6c2_0000_7000_8000_0000_0000_0001),
            conversation_id: Uuid::from_u128(0x0190_f6c2_0000_7000_8000_0000_0000_0002),
            source: Ipv4Addr::new(10, 203, 1, 7),
            expires_at: 1_800_000_000,
            person: "staff@example.org".into(),
        }
    }

    #[test]
    fn sign_and_verify() {
        let k = key(1, 7).signing_key().unwrap();
        let t = k.sign(&claims()).unwrap();
        assert!(t.starts_with("hst1.1."));
        let set = KeySet::from_keys(&TokenKeys {
            keys: vec![k.public_key()],
        });
        let from = IpAddr::V4(Ipv4Addr::new(10, 203, 1, 7));
        assert_eq!(set.verify(&t, 1_700_000_000, from).unwrap(), claims());
        // ::ffff:10.203.1.7
        let mapped: IpAddr = "::ffff:10.203.1.7".parse().unwrap();
        assert!(set.verify(&t, 1_700_000_000, mapped).is_ok());
        assert_eq!(set.verify(&t, 1_800_000_000, from), Err(VerifyError::Expired));
        assert_eq!(
            set.verify(&t, 1, IpAddr::V4(Ipv4Addr::new(10, 203, 1, 8))),
            Err(VerifyError::WrongSource)
        );
        assert_eq!(KeySet::default().verify(&t, 1, from), Err(VerifyError::UnknownKid(1)));
        assert_eq!(peek_kid(&t), Ok(1));
        // deterministic: any replica re-derives the same token
        assert_eq!(key(1, 7).signing_key().unwrap().sign(&claims()).unwrap(), t);
    }

    #[test]
    fn any_source_when_unspecified() {
        let k = key(3, 9).signing_key().unwrap();
        let mut c = claims();
        c.source = Ipv4Addr::UNSPECIFIED;
        let t = k.sign(&c).unwrap();
        let set = KeySet::from_keys(&TokenKeys {
            keys: vec![k.public_key()],
        });
        assert!(set.verify(&t, 1, "2001:db8::1".parse().unwrap()).is_ok());
    }

    #[test]
    fn another_keys_signature_fails() {
        let a = key(1, 1).signing_key().unwrap();
        let b = key(1, 2).signing_key().unwrap();
        let t = a.sign(&claims()).unwrap();
        let set = KeySet::from_keys(&TokenKeys {
            keys: vec![b.public_key()],
        });
        assert_eq!(
            set.verify(&t, 1, IpAddr::V4(claims().source)),
            Err(VerifyError::BadSignature)
        );
    }

    #[test]
    fn master_keys_parse() {
        let secret = B64.encode([5u8; 32]);
        let keys = MasterKey::parse_list(&format!("1:{secret}, 2:{secret}")).unwrap();
        assert_eq!(keys.iter().map(|k| k.kid).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(
            MasterKey::parse_list(&format!("1:{secret},1:{secret}")).unwrap_err(),
            TokenError::DuplicateKid(1)
        );
        assert!(MasterKey::parse("1:short").is_err());
        assert!(MasterKey::parse(&format!("x:{secret}")).is_err());
        assert!(MasterKey::parse(&format!("01:{secret}")).is_err());
        assert!(MasterKey::parse_list("").is_err());
        // the Debug form never shows the secret
        assert!(!format!("{:?}", keys[0]).contains(&secret));
    }

    #[test]
    fn control_tokens() {
        let k = key(1, 4).control_key().unwrap();
        let id = claims().sandbox_id;
        let t = k.control_token(id);
        assert!(t.starts_with(CONTROL_PREFIX));
        assert_eq!(t, key(1, 4).control_key().unwrap().control_token(id));
        assert_ne!(t, k.control_token(Uuid::from_u128(9)));
        let hashes = vec!["00".repeat(32), sha256_hex(&t)];
        assert!(control_token_matches(&t, &hashes));
        assert!(control_token_matches(&t, &[sha256_hex(&t).to_uppercase()]));
        assert!(!control_token_matches("hct_other", &hashes));
        assert!(!control_token_matches(&t, &[]));
        // the control and signing keys of one master key differ
        assert_ne!(
            key(1, 4).derive(INFO_CONTROL).unwrap(),
            key(1, 4).derive(INFO_ED25519).unwrap()
        );
    }

    #[test]
    fn person_bounds() {
        let k = key(1, 1).signing_key().unwrap();
        let mut c = claims();
        c.person = String::new();
        assert_eq!(k.sign(&c), Err(TokenError::BadPerson));
        c.person = "a".repeat(255);
        assert_eq!(k.sign(&c), Err(TokenError::BadPerson));
        c.person = "a".repeat(254);
        let t = k.sign(&c).unwrap();
        assert!(t.len() < MAX_TOKEN_LEN);
    }

    #[test]
    fn malformed() {
        for t in [
            "",
            "hst1",
            "hst2.1.AA.AA",
            "hst1.x.AA.AA",
            "hst1.1.AA.AA",
            "hst1.1.!!.AA",
            "hst1.1.AA.AA.AA",
        ] {
            let from = IpAddr::V4(Ipv4Addr::LOCALHOST);
            assert_eq!(KeySet::default().verify(t, 0, from), Err(VerifyError::Malformed), "{t}");
        }
    }
}
