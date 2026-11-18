//! A frozen sandbox's key (jarvis4 docs/PLAN-fabric.md, "How a frozen image travels"): every eviction seals the
//! sandbox under a fresh 256-bit key of its own, and hermit keeps that key in the row sealed under its freeze key
//! (`HERMIT_FREEZE_KEY`, base64 of 32 bytes): AES-256-GCM, `base64(nonce ‖ ciphertext ‖ tag)`. The row's id is the
//! associated data, so a key copied onto another row does not open.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom as _, SystemRandom};
use uuid::Uuid;

fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter().fold(String::with_capacity(b.len() * 2), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

/// Hermit's freeze key.
#[derive(Clone)]
pub struct FreezeKey(LessSafeKey);

impl std::fmt::Debug for FreezeKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FreezeKey(..)")
    }
}

impl FreezeKey {
    /// From `HERMIT_FREEZE_KEY`'s value.
    ///
    /// # Errors
    /// Not base64 of exactly 32 bytes.
    pub fn parse(b64: &str) -> Result<Self, String> {
        let raw = B64
            .decode(b64.trim())
            .map_err(|_| "HERMIT_FREEZE_KEY is not base64".to_owned())?;
        if raw.len() != 32 {
            return Err(format!("HERMIT_FREEZE_KEY is {} bytes, not 32", raw.len()));
        }
        let k = UnboundKey::new(&AES_256_GCM, &raw).map_err(|_| "HERMIT_FREEZE_KEY refused".to_owned())?;
        Ok(Self(LessSafeKey::new(k)))
    }

    /// A fresh key for one eviction, and its sealed form for row `id`: (64 hex digits for sandboxd, sealed).
    ///
    /// # Errors
    /// The system's random source failed.
    pub fn fresh(&self, id: Uuid) -> Result<(String, String), String> {
        let rng = SystemRandom::new();
        let mut k = [0u8; 32];
        rng.fill(&mut k).map_err(|_| "no randomness".to_owned())?;
        let hex = hex(&k);
        Ok((hex, self.seal(id, &k, &rng)?))
    }

    fn seal(&self, id: Uuid, key: &[u8; 32], rng: &SystemRandom) -> Result<String, String> {
        let mut nonce = [0u8; NONCE_LEN];
        rng.fill(&mut nonce).map_err(|_| "no randomness".to_owned())?;
        let mut buf = key.to_vec();
        self.0
            .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(id.as_bytes()), &mut buf)
            .map_err(|_| "sealing failed".to_owned())?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&buf);
        Ok(B64.encode(out))
    }

    /// The key sealed for row `id`, as 64 hex digits for sandboxd.
    ///
    /// # Errors
    /// Not a key this freeze key sealed for this row.
    pub fn open(&self, id: Uuid, sealed: &str) -> Result<String, String> {
        let raw = B64
            .decode(sealed)
            .map_err(|_| "a sealed freeze key is not base64".to_owned())?;
        if raw.len() != NONCE_LEN + 32 + AES_256_GCM.tag_len() {
            return Err("a sealed freeze key of the wrong length".into());
        }
        let (n, ct) = raw.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(n).map_err(|_| "a bad nonce".to_owned())?;
        let mut buf = ct.to_vec();
        let k = self
            .0
            .open_in_place(nonce, Aad::from(id.as_bytes()), &mut buf)
            .map_err(|_| "the freeze key does not open this sealed key (another key, or another row's)".to_owned())?;
        Ok(hex(k))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: u8) -> FreezeKey {
        FreezeKey::parse(&B64.encode([b; 32])).expect("a key")
    }

    #[test]
    fn a_sealed_key_opens_for_its_row_only_and_under_its_freeze_key_only() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let k = key(7);
        let (hex, sealed) = k.fresh(a).expect("fresh");
        assert_eq!(hex.len(), 64);
        assert!(hex.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(k.open(a, &sealed).expect("opens"), hex);
        assert!(k.open(b, &sealed).is_err(), "another row's sealed key opened");
        assert!(key(8).open(a, &sealed).is_err(), "another freeze key opened it");
        // two evictions never share a key
        assert_ne!(k.fresh(a).expect("fresh").0, hex);
    }

    #[test]
    fn a_freeze_key_is_32_bytes_of_base64() {
        assert!(FreezeKey::parse("not base64!").is_err());
        assert!(FreezeKey::parse(&B64.encode([1u8; 16])).is_err());
        assert!(FreezeKey::parse(&B64.encode([1u8; 32])).is_ok());
    }
}
