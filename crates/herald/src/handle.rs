//! Channel handles: `<chat id>.<signature>`, the signature an HMAC-SHA256 of the conversation and the chat id under
//! `HERALD_HANDLE_KEY`, cut to 128 bits and written in unpadded base64url. A handle is valid only together with the
//! conversation it was made for (the gateway's verified `Aviary-Sandbox-Conversation`).

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::hmac;
use subtle::ConstantTimeEq as _;

/// Bytes of the MAC kept in a handle.
const SIG_BYTES: usize = 16;

/// The signing key.
#[derive(Clone)]
pub struct Key(hmac::Key);

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key(..)")
    }
}

impl Key {
    /// A key from the configured secret (at least 32 bytes of text).
    #[must_use]
    pub fn new(secret: &str) -> Self {
        Self(hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes()))
    }

    fn mac(&self, conversation: &str, chat_id: i64) -> hmac::Tag {
        let msg = format!("herald-channel-v1\n{conversation}\n{chat_id}");
        hmac::sign(&self.0, msg.as_bytes())
    }

    /// The handle of `chat_id` for `conversation`.
    #[must_use]
    pub fn sign(&self, conversation: &str, chat_id: i64) -> String {
        let tag = self.mac(conversation, chat_id);
        format!("{chat_id}.{}", URL_SAFE_NO_PAD.encode(&tag.as_ref()[..SIG_BYTES]))
    }

    /// The chat id of `handle` when it was made for `conversation`.
    #[must_use]
    pub fn verify(&self, conversation: &str, handle: &str) -> Option<i64> {
        let (id, sig) = handle.split_once('.')?;
        let chat_id: i64 = id.parse().ok()?;
        let sig = URL_SAFE_NO_PAD.decode(sig).ok()?;
        if sig.len() != SIG_BYTES {
            return None;
        }
        let tag = self.mac(conversation, chat_id);
        bool::from(tag.as_ref()[..SIG_BYTES].ct_eq(&sig)).then_some(chat_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_works_only_for_its_conversation() {
        let k = Key::new("0123456789abcdef0123456789abcdef");
        let h = k.sign("conv-a", -1_001_234_567_890);
        assert_eq!(k.verify("conv-a", &h), Some(-1_001_234_567_890));
        assert_eq!(k.verify("conv-b", &h), None);
        assert_eq!(k.verify("conv-a", "-1001234567891.AAAAAAAAAAAAAAAAAAAAAA"), None);
        let other = Key::new("another key, also long enough to use");
        assert_eq!(other.verify("conv-a", &h), None);
    }

    #[test]
    fn a_tampered_handle_is_refused() {
        let k = Key::new("0123456789abcdef0123456789abcdef");
        let h = k.sign("c", -10_042);
        let (id, sig) = h.split_once('.').unwrap();
        assert_eq!(k.verify("c", &format!("{}.{sig}", id.replace("042", "043"))), None);
        assert_eq!(k.verify("c", &format!("{id}.{}", &sig[1..])), None);
        assert_eq!(k.verify("c", id), None);
        assert_eq!(k.verify("c", ""), None);
    }
}
