//! Configured routes (platform 3.11.8): names a deployment adds beside the four fixed ones, each to a service of its
//! own the sandbox calls directly (a label store's REST API, say), with the deployment's credential
//! injected. A configured name is recognised like a fixed name (exact host, ASCII case-insensitive, no port or port
//! 80, never resolved by DNS, never `CONNECT`), after the fixed names and before the self hosts. Only its path
//! prefixes are routed (the fixed names' clean-path rule applies); the headers are stripped and the identity triple
//! and `Aviary-Request` added as on a fixed name (3.11.3); the upstream gets the route's own credential. Limits: 64
//! MiB per request body, the `tools` route's timeout and drain bound.

use crate::{FixedName, clean_path};

/// A configured route as the gateway checks it: its name and the path prefixes it routes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub name: String,
    pub prefixes: Vec<String>,
}

/// Why a route was refused at configuration time.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RouteError {
    #[error("route name {0:?}: one DNS label [a-z][a-z0-9-]{{0,62}}, not ending in '-'")]
    BadName(String),
    #[error("route name {0:?} is a fixed name")]
    FixedName(String),
    #[error("route {name}: path prefix {prefix:?} must start and end with '/' and be a clean path")]
    BadPrefix { name: String, prefix: String },
    #[error("route {0}: at least one path prefix")]
    NoPrefix(String),
    #[error("route {0} twice")]
    Twice(String),
}

/// Whether `name` may be a configured route's name: one lowercase DNS label, not a fixed name.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 63
        && b[0].is_ascii_lowercase()
        && b[b.len() - 1] != b'-'
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

impl Route {
    /// A checked route.
    ///
    /// # Errors
    /// A bad or fixed name, no prefix, or a prefix that is not `/…/` and clean.
    pub fn new(name: &str, prefixes: &[&str]) -> Result<Self, RouteError> {
        if !valid_name(name) {
            return Err(RouteError::BadName(name.to_owned()));
        }
        if FixedName::from_host(name).is_some() {
            return Err(RouteError::FixedName(name.to_owned()));
        }
        if prefixes.is_empty() {
            return Err(RouteError::NoPrefix(name.to_owned()));
        }
        for p in prefixes {
            if !p.ends_with('/') || !clean_path(p) {
                return Err(RouteError::BadPrefix {
                    name: name.to_owned(),
                    prefix: (*p).to_owned(),
                });
            }
        }
        Ok(Self {
            name: name.to_owned(),
            prefixes: prefixes.iter().map(|p| (*p).to_owned()).collect(),
        })
    }

    /// Whether `path_and_query` is routed (the query is ignored): clean, under one of the prefixes. Any method.
    #[must_use]
    pub fn allows(&self, path_and_query: &str) -> bool {
        let path = path_and_query.split_once('?').map_or(path_and_query, |(p, _)| p);
        clean_path(path) && self.prefixes.iter().any(|p| path.starts_with(p.as_str()))
    }
}

/// Checks a set of routes together (no name twice).
///
/// # Errors
/// The first name given twice.
pub fn check_set<R: AsRef<Route>>(routes: &[R]) -> Result<(), RouteError> {
    for (i, r) in routes.iter().enumerate() {
        let r = r.as_ref();
        if routes[..i].iter().any(|o| o.as_ref().name == r.name) {
            return Err(RouteError::Twice(r.name.clone()));
        }
    }
    Ok(())
}

impl AsRef<Self> for Route {
    fn as_ref(&self) -> &Self {
        self
    }
}

/// The configured route a request's host names: exact, ASCII case-insensitive, with no port or port 80.
#[must_use]
pub fn from_host<'a, R: AsRef<Route>>(routes: &'a [R], host: &str) -> Option<&'a R> {
    let name = match host.rsplit_once(':') {
        Some((name, "80")) => name,
        Some(_) => return None,
        None => host,
    };
    routes.iter().find(|r| r.as_ref().name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_name("labels"));
        assert!(valid_name("a-1"));
        for bad in ["", "Labels", "-a", "a-", "a.b", "a_b", &"a".repeat(64)] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert_eq!(
            Route::new("tools", &["/x/"]),
            Err(RouteError::FixedName("tools".into()))
        );
        assert_eq!(Route::new("labels", &[]), Err(RouteError::NoPrefix("labels".into())));
        for p in ["api/", "/api", "/a/../b/", "/a//b/", "/%2e%2e/"] {
            assert!(Route::new("labels", &[p]).is_err(), "{p}");
        }
        let r = Route::new("labels", &["/api/v1/"]).unwrap();
        assert_eq!(
            check_set(&[r.clone(), r.clone()]),
            Err(RouteError::Twice("labels".into()))
        );
        assert_eq!(check_set(&[r]), Ok(()));
    }

    #[test]
    fn hosts_and_paths() {
        let routes = vec![Route::new("labels", &["/api/v1/"]).unwrap()];
        assert_eq!(from_host(&routes, "LABELS").map(|r| r.name.as_str()), Some("labels"));
        assert!(from_host(&routes, "labels:80").is_some());
        assert!(from_host(&routes, "labels:8080").is_none());
        assert!(from_host(&routes, "labels.example.org").is_none());
        let r = &routes[0];
        assert!(r.allows("/api/v1/designs"));
        assert!(r.allows("/api/v1/designs/forum?view=preview"));
        assert!(!r.allows("/api/v1"));
        assert!(!r.allows("/api/v2/designs"));
        assert!(!r.allows("/designs/forum"));
        assert!(!r.allows("/api/v1/../admin"));
        assert!(!r.allows("/api/v1/%2e%2e/admin"));
        assert!(!r.allows("/api/v1//x"));
    }
}
