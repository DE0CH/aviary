//! The iOS apps' build numbers (`CFBundleVersion`), and the rule App Store Connect holds them to: an upload's number
//! must be above every build of that app it already has ("The bundle version must be higher than the previously
//! uploaded version"). Pure functions; the effects (git, App Store Connect, GitHub) are in `main.rs`.
//!
//! A build is numbered `<commits on main>.<run attempt>`: the gate moves main to the exact commit it tested, so a
//! branch knows the number main's upload will carry, and main's count only grows.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SecureRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair};

/// A `CFBundleVersion`: up to three whole numbers joined by dots, compared number by number (a missing one is 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildNumber(Vec<u64>);

impl BuildNumber {
    /// `None` when `s` is not one to three whole numbers joined by dots.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() > 3
            || parts
                .iter()
                .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
        {
            return None;
        }
        parts
            .iter()
            .map(|p| p.parse().ok())
            .collect::<Option<Vec<u64>>>()
            .map(Self)
    }
}

impl Ord for BuildNumber {
    fn cmp(&self, other: &Self) -> Ordering {
        let len = self.0.len().max(other.0.len());
        let at = |v: &[u64], i: usize| v.get(i).copied().unwrap_or(0);
        (0..len)
            .map(|i| at(&self.0, i).cmp(&at(&other.0, i)))
            .find(|o| o.is_ne())
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for BuildNumber {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for BuildNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<String> = self.0.iter().map(u64::to_string).collect();
        f.write_str(&parts.join("."))
    }
}

/// The build number of a commit with `commits` commits in its history, on the workflow run's `attempt`.
#[must_use]
pub fn build_number(commits: u64, attempt: u64) -> BuildNumber {
    BuildNumber(vec![commits, attempt])
}

/// The highest of an app's build numbers; versions that are no build number are passed over.
pub fn highest<'a>(versions: impl IntoIterator<Item = &'a str>) -> Option<BuildNumber> {
    versions.into_iter().filter_map(BuildNumber::parse).max()
}

/// Why App Store Connect would refuse `ours` for `app`, whose highest build is `held`; `None` when it would take it.
#[must_use]
pub fn refusal(app: &str, ours: &BuildNumber, held: Option<&BuildNumber>) -> Option<String> {
    let held = held.filter(|h| ours <= *h)?;
    Some(format!(
        "{app}: build {ours} is not above {held}, the highest build App Store Connect holds of it, so its upload would \
         be refused (\"The bundle version must be higher than the previously uploaded version\")"
    ))
}

/// The record's place in the `ios-builds` release's body: a JSON object of bundle id to highest build number.
const FENCE: &str = "```json\n";

/// The `ios-builds` release's body: what it is, then the record.
#[must_use]
pub fn record_body(record: &BTreeMap<String, Option<String>>) -> String {
    let json = serde_json::to_string_pretty(record).unwrap_or_else(|_| "{}".into());
    format!(
        "App Store Connect's highest build number of each iOS app, kept by .github/workflows/ios-builds.yml \
         (testing/ios-build-number) and read by every branch's check that its build number is above them.\n\n\
         {FENCE}{json}\n```\n"
    )
}

/// The record in an `ios-builds` release's body.
///
/// # Errors
/// The body holds no record, or one that is not a JSON object of bundle id to build number.
pub fn parse_record(body: &str) -> Result<BTreeMap<String, Option<String>>> {
    let start = body.find(FENCE).context("no record in the release's body")? + FENCE.len();
    let end = body[start..]
        .find("\n```")
        .context("the record in the release's body is not closed")?
        + start;
    serde_json::from_str(&body[start..end]).context("the record in the release's body")
}

