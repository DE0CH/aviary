//! The known-answer vector of the token formats (`vectors/token-v1.json`, computed by `vectors/token-v1.py` with
//! Python's `cryptography`, not with this crate). The values are also written out here, so neither the file nor the
//! codec can change alone: a different byte layout, signing context, HKDF salt or info, or HMAC input fails this
//! test, as it would fail every other gateway, tools or hermit implementation.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use hermit_api::token::{Claims, KeySet, MasterKey, TokenKeys, control_token_matches, peek_kid, sha256_hex};
use serde_json::Value;
use uuid::Uuid;

const MASTER_KEY: &str = "1:BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc";
const PAYLOAD_HEX: &str = "010190f6c20000700080000000000000010190f6c2000070008000000000000002c0000207000000006b49d200\
                           7374616666406578616d706c652e6f7267";
const TOKEN: &str = "hst1.1.AQGQ9sIAAHAAgAAAAAAAAAEBkPbCAABwAIAAAAAAAAACwAACBwAAAABrSdIAc3RhZmZAZXhhbXBsZS5vcmc.\
                     BshwBoZfSVF4vOt5b_NYB4A4SfZg0n8_WcFTMZ49NdtMjJA09IOVy6j6Enpa8MdylPirxw7DNv7sj8ioCrHLCg";
const PUBLIC_KEY: &str = "p-Rs6z8KcdQsdkKo5b22-NQxDMOV-A1W1PEv19s0IRQ";
const CONTROL_TOKEN: &str = "hct_q4hwgtfUFXSnJjxhN7DXbpEhX5V6j_d5EmHr1L1Wh7Q";
const CONTROL_TOKEN_SHA256: &str = "2dd0d5a9a830a44f7384434f0272f6ac741e8d1d5a6107e86e51bee3c1188e12";

fn claims() -> Claims {
    Claims {
        sandbox_id: "0190f6c2-0000-7000-8000-000000000001".parse().unwrap(),
        conversation_id: "0190f6c2-0000-7000-8000-000000000002".parse().unwrap(),
        source: Ipv4Addr::new(192, 0, 2, 7),
        expires_at: 1_800_000_000,
        person: "staff@example.org".into(),
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[test]
fn the_file_holds_the_vector() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("vectors/token-v1.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    assert_eq!(v["masterKey"], MASTER_KEY);
    assert_eq!(v["payloadHex"], PAYLOAD_HEX);
    assert_eq!(v["token"], TOKEN);
    assert_eq!(v["publicKey"]["kid"], 1);
    assert_eq!(v["publicKey"]["alg"], "Ed25519");
    assert_eq!(v["publicKey"]["publicKey"], PUBLIC_KEY);
    assert_eq!(v["controlToken"], CONTROL_TOKEN);
    assert_eq!(v["controlTokenSha256"], CONTROL_TOKEN_SHA256);
    let c = claims();
    assert_eq!(v["claims"]["sandboxId"], c.sandbox_id.to_string());
    assert_eq!(v["claims"]["conversationId"], c.conversation_id.to_string());
    assert_eq!(v["claims"]["source"], c.source.to_string());
    assert_eq!(v["claims"]["expiresAt"], c.expires_at);
    assert_eq!(v["claims"]["person"], c.person);
}

#[test]
fn signing_gives_the_vector_byte_for_byte() {
    let key = MasterKey::parse(MASTER_KEY).unwrap();
    assert_eq!(key.kid, 1);
    let signing = key.signing_key().unwrap();
    let token = signing.sign(&claims()).unwrap();
    assert_eq!(token, TOKEN);
    // the payload part is the documented binary layout
    let payload = token.split('.').nth(2).unwrap();
    assert_eq!(hex(&B64.decode(payload).unwrap()), PAYLOAD_HEX);
    // the published key
    let public = signing.public_key();
    assert_eq!(
        (public.kid, public.alg.as_str(), public.public_key.as_str()),
        (1, "Ed25519", PUBLIC_KEY)
    );
}

#[test]
fn the_vector_verifies_with_the_published_key_alone() {
    let keys: TokenKeys = serde_json::from_value(serde_json::json!({
        "keys": [{"kid": 1, "alg": "Ed25519", "publicKey": PUBLIC_KEY}]
    }))
    .unwrap();
    let set = KeySet::from_keys(&keys);
    assert_eq!(peek_kid(TOKEN), Ok(1));
    let from = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7));
    assert_eq!(set.verify(TOKEN, 1_799_999_999, from).unwrap(), claims());
    assert!(set.verify(TOKEN, 1_800_000_000, from).is_err());
}

#[test]
fn the_control_token_is_the_vector() {
    let key = MasterKey::parse(MASTER_KEY).unwrap().control_key().unwrap();
    let id: Uuid = claims().sandbox_id;
    let token = key.control_token(id);
    assert_eq!(token, CONTROL_TOKEN);
    assert_eq!(sha256_hex(&token), CONTROL_TOKEN_SHA256);
    assert!(control_token_matches(CONTROL_TOKEN, &[CONTROL_TOKEN_SHA256.to_owned()]));
}
