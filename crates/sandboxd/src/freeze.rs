//! What a freeze or a thaw is handed (jarvis4 docs/PLAN-fabric.md, "How a frozen image travels"): where the frozen
//! sandbox goes in S3, credentials for that one object and nothing else, and the freeze's own key. The memory is
//! `<object>`, the scratch file `<object>.scratch`, both sealed (seal.rs).
//!
//!   POST /v1/sandboxes/{name}/freeze {object, region, accessKeyId, secretAccessKey, sessionToken, key}
//!   POST /v1/sandboxes/{name}/thaw   {spec: {…}, frozen: {object, region, accessKeyId, secretAccessKey, sessionToken, key}}

use crate::err;
use crate::fixed::Str;
use crate::json::{self, Val};
use crate::util::{Error, Res};
use core::fmt::Write as _;

/// An S3 object's URL, `s3://<bucket>/<key>`.
pub type Object = Str<512>;

#[derive(Clone, Copy)]
pub struct Transfer {
    pub object: Object,
    pub region: Str<32>,
    pub access_key_id: Str<128>,
    pub secret_access_key: Str<128>,
    /// STS session tokens run to a couple of thousand characters.
    pub session_token: Str<4096>,
    /// The freeze's key: 64 hex digits.
    pub key: Str<64>,
}

fn safe_object(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("s3://") else { return false };
    let Some((bucket, key)) = rest.split_once('/') else { return false };
    !bucket.is_empty()
        && bucket.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'.')
        && !key.is_empty()
        && !key.contains("..")
        && key.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_./".contains(&c))
}

fn plain(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_graphic())
}

impl Transfer {
    /// From the request's JSON object; a 400 for anything missing or malformed.
    pub fn parse(o: &[u8]) -> Res<Transfer> {
        let mut t = Transfer {
            object: Str::new(),
            region: Str::new(),
            access_key_id: Str::new(),
            secret_access_key: Str::new(),
            session_token: Str::new(),
            key: Str::new(),
        };
        let mut tmp = [0u8; 4096];
        json::object::<Error>(o, |k, v| {
            let Val::Str(s) = v else { return Ok(()) };
            let text = s.decode(&mut tmp).ok_or_else(|| Error::new(400, "a value is too long"))?;
            let fits = if k.is("object") {
                t.object.push(text)
            } else if k.is("region") {
                t.region.push(text)
            } else if k.is("accessKeyId") {
                t.access_key_id.push(text)
            } else if k.is("secretAccessKey") {
                t.secret_access_key.push(text)
            } else if k.is("sessionToken") {
                t.session_token.push(text)
            } else if k.is("key") {
                t.key.push(text)
            } else {
                true
            };
            if fits { Ok(()) } else { Err(Error::new(400, "a value is too long")) }
        })?;
        if !safe_object(t.object.as_str()) {
            return Err(Error::new(400, "object must be s3://<bucket>/<key> (letters, digits, - _ . /)"));
        }
        if t.region.is_empty()
            || !t.region.as_bytes().iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        {
            return Err(Error::new(400, "bad region"));
        }
        for (v, what) in [
            (t.access_key_id.as_str(), "accessKeyId"),
            (t.secret_access_key.as_str(), "secretAccessKey"),
            (t.session_token.as_str(), "sessionToken"),
        ] {
            if !plain(v) {
                return Err(err!(400, "{what} is missing or not plain text"));
            }
        }
        if t.key.len() != 64 || !t.key.as_bytes().iter().all(u8::is_ascii_hexdigit) {
            return Err(Error::new(400, "key must be 64 hex digits"));
        }
        Ok(t)
    }

    /// The scratch file's object: `<object>.scratch`.
    pub fn scratch_object(&self) -> Object {
        let mut o = self.object;
        let _ = o.push(".scratch");
        o
    }

    /// The `s3-put`/`s3-get` stage's environment (the credentials go to that stage only).
    pub fn aws_env(&self) -> [Str<4160>; 4] {
        let mut e = [Str::<4160>::new(); 4];
        let _ = write!(e[0], "AWS_ACCESS_KEY_ID={}", self.access_key_id.as_str());
        let _ = write!(e[1], "AWS_SECRET_ACCESS_KEY={}", self.secret_access_key.as_str());
        let _ = write!(e[2], "AWS_SESSION_TOKEN={}", self.session_token.as_str());
        let _ = write!(e[3], "AWS_DEFAULT_REGION={}", self.region.as_str());
        e
    }

    /// The `seal`/`open` stage's environment (the key goes to that stage only).
    pub fn seal_env(&self) -> Str<96> {
        let mut e = Str::<96>::new();
        let _ = write!(e, "SBX_SEAL_KEY={}", self.key.as_str());
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(object: &str, key: &str) -> String {
        format!(
            r#"{{"object":"{object}","region":"eu-central-1","accessKeyId":"ASIAEXAMPLE","secretAccessKey":"s3cr3t/+x","sessionToken":"{}","key":"{key}"}}"#,
            "T".repeat(2000)
        )
    }

    #[test]
    fn a_transfer_names_one_object_and_hands_each_stage_only_its_own_secret() {
        let k = "ab".repeat(32);
        let b = body("s3://jarvis4-fabric-images-795109470764/frozen/sbx-1/01a1", &k);
        let t = Transfer::parse(b.as_bytes()).ok().unwrap();
        assert_eq!(t.scratch_object().as_str(), "s3://jarvis4-fabric-images-795109470764/frozen/sbx-1/01a1.scratch");
        let aws = t.aws_env();
        assert_eq!(aws[0].as_str(), "AWS_ACCESS_KEY_ID=ASIAEXAMPLE");
        assert!(aws[2].as_str().starts_with("AWS_SESSION_TOKEN=TTTT") && aws[2].len() == 18 + 2000);
        assert_eq!(aws[3].as_str(), "AWS_DEFAULT_REGION=eu-central-1");
        assert!(aws.iter().all(|e| !e.as_str().contains(&k)), "the key never reaches aws");
        assert_eq!(t.seal_env().as_str(), format!("SBX_SEAL_KEY={k}"));
    }

    #[test]
    fn a_malformed_transfer_is_refused() {
        let k = "ab".repeat(32);
        for bad in ["http://x/y", "s3://Bucket/k", "s3://b/", "s3://b/../etc", "s3://b/k;rm", "s3://b"] {
            assert!(Transfer::parse(body(bad, &k).as_bytes()).is_err(), "{bad}");
        }
        assert!(Transfer::parse(body("s3://b/k", "short").as_bytes()).is_err());
        assert!(Transfer::parse(body("s3://b/k", &"zz".repeat(32)).as_bytes()).is_err());
        assert!(Transfer::parse(br#"{"object":"s3://b/k","key":"x"}"#).is_err(), "no credentials");
    }
}
