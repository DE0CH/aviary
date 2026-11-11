//! The Telegram account's session as text: `{"home_dc": 4, "dc_options": [<grammers DcOption>…]}`, the permanent
//! authorization keys hex-encoded inside. `herald login` writes it; `HERALD_TELEGRAM_SESSION` carries it. Only the
//! home DC and the DC options (with their keys) are kept: peers are resolved again at start, and herald reads no
//! updates.

use grammers_client::session::types::DcOption;
use grammers_client::session::{Session as _, SessionData, storages::MemorySession};
use serde::{Deserialize, Serialize};

/// The stored form.
#[derive(Debug, Serialize, Deserialize)]
pub struct Stored {
    pub home_dc: i32,
    pub dc_options: Vec<DcOption>,
}

/// Why a stored session is unusable.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the session is not JSON of the expected shape: {0}")]
    Shape(#[from] serde_json::Error),
    #[error("the session has no authorization key for its home DC {0}")]
    NoHomeKey(i32),
}

/// A memory session from its stored text.
///
/// # Errors
/// When the text isn't a stored session, or its home DC has no key (a session from before the login finished).
pub fn load(text: &str) -> Result<MemorySession, Error> {
    let stored: Stored = serde_json::from_str(text.trim())?;
    let home = stored.home_dc;
    if !stored.dc_options.iter().any(|d| d.id == home && d.auth_key.is_some()) {
        return Err(Error::NoHomeKey(home));
    }
    let mut data = SessionData {
        home_dc: home,
        ..SessionData::default()
    };
    for d in stored.dc_options {
        data.dc_options.insert(d.id, d);
    }
    Ok(MemorySession::from(data))
}

/// The stored text of `session`, DCs 1-5 and any others it knows.
///
/// # Errors
/// When the session can't be read (a poisoned lock).
pub fn store(session: &MemorySession) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let home_dc = session.home_dc_id()?;
    let mut dc_options = Vec::new();
    for id in 1..=5 {
        if let Some(d) = session.dc_option(id)? {
            dc_options.push(d);
        }
    }
    Ok(serde_json::to_string(&Stored { home_dc, dc_options })?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_round_trips_and_needs_a_home_key() {
        let s = MemorySession::from(SessionData::default());
        let text = store(&s).unwrap();
        // a fresh session has no keys: refused
        assert!(matches!(load(&text), Err(Error::NoHomeKey(_))));
        let mut stored: Stored = serde_json::from_str(&text).unwrap();
        let home = stored.home_dc;
        for d in &mut stored.dc_options {
            if d.id == home {
                d.auth_key = Some([7u8; 256]);
            }
        }
        let text = serde_json::to_string(&stored).unwrap();
        let loaded = load(&text).unwrap();
        assert_eq!(loaded.home_dc_id().unwrap(), home);
        assert_eq!(loaded.dc_option(home).unwrap().unwrap().auth_key, Some([7u8; 256]));
        assert_eq!(store(&loaded).unwrap(), text);
    }
}
