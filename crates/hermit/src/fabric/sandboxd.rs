//! The sandboxd clients (hermit 13.15): one `sandboxd_api::Client` per host, made on first use and kept while the
//! host keeps its overlay address. The real connector speaks mutual TLS to `https://sandboxd:7443` resolved to the
//! host's `wg_ip` with the fabric CA and hermit's client identity (CN `hermit`); tests connect to plain fakes.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};

use sandboxd_api::{Client, Error, Tls};

use crate::fabric::CreateError;

/// Makes the client of one host.
pub trait Connect: Send + Sync + 'static {
    /// The client of host `name` at overlay address `wg_ip`.
    ///
    /// # Errors
    /// When the client cannot be built.
    fn connect(&self, name: &str, wg_ip: IpAddr) -> Result<Client, Error>;
}

/// The production connector: mutual TLS on the overlay (`HERMIT_FABRIC_CA`, `HERMIT_FABRIC_CLIENT_CERT/KEY`).
pub struct Mtls {
    tls: Tls,
}

impl std::fmt::Debug for Mtls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Mtls(..)")
    }
}

impl Mtls {
    #[must_use]
    pub fn new(tls: Tls) -> Self {
        Self { tls }
    }
}

impl Connect for Mtls {
    fn connect(&self, _name: &str, wg_ip: IpAddr) -> Result<Client, Error> {
        Client::new(wg_ip, sandboxd_api::PORT, &self.tls)
    }
}

/// The clients, by host name (cheap to clone: the clones share the cache).
#[derive(Clone)]
pub struct Clients {
    connect: Arc<dyn Connect>,
    cache: Arc<Mutex<HashMap<String, (IpAddr, Client)>>>,
}

impl std::fmt::Debug for Clients {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Clients(..)")
    }
}

impl Clients {
    #[must_use]
    pub fn new(connect: Arc<dyn Connect>) -> Self {
        Self {
            connect,
            cache: Arc::default(),
        }
    }

    /// The client of host `name` (a new one when its address changed).
    ///
    /// # Errors
    /// When the client cannot be built.
    pub fn get(&self, name: &str, wg_ip: IpAddr) -> Result<Client, Error> {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((ip, c)) = cache.get(name)
            && *ip == wg_ip
        {
            return Ok(c.clone());
        }
        let c = self.connect.connect(name, wg_ip)?;
        cache.insert(name.to_owned(), (wg_ip, c.clone()));
        Ok(c)
    }

    /// The client of a host row.
    ///
    /// # Errors
    /// When the client cannot be built.
    pub fn of(&self, h: &crate::fabric::hosts::HostRow) -> Result<Client, Error> {
        self.get(&h.name, h.wg_ip)
    }

    /// Forgets a host's client (the host is gone).
    pub fn forget(&self, name: &str) {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner).remove(name);
    }
}

/// The status of a sandboxd error answer, if it was one.
#[must_use]
pub fn status(e: &Error) -> Option<u16> {
    match e {
        Error::Status { status, .. } => Some(*status),
        _ => None,
    }
}

/// A create's failure as the start reads it (11.2 step 5): 503 busy (a fixed table full, the shared filesystem not
/// mounted) is retried; 409 `ip … is used by <name>` names the stale sandbox; no answer is `Unreachable`; any
/// other answer fails the start with its class.
#[must_use]
pub fn create_error(e: &Error) -> CreateError {
    match e {
        Error::Status { status: 503, .. } => CreateError::Busy,
        Error::Status { status: 409, body } if body.contains(" is used by ") => {
            let other = body.rsplit(" is used by ").next().unwrap_or_default().trim();
            CreateError::IpInUse(other.to_owned())
        }
        Error::Status { status, body } => CreateError::Failed {
            class: format!("sandboxd_{status}"),
            message: body.clone(),
        },
        Error::Transport(_) | Error::Timeout(_) => CreateError::Unreachable(e.to_string()),
        Error::Decode(_) | Error::Tls(_) => CreateError::Failed {
            class: "sandboxd_client".into(),
            message: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_errors() {
        let st = |status, body: &str| Error::Status {
            status,
            body: body.into(),
        };
        assert_eq!(create_error(&st(503, "full")), CreateError::Busy);
        assert_eq!(
            create_error(&st(409, "ip 10.203.0.11 is used by sbx-0a1b")),
            CreateError::IpInUse("sbx-0a1b".into())
        );
        assert!(matches!(
            create_error(&st(409, "workspace missing")),
            CreateError::Failed { class, .. } if class == "sandboxd_409"
        ));
        assert!(matches!(
            create_error(&Error::Timeout(std::time::Duration::from_secs(1))),
            CreateError::Unreachable(_)
        ));
    }
}
