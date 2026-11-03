//! The newest Access token pigeon verified for each person (switch-final 3.2 C4): the legacy upstream opens, and
//! reopens, as the person with it. Also the old router's view-as key (spec 6.2): a token it signed is honoured while
//! any conversation is legacy, and the legacy upstream signs its view-as calls with it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::identity::now_secs;
use crate::viewas::{self, Claims};
use crate::{App, KeyList};

/// People kept at most; the entry with the earliest expiry goes first beyond.
const MAX_PEOPLE: usize = 20_000;

/// The book. Cheap to clone.
#[derive(Clone)]
pub struct Book(Arc<Inner>);

struct Inner {
    newest: Mutex<HashMap<String, (String, i64)>>,
    old_key: KeyList,
    app: OnceLock<App>,
}

impl std::fmt::Debug for Book {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Book").finish_non_exhaustive()
    }
}

impl Book {
    pub(crate) fn new(old_key: KeyList) -> Self {
        Self(Arc::new(Inner {
            newest: Mutex::new(HashMap::new()),
            old_key,
            app: OnceLock::new(),
        }))
    }

    pub(crate) fn set_app(&self, app: &App) {
        let _ = self.0.app.set(app.clone());
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, (String, i64)>> {
        self.0.newest.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A token pigeon just verified for `email`: kept when it expires later than the one kept.
    pub fn saw(&self, email: &str, token: &str, exp: i64) {
        let mut m = self.map();
        match m.get_mut(email) {
            Some((t, e)) if *e >= exp => {
                let _ = t;
            }
            Some(slot) => *slot = (token.to_owned(), exp),
            None => {
                if m.len() >= MAX_PEOPLE {
                    let now = now_secs();
                    m.retain(|_, (_, e)| *e > now);
                    if m.len() >= MAX_PEOPLE
                        && let Some(k) = m.iter().min_by_key(|(_, (_, e))| *e).map(|(k, _)| k.clone())
                    {
                        m.remove(&k);
                    }
                }
                m.insert(email.to_owned(), (token.to_owned(), exp));
            }
        }
    }

    /// The newest token of `email` and its expiry, while it has not expired.
    #[must_use]
    pub fn newest(&self, email: &str) -> Option<(String, i64)> {
        self.map().get(email).filter(|(_, e)| *e > now_secs()).cloned()
    }

    /// The claims of a token the old router signed, when its MAC holds under the old key, it has not expired, and
    /// some conversation is still legacy (the caller's identity checks follow, as for pigeon's own tokens).
    pub async fn old_view_as(&self, token: &str, caller: &str) -> Option<Claims> {
        let claims = viewas::verify(&self.0.old_key, token, now_secs())?;
        let app = self.0.app.get()?;
        match app.legacy().window(caller).await {
            Ok(w) if w.any_legacy => Some(claims),
            _ => None,
        }
    }

    /// The same claims signed with the old key: what the old router accepts on a legacy upstream call for a view.
    #[must_use]
    pub fn old_token(&self, claims: &Claims) -> String {
        viewas::sign(&self.0.old_key, claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_token_is_the_latest_expiry() {
        let b = Book::new(KeyList(vec![[1; 32]]));
        let now = now_secs();
        b.saw("a@example.org", "t1", now + 100);
        b.saw("a@example.org", "t0", now + 50);
        assert_eq!(b.newest("a@example.org").unwrap().0, "t1");
        b.saw("a@example.org", "t2", now + 200);
        assert_eq!(b.newest("a@example.org").unwrap().0, "t2");
        b.saw("b@example.org", "old", now - 1);
        assert!(
            b.newest("b@example.org").is_none(),
            "an expired token is not handed out"
        );
    }
}
