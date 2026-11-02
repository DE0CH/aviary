//! The staff catalogue (hermit 3.4.1): `{"users": [...], "admins": [...]}`, re-read every 30 s from the mounted
//! `ConfigMap`; a file that fails validation is refused and the last good one kept. The same rules run in YYC's CI as
//! `hermit catalogue-check`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use hermit_api::Timestamp;
use hermit_api::people::{ExplicitPerson, People, Person, Role, normalise_email};
use prometheus_client::metrics::counter::Counter;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// At most this many entries (users and admins together).
pub const MAX_ENTRIES: usize = 10_000;
/// At most this many bytes.
pub const MAX_BYTES: u64 = 1024 * 1024;
/// How often a refusal is logged again while the file stays refused.
const REFUSED_LOG_EVERY: Duration = Duration::from_secs(300);

/// A loaded, valid catalogue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    /// SHA-256 hex of the file.
    pub generation: String,
    pub loaded_at: Timestamp,
    /// Explicit addresses in `users` (normalised).
    pub users: BTreeSet<String>,
    /// `@domain` entries without the `@` (lowercased).
    pub domains: BTreeSet<String>,
    pub admins: BTreeSet<String>,
}

fn hostname_ok(d: &str) -> bool {
    !d.is_empty()
        && d.len() <= 253
        && d.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
        && !d.starts_with('.')
        && !d.ends_with('.')
        && !d.contains("..")
}

/// Hex SHA-256.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

impl Loaded {
    /// Validates a catalogue file's bytes (3.4.1).
    ///
    /// # Errors
    /// The first rule the file breaks, in English.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_BYTES {
            return Err(format!("the file is over {MAX_BYTES} bytes"));
        }
        let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("not valid JSON: {e}"))?;
        let obj = v.as_object().ok_or("not a JSON object")?;
        let strings = |key: &str| -> Result<Vec<String>, String> {
            let arr = obj
                .get(key)
                .ok_or_else(|| format!("`{key}` is missing"))?
                .as_array()
                .ok_or_else(|| format!("`{key}` is not an array"))?;
            arr.iter()
                .map(|e| {
                    e.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| format!("`{key}` has an entry that is not a string"))
                })
                .collect()
        };
        let raw_users = strings("users")?;
        let raw_admins = strings("admins")?;
        if raw_users.is_empty() {
            return Err("`users` is empty".into());
        }
        if raw_users.len() + raw_admins.len() > MAX_ENTRIES {
            return Err(format!("more than {MAX_ENTRIES} entries"));
        }
        let mut users = BTreeSet::new();
        let mut domains = BTreeSet::new();
        for u in &raw_users {
            let t = u.trim();
            if let Some(d) = t.strip_prefix('@') {
                let d = d.to_lowercase();
                if !hostname_ok(&d) {
                    return Err(format!("`users` entry {u:?} is not an email or @domain"));
                }
                domains.insert(d);
            } else {
                let e = normalise_email(t).ok_or_else(|| format!("`users` entry {u:?} is not an email or @domain"))?;
                users.insert(e);
            }
        }
        let mut admins = BTreeSet::new();
        for a in &raw_admins {
            let e = normalise_email(a).ok_or_else(|| format!("`admins` entry {a:?} is not an email"))?;
            if !allowed_by(&users, &domains, &e) {
                return Err(format!("admin {a:?} is not in `users`"));
            }
            admins.insert(e);
        }
        Ok(Self {
            generation: sha256_hex(bytes),
            loaded_at: Timestamp::now(),
            users,
            domains,
            admins,
        })
    }

    /// Exact match in `users`, or `users` has `@<domain>` for the part after the last `@` (no subdomains).
    #[must_use]
    pub fn allowed(&self, email: &str) -> bool {
        allowed_by(&self.users, &self.domains, email)
    }

    #[must_use]
    pub fn role(&self, email: &str) -> Option<Role> {
        if self.admins.contains(email) {
            Some(Role::Admin)
        } else if self.allowed(email) {
            Some(Role::Staff)
        } else {
            None
        }
    }

    /// H2 for an already normalised address.
    #[must_use]
    pub fn person(&self, email: &str) -> Person {
        let role = self.role(email);
        Person {
            email: email.to_owned(),
            allowed: role.is_some(),
            role,
        }
    }

    /// `GET /v1/people`.
    #[must_use]
    pub fn people(&self) -> People {
        let mut explicit: Vec<ExplicitPerson> = self
            .users
            .iter()
            .map(|e| ExplicitPerson {
                email: e.clone(),
                role: if self.admins.contains(e) {
                    Role::Admin
                } else {
                    Role::Staff
                },
            })
            .collect();
        for a in &self.admins {
            if !self.users.contains(a) {
                explicit.push(ExplicitPerson {
                    email: a.clone(),
                    role: Role::Admin,
                });
            }
        }
        explicit.sort_by(|a, b| a.email.cmp(&b.email));
        People {
            generation: self.generation.clone(),
            loaded_at: self.loaded_at,
            explicit,
            domains: self.domains.iter().cloned().collect(),
        }
    }
}

