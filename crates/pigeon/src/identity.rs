//! Identity (spec 4): deployment facts from hermit, Cloudflare Access tokens, who may use the deployment, first
//! names, and the request identity of 4.6.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aviary_core::ops::Gate;
use axum::http::{HeaderMap, header};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hermit_api::deployment::Deployment;
use hermit_api::people::Role;
use ring::signature::{self, RsaPublicKeyComponents};
use serde::Deserialize;
use serde_json::Value;

use crate::metrics::Metrics;
use crate::viewas;
use crate::{KeyList, Tuning};

/// A token longer than this is "no token" (spec 4.2).
pub const MAX_TOKEN: usize = 8 * 1024;
/// Leeway on `exp` and `nbf` (spec 4.3).
pub const LEEWAY_SECS: i64 = 5;
/// The cookie Cloudflare Access sets.
pub const ACCESS_COOKIE: &str = "CF_Authorization";
/// The header Cloudflare sets on Access-covered paths.
pub const ASSERTION_HEADER: &str = "cf-access-jwt-assertion";
/// The header the iOS app sends.
pub const APP_TOKEN_HEADER: &str = "cf-access-token";
/// A first name is at most this many characters (spec 4.5).
pub const FIRST_NAME_MAX: usize = 40;

const JWKS_PATH: &str = "/cdn-cgi/access/certs";
const GET_IDENTITY_PATH: &str = "/cdn-cgi/access/get-identity";
const HERMIT_PEOPLE_TIMEOUT: Duration = Duration::from_secs(5);
const HERMIT_FACTS_TIMEOUT: Duration = Duration::from_secs(5);
const JWKS_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest a lookup waits for a fetch in flight (the request and the body each have [`JWKS_TIMEOUT`]).
const FETCH_WAIT: Duration = Duration::from_secs(11);
const GET_IDENTITY_TIMEOUT: Duration = Duration::from_millis(1500);
const TOKEN_CACHE_ENTRIES: u64 = 10_000;
const PEOPLE_ENTRIES: u64 = 10_000;
const NAMES_ENTRIES: u64 = 10_000;

/// Where to look for the Access token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sources {
    /// The assertion header, then the cookie, then `cf-access-token` (every `/api`, `/paseo-ws` and `/ops` path).
    All,
    /// The cookie only (page paths: the edge sends no assertion there).
    CookieOnly,
}

/// The token of a request, from the first non-empty source (spec 4.2); `None` when absent or over 8 KiB.
#[must_use]
pub fn token_of(headers: &HeaderMap, sources: Sources) -> Option<String> {
    let header_value = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let found = match sources {
        Sources::All => header_value(ASSERTION_HEADER)
            .or_else(|| cookie(headers, ACCESS_COOKIE))
            .or_else(|| header_value(APP_TOKEN_HEADER)),
        Sources::CookieOnly => cookie(headers, ACCESS_COOKIE),
    };
    found.filter(|t| t.len() <= MAX_TOKEN)
}

/// A cookie's value, URI-decoded.
#[must_use]
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    for v in headers.get_all(header::COOKIE) {
        let Ok(v) = v.to_str() else { continue };
        for part in v.split(';') {
            if let Some((k, val)) = part.trim().split_once('=')
                && k.trim() == name
            {
                let val = percent_decode(val.trim());
                if !val.is_empty() {
                    return Some(val);
                }
            }
        }
    }
    None
}

/// Percent-decodes `s` once (invalid escapes are kept as they are; the result is lossy UTF-8).
#[must_use]
pub fn percent_decode(s: &str) -> String {
    String::from_utf8_lossy(&percent_decode_bytes(s.as_bytes())).into_owned()
}

/// Percent-decodes bytes once.
#[must_use]
pub fn percent_decode_bytes(b: &[u8]) -> Vec<u8> {
    let hex = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Seconds since the Unix epoch.
#[must_use]
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// A verified Access token (spec 4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    /// The `email` claim, trimmed and lowercased.
    pub email: String,
    /// The team domain whose key signed it.
    pub team: String,
    /// `exp`, seconds.
    pub exp: i64,
    /// `given_name`.
    pub given_name: Option<String>,
    /// `name`.
    pub name: Option<String>,
}