/// An App Store Connect API token (ES256), good for 20 minutes from `now` (seconds since 1970), signed with the
/// key's PKCS#8 PEM (the `.p8` App Store Connect gives).
///
/// # Errors
/// The PEM is no P-256 PKCS#8 key, or signing fails.
pub fn token(key_id: &str, issuer: &str, now: u64, pem: &str, rng: &dyn SecureRandom) -> Result<String> {
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).map(str::trim).collect();
    let der = STANDARD.decode(b64).context("the key's PEM")?;
    let Ok(pair) = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &der, rng) else {
        bail!("the key is no P-256 PKCS#8 key");
    };
    let part = |v: serde_json::Value| URL_SAFE_NO_PAD.encode(v.to_string());
    let header = part(serde_json::json!({"alg": "ES256", "kid": key_id, "typ": "JWT"}));
    let claims = part(serde_json::json!({"iss": issuer, "iat": now, "exp": now + 1200, "aud": "appstoreconnect-v1"}));
    let signed = format!("{header}.{claims}");
    let Ok(signature) = pair.sign(rng, signed.as_bytes()) else {
        bail!("signing the token");
    };
    Ok(format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{ECDSA_P256_SHA256_FIXED, KeyPair as _, UnparsedPublicKey};

    fn n(s: &str) -> BuildNumber {
        BuildNumber::parse(s).unwrap()
    }

    #[test]
    fn build_numbers_compare_number_by_number() {
        assert!(n("121.1") > n("96.1"));
        assert!(n("96.2") > n("96.1"));
        assert!(n("100") > n("99.9.9"));
        assert!(n("1.10") > n("1.9")); // not as text
        assert_eq!(n("96").cmp(&n("96.0")), Ordering::Equal);
        assert_eq!(n("36819.1").to_string(), "36819.1");
    }

    #[test]
    fn only_one_to_three_whole_numbers_are_build_numbers() {
        for bad in ["", "1.", ".1", "a", "1.2.3.4", "-1", "1 .2"] {
            assert_eq!(BuildNumber::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_build_is_numbered_by_its_commits_then_its_attempt() {
        assert_eq!(build_number(36819, 1).to_string(), "36819.1");
        assert!(build_number(36820, 1) > build_number(36819, 2));
    }

    #[test]
    fn the_highest_build_passes_over_what_is_no_build_number() {
        assert_eq!(highest(["20.1", "96.1", "18.1", "beta"]), Some(n("96.1")));
        assert_eq!(highest([]), None);
    }

    #[test]
    fn a_number_at_or_below_the_highest_held_is_refused() {
        // run 20 of aviary's app-ios, after DE0CH/claudecodeui's yyc-ios had uploaded 96.1 (2026-11-26)
        let said = refusal("dev.de0ch.yycai", &n("20.1"), Some(&n("96.1"))).unwrap();
        assert!(
            said.contains("20.1") && said.contains("96.1") && said.contains("dev.de0ch.yycai"),
            "{said}"
        );
        assert!(refusal("dev.de0ch.yycai", &n("96.1"), Some(&n("96.1"))).is_some());
        assert_eq!(refusal("dev.de0ch.yycai", &n("36819.1"), Some(&n("96.1"))), None);
        assert_eq!(refusal("dev.de0ch.jarvis4chat", &n("36819.1"), None), None);
    }

    #[test]
    fn the_record_reads_back_from_the_release_body() {
        let record = BTreeMap::from([
            ("dev.de0ch.yycai".to_string(), Some("96.1".to_string())),
            ("dev.de0ch.jarvis4chat".to_string(), None),
        ]);
        assert_eq!(parse_record(&record_body(&record)).unwrap(), record);
        assert!(parse_record("no record here").is_err());
    }

    #[test]
    fn the_token_is_an_es256_jwt_for_app_store_connect() {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            STANDARD.encode(pkcs8.as_ref())
        );
        let jwt = token("KEY123", "issuer-uuid", 1_000_000, &pem, &rng).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let json =
            |p: &str| -> serde_json::Value { serde_json::from_slice(&URL_SAFE_NO_PAD.decode(p).unwrap()).unwrap() };
        assert_eq!(
            json(parts[0]),
            serde_json::json!({"alg": "ES256", "kid": "KEY123", "typ": "JWT"})
        );
        assert_eq!(
            json(parts[1]),
            serde_json::json!({"iss": "issuer-uuid", "iat": 1_000_000, "exp": 1_001_200, "aud": "appstoreconnect-v1"})
        );
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap();
        let signed = format!("{}.{}", parts[0], parts[1]);
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, pair.public_key().as_ref())
            .verify(signed.as_bytes(), &URL_SAFE_NO_PAD.decode(parts[2]).unwrap())
            .unwrap();
    }
}
