//! What the supervisor takes from its boot environment, and the environment it gives the processes it starts.
//!
//! - The sandbox token travels only as the proxy credential in `HTTP_PROXY`'s userinfo (DESIGN 12 D1, platform
//!   R32, hermit 5.1): there is no `HERMIT_SANDBOX_TOKEN`. Every call to a fixed name (`magpie`, `hermit`, `model`)
//!   is an absolute-form request through that proxy with `Proxy-Authorization: Basic sbx:<token>` (the only
//!   addressing mode).
//! - Every process the supervisor starts gets exactly the image's `PATH`, `LANG` and `LC_ALL`, its `HOME`, then the
//!   descriptor's `env` unchanged, then the caller's fixed table, which wins on a clash (supervisor 6.2). Nothing of
//!   the boot environment itself is passed on.

use std::collections::BTreeMap;
use std::time::Duration;

use aviary_core::env;
use aviary_core::version::Build;

use crate::limits::{CONNECT_TIMEOUT, POOL_PER_NAME};

/// The variable carrying the proxy and the credential.
pub const PROXY_VAR: &str = "HTTP_PROXY";
/// `HOME` of the agent's processes (uid 1000).
pub const AGENT_HOME: &str = "/home/sandbox";
/// `HOME` of root's processes (dockerd).
pub const ROOT_HOME: &str = "/root";

/// The boot environment's parts the supervisor uses.
#[derive(Clone)]
pub struct BootEnv {
    /// `http://sbx:<token>@<proxy address>`.
    pub proxy: String,
    pub path: String,
    pub lang: Option<String>,
    pub lc_all: Option<String>,
}

impl std::fmt::Debug for BootEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BootEnv")
            .field("proxy", &aviary_core::log::redact(&self.proxy))
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// The proxy URL has no sandbox token in its userinfo.
#[derive(Debug, thiserror::Error)]
#[error("{PROXY_VAR} carries no sandbox credential")]
pub struct NoCredential;

impl BootEnv {
    /// Reads the boot environment; a missing `HTTP_PROXY` or `PATH` aborts start (`aviary_core::env`).
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            proxy: env::required(PROXY_VAR),
            path: env::required("PATH"),
            // image facts: an image may set neither
            lang: env::try_required("LANG").ok(),
            lc_all: env::try_required("LC_ALL").ok(),
        }
    }

    /// The sandbox token from the proxy URL's password.
    ///
    /// # Errors
    /// When the URL has no `user:password@`.
    pub fn token(&self) -> Result<String, NoCredential> {
        proxy_token(&self.proxy).ok_or(NoCredential)
    }

    /// The environment of a process the supervisor starts (6.2): the image's basics and `home`, the descriptor's
    /// `env`, then `fixed`, which wins on a clash (logged once per name).
    #[must_use]
    pub fn child_env(
        &self,
        home: &str,
        descriptor: &BTreeMap<String, String>,
        fixed: &[(&str, String)],
    ) -> Vec<String> {
        let mut m: BTreeMap<String, String> = BTreeMap::new();
        m.insert("PATH".into(), self.path.clone());
        if let Some(v) = &self.lang {
            m.insert("LANG".into(), v.clone());
        }
        if let Some(v) = &self.lc_all {
            m.insert("LC_ALL".into(), v.clone());
        }
        m.insert("HOME".into(), home.to_owned());
        for (k, v) in descriptor {
            if valid_name(k) && !v.contains('\0') {
                m.insert(k.clone(), v.clone());
            }
        }
        for (k, v) in fixed {
            if let Some(old) = m.insert((*k).to_owned(), v.clone())
                && old != *v
                && descriptor.contains_key(*k)
            {
                tracing::info!(name = %k, "descriptor env overridden by the fixed table");
            }
        }
        m.into_iter().map(|(k, v)| format!("{k}={v}")).collect()
    }

    /// The client for the fixed names: every request through the boot proxy (its URL's userinfo becomes the
    /// `Proxy-Authorization`), HTTP/1.1 keep-alive with at most 8 idle connections per name, 10 s to connect,
    /// `User-Agent: supervisor/<sha12>`, and no other proxy from the environment.
    ///
    /// # Errors
    /// When the proxy URL does not parse.
    pub fn fixed_names_client(&self, build: &Build) -> Result<reqwest::Client, reqwest::Error> {
        let proxy = reqwest::Proxy::all(&self.proxy)?;
        aviary_core::http::client_builder(build, None)
            .no_proxy()
            .proxy(proxy)
            .http1_only()
            .pool_max_idle_per_host(POOL_PER_NAME)
            .pool_idle_timeout(Duration::from_secs(50))
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
    }
}

/// An environment variable name: `[A-Za-z_][A-Za-z0-9_]*`.
#[must_use]
pub fn valid_name(k: &str) -> bool {
    let b = k.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

/// The password of `http://user:password@host:port` (percent-decoded), `None` without one.
#[must_use]
pub fn proxy_token(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let (userinfo, _) = authority.rsplit_once('@')?;
    let (_, password) = userinfo.split_once(':')?;
    let token = percent_decode(password)?;
    (!token.is_empty()).then_some(token)
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boot() -> BootEnv {
        BootEnv {
            proxy: "http://sbx:hst1.1.payload.sig@192.0.2.1:3128".into(),
            path: "/usr/local/bin:/usr/bin:/bin".into(),
            lang: Some("C.UTF-8".into()),
            lc_all: None,
        }
    }

    #[test]
    fn the_token_comes_from_the_proxy_userinfo() {
        assert_eq!(boot().token().unwrap(), "hst1.1.payload.sig");
        assert_eq!(proxy_token("http://sbx:a%2Eb@h:1/").as_deref(), Some("a.b"));
        assert_eq!(proxy_token("http://192.0.2.1:3128"), None);
        assert_eq!(proxy_token("http://sbx@192.0.2.1:3128"), None);
        assert_eq!(proxy_token("http://sbx:@h:1"), None);
        assert_eq!(proxy_token("not a url"), None);
        let d = format!("{:?}", boot());
        assert!(!d.contains("payload.sig"), "{d}");
    }

    #[test]
    fn child_env_is_basics_then_descriptor_then_fixed() {
        let mut desc = BTreeMap::new();
        desc.insert("HTTP_PROXY".into(), "http://sbx:t@p:3128".into());
        desc.insert("NO_PROXY".into(), "everything".into());
        desc.insert("PATH".into(), "/evil".into());
        desc.insert("bad name".into(), "x".into());
        let env = boot().child_env(
            AGENT_HOME,
            &desc,
            &[
                ("NO_PROXY", "localhost,127.0.0.1,::1".into()),
                ("ANTHROPIC_BASE_URL", "http://model".into()),
            ],
        );
        assert!(env.contains(&"HOME=/home/sandbox".to_owned()));
        assert!(env.contains(&"LANG=C.UTF-8".to_owned()));
        assert!(!env.iter().any(|e| e.starts_with("LC_ALL=")));
        assert!(env.contains(&"HTTP_PROXY=http://sbx:t@p:3128".to_owned()));
        assert!(env.contains(&"NO_PROXY=localhost,127.0.0.1,::1".to_owned()));
        assert!(env.contains(&"ANTHROPIC_BASE_URL=http://model".to_owned()));
        // the descriptor's env is applied unchanged, even over the image's PATH
        assert!(env.contains(&"PATH=/evil".to_owned()));
        assert!(!env.iter().any(|e| e.starts_with("bad name")));
    }

    #[test]
    fn the_client_builds_with_the_proxy() {
        let build = Build::new("supervisor");
        assert!(boot().fixed_names_client(&build).is_ok());
    }
}