/// Why a token was refused (logged, never shown).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Not three base64url parts of JSON.
    Malformed,
    /// `alg` is not RS256 or `kid` is missing.
    Algorithm,
    /// `iss` is not a team the deployment names.
    Issuer,
    /// No key with that `kid`.
    UnknownKey,
    /// The signature does not verify.
    Signature,
    /// `aud` does not contain the team's audience.
    Audience,
    /// Past `exp` or before `nbf`.
    Time,
    /// No `email` (a service token).
    NoEmail,
    /// The deployment facts are not loaded yet.
    NoFacts,
}

impl Refused {
    /// The metrics label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::Algorithm => "algorithm",
            Self::Issuer => "issuer",
            Self::UnknownKey => "unknown_key",
            Self::Signature => "signature",
            Self::Audience => "audience",
            Self::Time => "time",
            Self::NoEmail => "no_email",
            Self::NoFacts => "no_facts",
        }
    }
}

#[derive(Deserialize)]
struct JwtHeader {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
}

#[derive(Deserialize)]
struct Jwk {
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    kty: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
}

#[derive(Deserialize)]
struct JwkSet {
    #[serde(default)]
    keys: Vec<Jwk>,
}

#[derive(Clone)]
struct PublicKey {
    n: Vec<u8>,
    e: Vec<u8>,
}

/// One team's keys and their fetch bookkeeping (spec 4.3). At most one fetch runs per team at a time; a lookup that
/// needs the fetch in flight waits for it.
struct Team {
    st: Mutex<TeamState>,
    /// Bumped when a fetch ends, whatever its outcome.
    done: tokio::sync::watch::Sender<u64>,
}

#[derive(Default)]
struct TeamState {
    keys: HashMap<String, PublicKey>,
    /// When the keys were last fetched (`None`: never).
    fetched: Option<Instant>,
    /// When the last fetch started, whatever its outcome.
    last_attempt: Option<Instant>,
    /// When the last fetch for an unknown kid of a loaded set started.
    last_kid_fetch: Option<Instant>,
    /// A fetch is running.
    in_flight: bool,
}

impl TeamState {
    /// Whether a refresh may start: none is running, and the last attempt succeeded or is a cooldown old (a failing
    /// certs endpoint is asked at most once per cooldown).
    fn may_retry(&self, cooldown: Duration) -> bool {
        !self.in_flight
            && self
                .last_attempt
                .is_none_or(|a| self.fetched.is_some_and(|f| f >= a) || a.elapsed() >= cooldown)
    }

    fn begin(&mut self) {
        self.in_flight = true;
        self.last_attempt = Some(Instant::now());
    }
}

/// The person's answer from hermit (spec 4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// May use the deployment in this role.
    Allowed(Role),
    /// May not.
    NotAllowed,
}

/// A signed-in person (spec 4.6 `Person`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    /// The verified email.
    pub email: String,
    /// The role hermit gave.
    pub role: Role,
    /// The token's `exp`.
    pub exp: i64,
    /// The token's team.
    pub team: String,
    /// The raw token (for app sign-in and get-identity; never logged).
    pub token: String,
    /// `given_name` / `name` claims.
    pub given_name: Option<String>,
    /// `name`.
    pub name: Option<String>,
}

impl Person {
    /// Whether the person is an admin.
    #[must_use]
    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }
}

/// A view-as identity (spec 4.6 `ViewAs`): an admin acting as a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewAs {
    /// The admin (the caller).
    pub admin: Person,
    /// The person viewed.
    pub person: String,
    /// The view session.
    pub session: String,
    /// When the view ends, seconds.
    pub exp: i64,
}

/// The request identity (spec 4.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// No token, or the token failed verification.
    SignedOut,
    /// Verified, hermit says not allowed.
    NotAllowed(String),
    /// Verified and allowed.
    Person(Person),
    /// An admin with a valid view-as token.
    ViewAs(ViewAs),
    /// A view-as token was present but not honoured.
    ViewAsRefused,
    /// hermit unreachable and nothing cached.
    Unavailable,
}