fn allowed_by(users: &BTreeSet<String>, domains: &BTreeSet<String>, email: &str) -> bool {
    if users.contains(email) {
        return true;
    }
    email.rsplit_once('@').is_some_and(|(_, d)| domains.contains(d))
}

/// The live catalogue of one replica.
#[derive(Debug)]
pub struct Catalogue {
    path: PathBuf,
    current: RwLock<Option<Arc<Loaded>>>,
    refused: Mutex<Option<(String, Instant)>>,
    refusals: Counter,
}

impl Catalogue {
    /// A catalogue read from `path` (nothing loaded yet: call [`Catalogue::reload`]).
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            current: RwLock::new(None),
            refused: Mutex::new(None),
            refusals: Counter::default(),
        }
    }

    /// A catalogue holding `loaded` (tests).
    #[must_use]
    pub fn fixed(loaded: Loaded) -> Self {
        let c = Self::new("/nonexistent");
        *c.current.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(loaded));
        c
    }

    /// The current good catalogue, if one was ever loaded.
    #[must_use]
    pub fn get(&self) -> Option<Arc<Loaded>> {
        self.current.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The reason the file is refused now, if it is (for the ops verdict).
    #[must_use]
    pub fn refused(&self) -> Option<String> {
        self.refused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|(r, _)| r.clone())
    }

    /// How many re-reads were refused (`hermit_catalogue_refused_total`).
    #[must_use]
    pub fn refusals(&self) -> u64 {
        self.refusals.get()
    }

    /// The counter behind [`Catalogue::refusals`], for the ops registry.
    #[must_use]
    pub fn refusals_counter(&self) -> Counter {
        self.refusals.clone()
    }

    /// Whether `email` (normalised) is allowed now; `false` with no catalogue.
    #[must_use]
    pub fn allowed(&self, email: &str) -> bool {
        self.get().is_some_and(|c| c.allowed(email))
    }

    /// Re-reads the file: an unchanged file (same SHA-256) is skipped; a refused one keeps the last good catalogue.
    /// Returns whether a good catalogue is loaded now.
    pub fn reload(&self) -> bool {
        let res = std::fs::metadata(&self.path)
            .map_err(|e| format!("cannot read: {e}"))
            .and_then(|m| {
                if m.len() > MAX_BYTES {
                    Err(format!("the file is over {MAX_BYTES} bytes"))
                } else {
                    std::fs::read(&self.path).map_err(|e| format!("cannot read: {e}"))
                }
            });
        let outcome = res.and_then(|bytes| {
            if self.get().is_some_and(|c| c.generation == sha256_hex(&bytes)) {
                return Ok(None);
            }
            Loaded::parse(&bytes).map(Some)
        });
        match outcome {
            Ok(None) => {
                self.clear_refusal();
            }
            Ok(Some(loaded)) => {
                tracing::info!(
                    generation = %loaded.generation,
                    users = loaded.users.len(),
                    domains = loaded.domains.len(),
                    admins = loaded.admins.len(),
                    "catalogue loaded"
                );
                *self.current.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(loaded));
                self.clear_refusal();
            }
            Err(reason) => self.refuse(reason),
        }
        self.get().is_some()
    }

    fn clear_refusal(&self) {
        *self.refused.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn refuse(&self, reason: String) {
        let mut r = self.refused.lock().unwrap_or_else(PoisonError::into_inner);
        let log = match &*r {
            Some((old, at)) => *old != reason || at.elapsed() >= REFUSED_LOG_EVERY,
            None => true,
        };
        if log {
            self.refusals.inc();
            tracing::warn!(path = %self.path.display(), error = %reason, "catalogue refused");
            *r = Some((reason, Instant::now()));
        }
    }
}

