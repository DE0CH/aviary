//! Property tests of the `hst1.` codec (hermit 6.1): sign then verify gives the claims back; any change to a signed
//! token, a wrong key, an expired time or another source address is refused.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{IpAddr, Ipv4Addr};

use hermit_api::token::{Claims, KeySet, MasterKey, TokenKeys, VerifyError, peek_kid};
use proptest::prelude::*;
use uuid::Uuid;

fn claims() -> impl Strategy<Value = Claims> {
    (
        any::<u128>(),
        any::<u128>(),
        any::<u32>(),
        1u64..u64::MAX,
        "[a-z0-9.+-]{1,60}@[a-z0-9.-]{1,60}",
    )
        .prop_map(|(s, c, ip, exp, person)| Claims {
            sandbox_id: Uuid::from_u128(s),
            conversation_id: Uuid::from_u128(c),
            source: Ipv4Addr::from(ip),
            expires_at: exp,
            person,
        })
}

fn set_for(keys: &[MasterKey]) -> KeySet {
    KeySet::from_keys(&TokenKeys {
        keys: keys.iter().map(|k| k.signing_key().unwrap().public_key()).collect(),
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn sign_verify_round_trip(c in claims(), kid in 1u32..1000, secret in any::<[u8; 32]>()) {
        let key = MasterKey::new(kid, secret);
        let token = key.signing_key().unwrap().sign(&c).unwrap();
        prop_assert!(token.is_ascii() && !token.contains([':', '@', '/', '%']), "userinfo safe");
        prop_assert_eq!(peek_kid(&token), Ok(kid));
        let set = set_for(&[key]);
        let from = IpAddr::V4(c.source);
        prop_assert_eq!(set.verify(&token, c.expires_at - 1, from), Ok(c.clone()));
        prop_assert_eq!(set.verify(&token, c.expires_at, from), Err(VerifyError::Expired));
    }

    #[test]
    fn any_tampering_is_refused(c in claims(), secret in any::<[u8; 32]>(), at in any::<prop::sample::Index>(), flip in 1u8..=255) {
        let key = MasterKey::new(1, secret);
        let token = key.signing_key().unwrap().sign(&c).unwrap();
        let mut bytes = token.into_bytes();
        let i = at.index(bytes.len());
        bytes[i] ^= flip;
        if let Ok(tampered) = String::from_utf8(bytes) {
            let r = set_for(&[key]).verify(&tampered, 0, IpAddr::V4(c.source));
            prop_assert!(r.is_err(), "tampered at {} still verifies", i);
        }
    }

    #[test]
    fn wrong_key_or_unknown_kid(c in claims(), a in any::<[u8; 32]>(), b in any::<[u8; 32]>()) {
        prop_assume!(a != b);
        let token = MasterKey::new(1, a).signing_key().unwrap().sign(&c).unwrap();
        let from = IpAddr::V4(c.source);
        prop_assert_eq!(set_for(&[MasterKey::new(1, b)]).verify(&token, 0, from), Err(VerifyError::BadSignature));
        prop_assert_eq!(set_for(&[MasterKey::new(2, a)]).verify(&token, 0, from), Err(VerifyError::UnknownKid(1)));
    }

    #[test]
    fn source_address_binding(c in claims(), other in any::<u32>(), secret in any::<[u8; 32]>()) {
        let key = MasterKey::new(1, secret);
        let token = key.signing_key().unwrap().sign(&c).unwrap();
        let set = set_for(&[key]);
        let other = Ipv4Addr::from(other);
        let r = set.verify(&token, 0, IpAddr::V4(other));
        if c.source.is_unspecified() || other == c.source {
            prop_assert!(r.is_ok());
        } else {
            prop_assert_eq!(r, Err(VerifyError::WrongSource));
        }
        // the IPv4-mapped IPv6 form of the right address passes
        prop_assert!(set.verify(&token, 0, IpAddr::V6(c.source.to_ipv6_mapped())).is_ok());
    }

    #[test]
    fn random_strings_never_verify(s in "\\PC{0,300}") {
        let set = set_for(&[MasterKey::new(1, [7; 32])]);
        prop_assert!(set.verify(&s, 0, IpAddr::V4(Ipv4Addr::UNSPECIFIED)).is_err());
    }
}