impl Identity {
    /// The email whose data the request reads or writes (`Person` or the viewed person).
    #[must_use]
    pub fn subject(&self) -> Option<&str> {
        match self {
            Self::Person(p) => Some(&p.email),
            Self::ViewAs(v) => Some(&v.person),
            _ => None,
        }
    }

    /// The metrics label.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::SignedOut => "signed_out",
            Self::NotAllowed(_) => "not_allowed",
            Self::Person(_) => "person",
            Self::ViewAs(_) => "view_as",
            Self::ViewAsRefused => "view_as_refused",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone)]
struct PeopleEntry {
    access: Access,
    at: Instant,
}

#[derive(Clone)]
struct NameEntry {
    name: Option<String>,
    until: Instant,
}

/// Facts, keys, people and names: everything 4.1-4.6 caches. Cheap to clone.
#[derive(Clone)]
pub struct Identities(Arc<IdInner>);

struct IdInner {
    hermit: hermit_api::Client,
    http: reqwest::Client,
    access_origin: Arc<dyn Fn(&str) -> String + Send + Sync>,
    tuning: Tuning,
    view_as_keys: KeyList,
    /// The switch window's book of the newest verified token per person, and the old view-as key (WP-50).
    switch: crate::legacy::tokens::Book,
    metrics: Metrics,
    facts: RwLock<Option<Arc<Deployment>>>,
    /// Per team (only teams the facts name get an entry).
    teams: Mutex<HashMap<String, Arc<Team>>>,
    tokens: moka::future::Cache<[u8; 32], (Verified, Instant)>,
    people: moka::future::Cache<String, PeopleEntry>,
    names: moka::future::Cache<String, NameEntry>,
}

impl std::fmt::Debug for Identities {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identities").finish_non_exhaustive()
    }
}

fn decode_part(part: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(part.trim_end_matches('=')).ok()
}

impl Identities {
    pub(crate) fn new(
        hermit: hermit_api::Client,
        http: reqwest::Client,
        access_origin: Arc<dyn Fn(&str) -> String + Send + Sync>,
        tuning: Tuning,
        view_as_keys: KeyList,
        metrics: Metrics,
        switch: crate::legacy::tokens::Book,
    ) -> Self {
        let tokens = moka::future::Cache::builder()
            .max_capacity(TOKEN_CACHE_ENTRIES)
            .time_to_live(tuning.token_cache)
            .build();
        let people = moka::future::Cache::builder()
            .max_capacity(PEOPLE_ENTRIES)
            .time_to_live(tuning.people_fresh + tuning.people_stale)
            .build();
        let names = moka::future::Cache::builder()
            .max_capacity(NAMES_ENTRIES)
            .time_to_live(tuning.first_name_ok.max(tuning.first_name_err))
            .build();
        Self(Arc::new(IdInner {
            hermit,
            http,
            access_origin,
            tuning,
            view_as_keys,
            metrics,
            switch,
            facts: RwLock::new(None),
            teams: Mutex::new(HashMap::new()),
            tokens,
            people,
            names,
        }))
    }

    /// The deployment facts, once loaded.
    #[must_use]
    pub fn facts(&self) -> Option<Arc<Deployment>> {
        self.0.facts.read().ok().and_then(|f| f.clone())
    }

    /// Whether the locale selects the Traditional Chinese (HK) strings (spec 4.1: `zh*`).
    #[must_use]
    pub fn is_zh(&self) -> bool {
        self.facts()
            .is_some_and(|f| f.product.locale.to_ascii_lowercase().starts_with("zh"))
    }

    /// The product name ("" before the facts load).
    #[must_use]
    pub fn product_name(&self) -> String {
        self.facts().map(|f| f.product.name.clone()).unwrap_or_default()
    }

    pub(crate) fn start(&self, facts_gate: Gate, jwks_gate: Gate) {
        let me = self.clone();
        tokio::spawn(async move { me.facts_loop(facts_gate, jwks_gate).await });
    }

