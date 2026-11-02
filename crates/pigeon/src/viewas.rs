//! View as (spec 6): the HMAC token, its checks, and the start route `POST …/view-as` (6.1).
//!
//! Token: `base64url(JSON{a, u, s, e}) + "." + base64url(HMAC-SHA256(key, first part))`, no padding; the first key
//! of `PIGEON_VIEW_AS_KEYS` signs, every key verifies. Tokens are stateless: any replica checks any token.

use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::hmac;
use serde::{Deserialize, Serialize};

use crate::http::{Page, SMALL_BODY, page, query_param, same_origin, small_body};
use crate::identity::{Access, Identity, now_secs};
use crate::paseo::compat;
use crate::{App, KeyList};

/// A token is at most this long (spec 6.2).
pub const MAX_TOKEN: usize = 2048;
/// A view lasts 12 h (spec 6.1).
pub const LIFETIME_SECS: i64 = 43_200;
/// The session id is at most this long.
pub const MAX_SESSION: usize = 64;
const RECORD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The token's claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    /// The admin.
    pub a: String,
    /// The person viewed.
    pub u: String,
    /// The view session (a random UUID, version 4).
    pub s: String,
    /// Expiry, seconds since the epoch.
    pub e: i64,
}

fn mac(key: &[u8; 32], data: &[u8]) -> hmac::Tag {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data)
}

/// Signs `claims` with the first key.
#[must_use]
pub fn sign(keys: &KeyList, claims: &Claims) -> String {
    let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap_or_default());
    let tag = keys.0.first().map(|k| mac(k, body.as_bytes()));
    let sig = tag.map(|t| URL_SAFE_NO_PAD.encode(t.as_ref())).unwrap_or_default();
    format!("{body}.{sig}")
}

/// The claims of a token that passes spec 6.2's first checks (length, shape, MAC under any key, claim types,
/// `e > now`); the identity checks are the caller's.
#[must_use]
pub fn verify(keys: &KeyList, token: &str, now: i64) -> Option<Claims> {
    if token.len() > MAX_TOKEN {
        return None;
    }
    let mut parts = token.split('.');
    let (Some(body), Some(sig), None) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    let sig = URL_SAFE_NO_PAD.decode(sig).ok()?;
    let ok = keys
        .0
        .iter()
        .any(|k| hmac::verify(&hmac::Key::new(hmac::HMAC_SHA256, k), body.as_bytes(), &sig).is_ok());
    if !ok {
        return None;
    }
    let claims: Claims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(body).ok()?).ok()?;
    if claims.s.is_empty() || claims.s.len() > MAX_SESSION || claims.e <= now {
        return None;
    }
    Some(claims)
}

fn timestamp(secs: i64) -> Option<magpie_api::Timestamp> {
    time::OffsetDateTime::from_unix_timestamp(secs)
        .ok()
        .map(magpie_api::Timestamp)
}

