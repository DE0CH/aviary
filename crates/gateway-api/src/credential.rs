//! The proxy credential and the boot environment (platform 3.11.1-3.11.2, hermit 5.1, DESIGN 12 D1).
//!
//! Every sandbox client sends absolute-form requests (or `CONNECT host:443`) to the proxy named in its boot
//! environment, with `Proxy-Authorization: Basic base64("sbx:" + token)`. The token travels only as that credential,
//! in the userinfo of the proxy URL; there is no separate token variable.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

/// The credential's user name.
pub const PROXY_USER: &str = "sbx";
/// `Proxy-Authenticate` on a 407.
pub const PROXY_AUTHENTICATE: &str = "Basic realm=\"aviary-gateway\"";
/// The proxy variables of the boot environment, all set to the same URL.
pub const PROXY_VARS: [&str; 4] = ["http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY"];
/// `NO_PROXY` and `no_proxy`.
pub const NO_PROXY_VARS: [&str; 2] = ["NO_PROXY", "no_proxy"];
pub const NO_PROXY: &str = "localhost,127.0.0.1,::1";

/// `Basic base64("sbx:" + token)` for `Proxy-Authorization` (or `Authorization` on self-host paths).
#[must_use]
pub fn proxy_authorization(token: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{PROXY_USER}:{token}")))
}

/// Why a credential header was refused (a 407 when missing, else 403 `not_allowed`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialError {
    /// Not `Basic <base64>`: answer 407.
    Missing,
    /// Basic, but not `sbx:<token>`.
    Malformed,
}

/// The token in a `Proxy-Authorization` (or `Authorization`) value.
///
/// # Errors
/// [`CredentialError`].
pub fn parse_proxy_authorization(value: &str) -> Result<String, CredentialError> {
    let value = value.trim();
    let (scheme, rest) = value.split_once(' ').ok_or(CredentialError::Missing)?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return Err(CredentialError::Missing);
    }
    let raw = STANDARD.decode(rest.trim()).map_err(|_| CredentialError::Malformed)?;
    let text = String::from_utf8(raw).map_err(|_| CredentialError::Malformed)?;
    let (user, token) = text.split_once(':').ok_or(CredentialError::Malformed)?;
    if user != PROXY_USER || token.is_empty() {
        return Err(CredentialError::Malformed);
    }
    Ok(token.to_owned())
}

/// `http://sbx:<token>@<addr>` (`addr` is `host:port`). The token is URL-userinfo safe by construction.
#[must_use]
pub fn proxy_url(token: &str, addr: &str) -> String {
    format!("http://{PROXY_USER}:{token}@{addr}")
}

/// The sandbox token in a proxy URL's userinfo (how the supervisor learns its own token, D1).
#[must_use]
pub fn token_from_proxy_url(url: &str) -> Option<String> {
    let rest = url.strip_prefix("http://")?;
    let authority = rest.split('/').next()?;
    let (userinfo, _) = authority.rsplit_once('@')?;
    let (user, token) = userinfo.split_once(':')?;
    (user == PROXY_USER && !token.is_empty()).then(|| token.to_owned())
}

/// The proxy address (`host:port`) of a proxy URL, without the credential.
#[must_use]
pub fn proxy_addr(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("http://")?;
    let authority = rest.split('/').next()?;
    Some(authority.rsplit_once('@').map_or(authority, |(_, a)| a))
}

/// The variables the boot environment must carry (hermit 5.1 item 2), in a fixed order.
#[must_use]
pub fn boot_env(token: &str, addr: &str) -> Vec<(&'static str, String)> {
    let url = proxy_url(token, addr);
    let mut out: Vec<(&'static str, String)> = PROXY_VARS.iter().map(|v| (*v, url.clone())).collect();
    out.extend(NO_PROXY_VARS.iter().map(|v| (*v, NO_PROXY.to_owned())));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "hst1.1.AQID.BAUG";

    #[test]
    fn header_round_trip() {
        let h = proxy_authorization(T);
        assert!(h.starts_with("Basic "));
        assert_eq!(parse_proxy_authorization(&h), Ok(T.to_owned()));
        assert_eq!(
            parse_proxy_authorization(&h.replacen("Basic", "basic", 1)),
            Ok(T.to_owned())
        );
        assert_eq!(parse_proxy_authorization("Bearer x"), Err(CredentialError::Missing));
        assert_eq!(parse_proxy_authorization(""), Err(CredentialError::Missing));
        assert_eq!(parse_proxy_authorization("Basic !!"), Err(CredentialError::Malformed));
        let other = format!("Basic {}", STANDARD.encode(format!("root:{T}")));
        assert_eq!(parse_proxy_authorization(&other), Err(CredentialError::Malformed));
    }

    #[test]
    fn url_round_trip() {
        let u = proxy_url(T, "192.0.2.1:3128");
        assert_eq!(u, "http://sbx:hst1.1.AQID.BAUG@192.0.2.1:3128");
        assert_eq!(token_from_proxy_url(&u).as_deref(), Some(T));
        assert_eq!(proxy_addr(&u), Some("192.0.2.1:3128"));
        assert_eq!(token_from_proxy_url("http://192.0.2.1:3128"), None);
        assert_eq!(token_from_proxy_url("https://sbx:x@h:1"), None);
        assert_eq!(
            token_from_proxy_url(&format!("{u}/mirror/pypi/simple")).as_deref(),
            Some(T)
        );
    }

    #[test]
    fn the_boot_environment() {
        let env = boot_env(T, "192.0.2.1:3128");
        assert_eq!(env.len(), 6);
        for (k, v) in &env {
            if k.eq_ignore_ascii_case("no_proxy") {
                assert_eq!(v, NO_PROXY);
            } else {
                assert_eq!(token_from_proxy_url(v).as_deref(), Some(T));
            }
        }
    }
}