    /// Reads H1 once; keeps the last good copy on failure (spec 4.1).
    ///
    /// # Errors
    /// When hermit did not answer with the facts.
    pub async fn refresh_facts(&self) -> Result<Arc<Deployment>, String> {
        match tokio::time::timeout(HERMIT_FACTS_TIMEOUT, self.0.hermit.deployment()).await {
            Ok(Ok(d)) => {
                let d = Arc::new(d);
                if let Ok(mut f) = self.0.facts.write() {
                    *f = Some(d.clone());
                }
                Ok(d)
            }
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("timed out".into()),
        }
    }

    async fn facts_loop(self, facts_gate: Gate, jwks_gate: Gate) {
        let mut facts_gate = Some(facts_gate);
        let mut jwks_gate = Some(jwks_gate);
        loop {
            let ok = match self.refresh_facts().await {
                Ok(_) => {
                    if let Some(g) = facts_gate.take() {
                        g.open();
                        tracing::info!("deployment facts loaded");
                    }
                    true
                }
                Err(e) => {
                    tracing::warn!(error = %e, "deployment facts not refreshed");
                    false
                }
            };
            // every refresh loads the keys of teams that have none yet (a team hermit adds later, spec 4.1)
            if ok
                && self.load_all_jwks().await
                && let Some(g) = jwks_gate.take()
            {
                g.open();
                tracing::info!("access keys loaded");
            }
            let wait = if facts_gate.is_some() || jwks_gate.is_some() {
                self.0.tuning.start_retry
            } else {
                self.0.tuning.facts_every
            };
            tokio::time::sleep(wait).await;
        }
    }

    /// `domain`'s entry (created on first use).
    fn team(&self, domain: &str) -> Option<Arc<Team>> {
        let mut t = self.0.teams.lock().ok()?;
        Some(
            t.entry(domain.to_owned())
                .or_insert_with(|| {
                    Arc::new(Team {
                        st: Mutex::new(TeamState::default()),
                        done: tokio::sync::watch::channel(0).0,
                    })
                })
                .clone(),
        )
    }

    /// Loads every team's JWKS that is not loaded yet; true when every team has keys.
    async fn load_all_jwks(&self) -> bool {
        let Some(facts) = self.facts() else { return false };
        let mut all = true;
        for app in &facts.sign_in.access {
            let Some(team) = self.team(&app.team_domain) else {
                return false;
            };
            let start = match team.st.lock() {
                Ok(mut s) if s.fetched.is_none() && !s.in_flight => {
                    s.begin();
                    true
                }
                Ok(s) => {
                    all &= s.fetched.is_some();
                    false
                }
                Err(_) => return false,
            };
            if start && self.fetch_jwks(&app.team_domain, &team).await.is_err() {
                all = false;
            }
        }
        all
    }

    /// Runs a fetch [`TeamState::begin`] has marked, in its own task (a dropped request never leaves it marked).
    fn spawn_fetch(&self, domain: &str, team: &Arc<Team>) {
        let me = self.clone();
        let domain = domain.to_owned();
        let team = team.clone();
        tokio::spawn(async move {
            let _ = me.fetch_jwks(&domain, &team).await;
        });
    }

    /// Fetches `domain`'s keys after [`TeamState::begin`]; publishes them on success (a failure keeps the cached
    /// set), ends the in-flight mark and wakes the waiters whatever the outcome.
    async fn fetch_jwks(&self, domain: &str, team: &Team) -> Result<(), String> {
        let got = self.download_jwks(domain).await;
        let outcome = match team.st.lock() {
            Ok(mut s) => {
                s.in_flight = false;
                got.map(|keys| {
                    s.keys = keys;
                    s.fetched = Some(Instant::now());
                })
            }
            Err(_) => Err("poisoned".into()),
        };
        team.done.send_modify(|g| *g = g.wrapping_add(1));
        match &outcome {
            Ok(()) => self.0.metrics.jwks_fetch(domain),
            Err(e) => tracing::warn!(team = %domain, error = %e, "access keys not fetched"),
        }
        outcome
    }