/// `POST /api/…/view-as` (spec 6.1): an admin's form from the ops page starts a view. Recorded in magpie first;
/// only then is a token issued, by a 303 to the app with the token in the fragment.
pub async fn start(app: &App, req: Request, id: &Identity) -> Response {
    let admin = match id {
        Identity::Person(p) if p.is_admin() => p.clone(),
        Identity::SignedOut => return page(app, Page::SignedOut, None),
        Identity::Unavailable => return page(app, Page::Unavailable, None),
        Identity::Person(p) => {
            tracing::warn!(person = %aviary_core::log::person_key(&p.email), "view-as start refused");
            return page(app, Page::AdminsOnly, None);
        }
        _ => {
            tracing::warn!("view-as start refused");
            return page(app, Page::AdminsOnly, None);
        }
    };
    if *req.method() != Method::POST {
        return page(app, Page::ReadOnly, None);
    }
    if !same_origin(req.headers(), false) {
        tracing::warn!(person = %aviary_core::log::person_key(&admin.email), "view-as start refused: not same-origin");
        return page(app, Page::AdminsOnly, None);
    }
    let Some(body) = small_body(req.into_body(), SMALL_BODY).await else {
        return page(app, Page::NotAUser, None);
    };
    let form = String::from_utf8_lossy(&body);
    let user = query_param(Some(&form), "user")
        .map(|u| u.trim().to_lowercase())
        .unwrap_or_default();
    if user.is_empty() || user.starts_with('@') || !user.contains('@') {
        return page(app, Page::NotAUser, None);
    }
    match app.0.identity.access(&user).await {
        Some(Access::Allowed(_)) => {}
        Some(Access::NotAllowed) => return page(app, Page::NotAUser, None),
        None => return page(app, Page::Unavailable, None),
    }
    let claims = Claims {
        a: admin.email.clone(),
        u: user.clone(),
        s: uuid::Uuid::new_v4().to_string(),
        e: now_secs() + LIFETIME_SECS,
    };
    let Some(expires_at) = timestamp(claims.e) else {
        return page(app, Page::NotRecorded, None);
    };
    let record = magpie_api::ops::ViewAsRequest {
        admin: claims.a.clone(),
        person: claims.u.clone(),
        session: claims.s.clone(),
        expires_at,
    };
    match tokio::time::timeout(RECORD_TIMEOUT, app.0.magpie.view_as(&record)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "view-as not recorded");
            return page(app, Page::NotRecorded, None);
        }
        Err(_) => {
            tracing::warn!("view-as not recorded: timed out");
            return page(app, Page::NotRecorded, None);
        }
    }
    let token = sign(&app.0.secrets.view_as_keys, &claims);
    tracing::info!(
        admin = %aviary_core::log::person_key(&claims.a),
        viewed = %aviary_core::log::person_key(&claims.u),
        session = %claims.s,
        "view-as started"
    );
    let mut r = StatusCode::SEE_OTHER.into_response();
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&format!("/#{}{token}", compat::VIEW_AS_FRAGMENT)) {
        h.insert(header::LOCATION, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(b: u8) -> KeyList {
        KeyList(vec![[b; 32]])
    }

    fn claims(e: i64) -> Claims {
        Claims {
            a: "admin@example.com".into(),
            u: "staff@example.org".into(),
            s: "4a0b1c2d-0000-4000-8000-000000000000".into(),
            e,
        }
    }

    #[test]
    fn round_trip_and_rotation() {
        let now = 1_000_000;
        let t = sign(&keys(1), &claims(now + 10));
        assert_eq!(verify(&keys(1), &t, now), Some(claims(now + 10)));
        assert_eq!(verify(&keys(2), &t, now), None, "another key");
        let both = KeyList(vec![[2; 32], [1; 32]]);
        assert_eq!(
            verify(&both, &t, now),
            Some(claims(now + 10)),
            "the previous key still verifies"
        );
        assert!(sign(&both, &claims(now + 10)) != t, "the first key signs");
    }

    #[test]
    fn refusals() {
        let now = 1_000_000;
        assert_eq!(verify(&keys(1), &sign(&keys(1), &claims(now)), now), None, "expired");
        let t = sign(&keys(1), &claims(now + 10));
        let (body, sig) = t.split_once('.').unwrap();
        assert_eq!(verify(&keys(1), &format!("{body}x.{sig}"), now), None, "forged body");
        assert_eq!(verify(&keys(1), &format!("{t}.x"), now), None, "three parts");
        assert_eq!(verify(&keys(1), &"a".repeat(MAX_TOKEN + 1), now), None, "too long");
        let mut long = claims(now + 10);
        long.s = "s".repeat(MAX_SESSION + 1);
        assert_eq!(verify(&keys(1), &sign(&keys(1), &long), now), None, "session too long");
        let wrong_types = URL_SAFE_NO_PAD.encode(br#"{"a":"x","u":"y","s":"z","e":"soon"}"#);
        let sig = URL_SAFE_NO_PAD.encode(mac(&[1; 32], wrong_types.as_bytes()).as_ref());
        assert_eq!(
            verify(&keys(1), &format!("{wrong_types}.{sig}"), now),
            None,
            "e not an integer"
        );
    }
}