/// `hermit catalogue-check <file>`: the validation alone (YYC's CI, platform 13.3 item 7).
///
/// # Errors
/// The reason the file is refused.
pub fn check_file(path: &Path) -> Result<Loaded, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Loaded::parse(&bytes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn parse(v: &serde_json::Value) -> Result<Loaded, String> {
        Loaded::parse(v.to_string().as_bytes())
    }

    #[test]
    fn rules() {
        let c = parse(&serde_json::json!({
            "users": ["@example.org", " Staff@Example.NET ", "boss@example.com"],
            "admins": ["boss@example.com"]
        }))
        .unwrap();
        assert_eq!(c.role("a@example.org"), Some(Role::Staff));
        assert_eq!(c.role("a@sub.example.org"), None, "no subdomain matching");
        assert_eq!(c.role("staff@example.net"), Some(Role::Staff));
        assert_eq!(c.role("boss@example.com"), Some(Role::Admin));
        assert_eq!(c.role("x@example.com"), None);
        let p = c.people();
        assert_eq!(p.domains, vec!["example.org"]);
        assert_eq!(p.explicit.len(), 2);
        assert!(
            p.explicit
                .iter()
                .any(|e| e.email == "boss@example.com" && e.role == Role::Admin)
        );
    }

    #[test]
    fn refusals() {
        for (v, why) in [
            (serde_json::json!([]), "object"),
            (serde_json::json!({"users": [], "admins": []}), "empty"),
            (serde_json::json!({"users": ["a@b.c"]}), "admins"),
            (serde_json::json!({"users": ["a@b.c"], "admins": ["z@b.c"]}), "not in"),
            (serde_json::json!({"users": ["nope"], "admins": []}), "email"),
            (serde_json::json!({"users": ["@bad domain"], "admins": []}), "email"),
            (serde_json::json!({"users": [3], "admins": []}), "string"),
        ] {
            let e = parse(&v).unwrap_err();
            assert!(e.contains(why), "{v}: {e}");
        }
        let many: Vec<String> = (0..=MAX_ENTRIES).map(|i| format!("u{i}@example.org")).collect();
        assert!(parse(&serde_json::json!({"users": many, "admins": []})).is_err());
        // an admin matched by domain is allowed
        assert!(parse(&serde_json::json!({"users": ["@example.org"], "admins": ["a@example.org"]})).is_ok());
    }

    #[test]
    fn reload_keeps_the_last_good_one() {
        let dir = std::env::temp_dir().join(format!("hermit-cat-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("catalogue.json");
        let cat = Catalogue::new(&path);
        assert!(!cat.reload());
        assert!(cat.refused().is_some());
        std::fs::write(&path, r#"{"users": ["@example.org"], "admins": []}"#).unwrap();
        assert!(cat.reload());
        let g = cat.get().unwrap().generation.clone();
        assert!(cat.refused().is_none());
        std::fs::write(&path, r#"{"users": [], "admins": []}"#).unwrap();
        assert!(cat.reload());
        assert_eq!(cat.get().unwrap().generation, g);
        assert!(cat.refused().unwrap().contains("empty"));
        std::fs::write(&path, r#"{"users": ["a@example.net"], "admins": ["a@example.net"]}"#).unwrap();
        assert!(cat.reload());
        assert!(cat.allowed("a@example.net") && !cat.allowed("x@example.org"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