    async fn download_jwks(&self, domain: &str) -> Result<HashMap<String, PublicKey>, String> {
        let url = format!("{}{JWKS_PATH}", (self.0.access_origin)(domain));
        let res = tokio::time::timeout(JWKS_TIMEOUT, self.0.http.get(url).send())
            .await
            .map_err(|_| "timed out".to_owned())?
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("status {}", res.status()));
        }
        let set: JwkSet = tokio::time::timeout(JWKS_TIMEOUT, res.json())
            .await
            .map_err(|_| "timed out".to_owned())?
            .map_err(|e| e.to_string())?;
        let mut keys = HashMap::new();
        for k in set.keys {
            if k.kty.as_deref() != Some("RSA") {
                continue;
            }
            let (Some(kid), Some(n), Some(e)) = (k.kid, k.n, k.e) else {
                continue;
            };
            let (Some(n), Some(e)) = (decode_part(&n), decode_part(&e)) else {
                continue;
            };
            keys.insert(kid, PublicKey { n, e });
        }
        Ok(keys)
    }

    /// The key `kid` of `team` (spec 4.3). A cached key is used at once; a stale set starts one background refresh
    /// (none while one runs or a failed one is inside the cooldown). An unknown kid waits for the fetch in flight,
    /// or starts one when the cooldown allows (for a team with no keys yet: since its last attempt), then looks
    /// again; otherwise it is refused at once.
    async fn key(&self, domain: &str, kid: &str) -> Option<PublicKey> {
        enum Next {
            Have(PublicKey),
            Wait(tokio::sync::watch::Receiver<u64>),
            Refuse,
        }
        let team = self.team(domain)?;
        let cooldown = self.0.tuning.jwks_cooldown;
        let next = {
            let mut s = team.st.lock().ok()?;
            if let Some(key) = s.keys.get(kid).cloned() {
                let stale = s.fetched.is_none_or(|f| f.elapsed() >= self.0.tuning.jwks_ttl);
                if stale && s.may_retry(cooldown) {
                    s.begin();
                    self.spawn_fetch(domain, &team);
                }
                Next::Have(key)
            } else if s.in_flight {
                // subscribed under the lock: the fetch's end is seen even if it ends right now
                Next::Wait(team.done.subscribe())
            } else {
                let allowed = if s.fetched.is_some() {
                    s.last_kid_fetch.is_none_or(|a| a.elapsed() >= cooldown)
                } else {
                    s.may_retry(cooldown)
                };
                if allowed {
                    s.begin();
                    if s.fetched.is_some() {
                        s.last_kid_fetch = s.last_attempt;
                    }
                    let rx = team.done.subscribe();
                    self.spawn_fetch(domain, &team);
                    Next::Wait(rx)
                } else {
                    Next::Refuse
                }
            }
        };
        match next {
            Next::Have(key) => Some(key),
            Next::Refuse => None,
            Next::Wait(mut rx) => {
                let _ = tokio::time::timeout(FETCH_WAIT, rx.changed()).await;
                team.st.lock().ok()?.keys.get(kid).cloned()
            }
        }
    }

    /// Verifies a token (spec 4.3), through the verified-token cache.
    ///
    /// # Errors
    /// Why the token is refused.
    pub async fn verify(&self, token: &str) -> Result<Verified, Refused> {
        let digest: [u8; 32] = ring::digest::digest(&ring::digest::SHA256, token.as_bytes())
            .as_ref()
            .try_into()
            .map_err(|_| Refused::Malformed)?;
        if let Some((v, _)) = self.0.tokens.get(&digest).await
            && v.exp + LEEWAY_SECS >= now_secs()
        {
            return Ok(v);
        }
        let v = self.verify_uncached(token).await?;
        self.0.tokens.insert(digest, (v.clone(), Instant::now())).await;
        Ok(v)
    }

    async fn verify_uncached(&self, token: &str) -> Result<Verified, Refused> {
        let mut parts = token.split('.');
        let (Some(h), Some(p), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
            return Err(Refused::Malformed);
        };
        let header: JwtHeader =
            serde_json::from_slice(&decode_part(h).ok_or(Refused::Malformed)?).map_err(|_| Refused::Malformed)?;
        let claims: Value =
            serde_json::from_slice(&decode_part(p).ok_or(Refused::Malformed)?).map_err(|_| Refused::Malformed)?;
        let sig = decode_part(s).ok_or(Refused::Malformed)?;
        if header.alg != "RS256" {
            return Err(Refused::Algorithm);
        }
        let kid = header.kid.filter(|k| !k.is_empty()).ok_or(Refused::Algorithm)?;
        let facts = self.facts().ok_or(Refused::NoFacts)?;
        let iss = claims.get("iss").and_then(Value::as_str).ok_or(Refused::Issuer)?;
        let app = facts
            .sign_in
            .access
            .iter()
            .find(|a| iss == format!("https://{}", a.team_domain))
            .ok_or(Refused::Issuer)?;
        let key = self.key(&app.team_domain, &kid).await.ok_or(Refused::UnknownKey)?;
        let message = &token[..h.len() + 1 + p.len()];
        RsaPublicKeyComponents { n: &key.n, e: &key.e }
            .verify(&signature::RSA_PKCS1_2048_8192_SHA256, message.as_bytes(), &sig)
            .map_err(|_| Refused::Signature)?;
        let aud_ok = match claims.get("aud") {
            Some(Value::String(a)) => *a == app.audience,
            Some(Value::Array(list)) => list.iter().any(|a| a.as_str() == Some(app.audience.as_str())),
            _ => false,
        };
        if !aud_ok {
            return Err(Refused::Audience);
        }
        let now = now_secs();
        let exp = claims.get("exp").and_then(Value::as_i64).ok_or(Refused::Time)?;
        if exp + LEEWAY_SECS < now {
            return Err(Refused::Time);
        }
        if let Some(nbf) = claims.get("nbf") {
            let nbf = nbf.as_i64().ok_or(Refused::Time)?;
            if nbf - LEEWAY_SECS > now {
                return Err(Refused::Time);
            }
        }
        let email = claims
            .get("email")
            .and_then(Value::as_str)
            .map(|e| e.trim().to_lowercase())
            .filter(|e| !e.is_empty())
            .ok_or(Refused::NoEmail)?;
        let text = |k: &str| {
            claims
                .get(k)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .filter(|s| !s.trim().is_empty())
        };
        Ok(Verified {
            email,
            team: app.team_domain.clone(),
            exp,
            given_name: text("given_name"),
            name: text("name"),
        })
    }

    /// Who may use the deployment (spec 4.4): cached 15 s, the last answer used up to 300 s after that while hermit
    /// fails; `None` when hermit cannot be asked and nothing is cached.
    pub async fn access(&self, email: &str) -> Option<Access> {
        let cached = self.0.people.get(email).await;
        if let Some(e) = &cached
            && e.at.elapsed() < self.0.tuning.people_fresh
        {
            return Some(e.access);
        }
        match tokio::time::timeout(HERMIT_PEOPLE_TIMEOUT, self.0.hermit.person(email)).await {
            Ok(Ok(p)) => {
                let access = match (p.allowed, p.role) {
                    (true, Some(role)) => Access::Allowed(role),
                    _ => Access::NotAllowed,
                };
                self.remember(email, access).await;
                Some(access)
            }
            // a well-formed refusal of the address itself (bad_email): nobody by that address may use it
            Ok(Err(e)) if e.status() == Some(400) => {
                self.remember(email, Access::NotAllowed).await;
                Some(Access::NotAllowed)
            }
            Ok(Err(_)) | Err(_) => {
                let e = cached?;
                if e.at.elapsed() < self.0.tuning.people_fresh + self.0.tuning.people_stale {
                    self.0.metrics.hermit_stale();
                    Some(e.access)
                } else {
                    None
                }
            }
        }
    }

    async fn remember(&self, email: &str, access: Access) {
        self.0
            .people
            .insert(
                email.to_owned(),
                PeopleEntry {
                    access,
                    at: Instant::now(),
                },
            )
            .await;
    }

    /// The request identity (spec 4.6) from the token in `headers` (by `sources`) and an optional view-as token.
    pub async fn resolve(&self, headers: &HeaderMap, sources: Sources, view_as: Option<&str>) -> Identity {
        let id = self.resolve_inner(headers, sources, view_as).await;
        self.0.metrics.identity(id.label());
        id
    }

    async fn resolve_inner(&self, headers: &HeaderMap, sources: Sources, view_as: Option<&str>) -> Identity {
        // the caller's own identity comes first: a view-as token never stands in for a sign-in
        let Some(token) = token_of(headers, sources) else {
            return Identity::SignedOut;
        };
        let v = match self.verify(&token).await {
            Ok(v) => v,
            Err(why) => {
                tracing::debug!(why = why.as_str(), "token refused");
                return Identity::SignedOut;
            }
        };
        let role = match self.access(&v.email).await {
            None => return Identity::Unavailable,
            Some(Access::NotAllowed) => return Identity::NotAllowed(v.email),
            Some(Access::Allowed(role)) => role,
        };
        let person = Person {
            email: v.email,
            role,
            exp: v.exp,
            team: v.team,
            token,
            given_name: v.given_name,
            name: v.name,
        };
        // the legacy upstream opens with the newest token pigeon verified for the person (DESIGN 9.5)
        self.0.switch.saw(&person.email, &person.token, person.exp);
        let Some(raw) = view_as else {
            return Identity::Person(person);
        };
        self.view_as(person, raw).await
    }

    /// Spec 6.2's checks for a caller carrying a view-as token.
    async fn view_as(&self, caller: Person, raw: &str) -> Identity {
        let claims = match viewas::verify(&self.0.view_as_keys, raw, now_secs()) {
            Some(c) => Some(c),
            // a token the old router signed, honoured while any conversation is legacy (spec 6.2, DESIGN 9.5)
            None => self.0.switch.old_view_as(raw, &caller.email).await,
        };
        let Some(claims) = claims else {
            tracing::warn!(person = %aviary_core::log::person_key(&caller.email), "view-as refused: bad token");
            return Identity::ViewAsRefused;
        };
        if !caller.is_admin() || claims.a != caller.email {
            tracing::warn!(person = %aviary_core::log::person_key(&caller.email), "view-as refused: not this admin");
            return Identity::ViewAsRefused;
        }
        match self.access(&claims.u).await {
            None => Identity::Unavailable,
            Some(Access::NotAllowed) => {
                tracing::warn!(
                    person = %aviary_core::log::person_key(&caller.email),
                    "view-as refused: the person is not allowed"
                );
                Identity::ViewAsRefused
            }
            Some(Access::Allowed(_)) => Identity::ViewAs(ViewAs {
                admin: caller,
                person: claims.u,
                session: claims.s,
                exp: claims.e,
            }),
        }
    }

    /// The first name for the greeting (spec 4.5): from the token's claims, else Access's get-identity (cached per
    /// email 6 h, 5 min after a failure).
    pub async fn first_name(&self, p: &Person) -> Option<String> {
        if let Some(n) = first_name_from(p.given_name.as_deref(), p.name.as_deref()) {
            self.0
                .names
                .insert(
                    p.email.clone(),
                    NameEntry {
                        name: Some(n.clone()),
                        until: Instant::now() + self.0.tuning.first_name_ok,
                    },
                )
                .await;
            return Some(n);
        }
        if let Some(e) = self.0.names.get(&p.email).await
            && Instant::now() < e.until
        {
            return e.name;
        }
        let fetched = self.get_identity(&p.team, &p.token).await;
        let (name, keep) = match fetched {
            Some(v) => {
                let field = |k: &str| {
                    v.get(k)
                        .and_then(Value::as_str)
                        .or_else(|| v.get("oidc_fields").and_then(|o| o.get(k)).and_then(Value::as_str))
                };
                (
                    first_name_from(field("given_name"), field("name")),
                    self.0.tuning.first_name_ok,
                )
            }
            None => (None, self.0.tuning.first_name_err),
        };
        self.0
            .names
            .insert(
                p.email.clone(),
                NameEntry {
                    name: name.clone(),
                    until: Instant::now() + keep,
                },
            )
            .await;
        name
    }

    /// The first name this replica has for `email`, without asking anyone (the view-as `me`, spec 5.4).
    pub async fn cached_first_name(&self, email: &str) -> Option<String> {
        self.0
            .names
            .get(email)
            .await
            .filter(|e| Instant::now() < e.until)
            .and_then(|e| e.name)
    }

    async fn get_identity(&self, team: &str, token: &str) -> Option<Value> {
        let url = format!("{}{GET_IDENTITY_PATH}", (self.0.access_origin)(team));
        let res = tokio::time::timeout(
            GET_IDENTITY_TIMEOUT,
            self.0
                .http
                .get(url)
                .header(header::COOKIE, format!("{ACCESS_COOKIE}={token}"))
                .send(),
        )
        .await
        .ok()?
        .ok()?;
        if !res.status().is_success() {
            return None;
        }
        tokio::time::timeout(GET_IDENTITY_TIMEOUT, res.json::<Value>())
            .await
            .ok()?
            .ok()
    }
}

fn is_cjk(c: char) -> bool {
    matches!(u32::from(c),
        0x2E80..=0x2FDF | 0x3000..=0x303F | 0x3040..=0x30FF | 0x3100..=0x312F | 0x3130..=0x318F
        | 0x31A0..=0x31FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF
        | 0xFF00..=0xFFEF | 0x20000..=0x2FA1F)
}

/// Spec 4.5's rule: `given_name`, else the first word of `name` (a name with CJK characters kept whole), at most
/// 40 characters, `None` when empty or containing `@`.
#[must_use]
pub fn first_name_from(given_name: Option<&str>, name: Option<&str>) -> Option<String> {
    let pick = given_name
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            let n = name?.trim();
            if n.chars().any(is_cjk) {
                Some(n.to_owned())
            } else {
                n.split_whitespace().next().map(str::to_owned)
            }
        })?;
    if pick.is_empty() || pick.contains('@') {
        return None;
    }
    Some(pick.chars().take(FIRST_NAME_MAX).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_names() {
        assert_eq!(
            first_name_from(Some("Tai Man"), Some("Chan Tai Man")).as_deref(),
            Some("Tai Man")
        );
        assert_eq!(first_name_from(None, Some("Tai Man Chan")).as_deref(), Some("Tai"));
        assert_eq!(first_name_from(None, Some("陳 大文")).as_deref(), Some("陳 大文"));
        assert_eq!(first_name_from(Some(" "), Some("a@b.c")), None);
        assert_eq!(first_name_from(Some("x@y"), None), None);
        assert_eq!(first_name_from(None, None), None);
        assert_eq!(first_name_from(Some(&"a".repeat(50)), None).unwrap().len(), 40);
    }

    #[test]
    fn token_sources_in_order() {
        let mut h = HeaderMap::new();
        h.insert(APP_TOKEN_HEADER, "app".parse().unwrap());
        assert_eq!(token_of(&h, Sources::All).as_deref(), Some("app"));
        assert_eq!(token_of(&h, Sources::CookieOnly), None);
        h.insert(header::COOKIE, "a=1; CF_Authorization=c%2Eok; b=2".parse().unwrap());
        assert_eq!(token_of(&h, Sources::All).as_deref(), Some("c.ok"));
        assert_eq!(token_of(&h, Sources::CookieOnly).as_deref(), Some("c.ok"));
        h.insert(ASSERTION_HEADER, "hdr".parse().unwrap());
        assert_eq!(token_of(&h, Sources::All).as_deref(), Some("hdr"));
        h.insert(ASSERTION_HEADER, "x".repeat(MAX_TOKEN + 1).parse().unwrap());
        assert_eq!(token_of(&h, Sources::All), None, "over 8 KiB is no token");
    }

    #[test]
    fn percent_decoding_once() {
        assert_eq!(percent_decode("a%2Fb%252F"), "a/b%2F");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
    }
}
