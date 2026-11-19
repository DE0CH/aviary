//! A session machine on aviary's fabric (DE0CH/jarvis4 docs/PLAN-fabric.md): the machine is a gVisor sandbox the
//! fabric hermit (aviary's own hermit, an internal service of Jarvis 4's cluster) started, and its id is that
//! sandbox's (`x` + 32 hex). The session has one supervisor, the one this hermit boots; the fabric hermit reaches it
//! on `:7070` with its own control token for that sandbox (health: the sandbox is ready; release: Claude Code stops
//! and everything is stored). So the descriptor's `control.tokenSha256` also carries the fabric hermit's hashes,
//! read at the machine's boot from the fabric hermit's own descriptor of the sandbox, as caller gateway.
//!
//! Env: `FABRIC_HERMIT_URL` (unset: no fabric machines; everything here is skipped) and
//! `FABRIC_HERMIT_GATEWAY_TOKEN` (the fabric hermit's pair token of the caller gateway).

use anyhow::Context as _;
use serde::Deserialize;
use uuid::Uuid;

/// The fabric hermit as this hermit reaches it.
#[derive(Clone, Debug)]
pub struct FabricHermit {
    pub url: String,
    pub gateway_token: String,
}

impl FabricHermit {
    /// From the environment; `None` when no fabric hermit is configured.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let url = std::env::var("FABRIC_HERMIT_URL")
            .ok()
            .filter(|u| !u.is_empty())?;
        let gateway_token = std::env::var("FABRIC_HERMIT_GATEWAY_TOKEN").unwrap_or_default();
        Some(Self {
            url: url.trim_end_matches('/').to_owned(),
            gateway_token,
        })
    }

    /// The fabric hermit's control-token hashes for the sandbox that is this machine: its liveness (the sandbox's
    /// conversation and person), then its descriptor, with the gateway's three identity headers.
    ///
    /// # Errors
    /// The fabric hermit does not answer, or does not know the sandbox as live.
    pub async fn control_hashes(
        &self,
        http: &reqwest::Client,
        sandbox: Uuid,
    ) -> anyhow::Result<Vec<String>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Liveness {
            conversation_id: Uuid,
            person: String,
            live: bool,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Control {
            token_sha256: Vec<String>,
        }
        #[derive(Deserialize)]
        struct SelfAnswer {
            control: Control,
        }
        let l: Liveness = http
            .get(format!("{}/v1/sandbox-tokens/{sandbox}", self.url))
            .bearer_auth(&self.gateway_token)
            .header("aviary-protocol", "hermit-core=1")
            .send()
            .await
            .context("the fabric hermit")?
            .error_for_status()
            .context("the fabric hermit's sandbox liveness")?
            .json()
            .await?;
        anyhow::ensure!(l.live, "the fabric hermit's sandbox {sandbox} is not live");
        let d: SelfAnswer = http
            .get(format!("{}/v1/sandbox/self", self.url))
            .bearer_auth(&self.gateway_token)
            .header("aviary-protocol", "hermit-sandbox=1")
            .header("aviary-sandbox-id", sandbox.to_string())
            .header("aviary-sandbox-conversation", l.conversation_id.to_string())
            .header("aviary-sandbox-person", l.person)
            .send()
            .await
            .context("the fabric hermit")?
            .error_for_status()
            .context("the fabric hermit's descriptor")?
            .json()
            .await?;
        anyhow::ensure!(
            !d.control.token_sha256.is_empty()
                && d.control
                    .token_sha256
                    .iter()
                    .all(|h| h.len() == 64 && h.bytes().all(|c| c.is_ascii_hexdigit())),
            "the fabric hermit's control hashes are not SHA-256 hex"
        );
        Ok(d.control.token_sha256)
    }
}

/// The fabric sandbox a machine id names (`x` + the sandbox's 32 hex digits), `None` for any other machine.
#[must_use]
pub fn sandbox_of_machine(machine: &str) -> Option<Uuid> {
    let hex = machine.strip_prefix('x')?;
    if hex.len() != 32
        || !hex
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return None;
    }
    Uuid::parse_str(hex).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_ids() {
        assert_eq!(
            sandbox_of_machine("x0190f6c25b1e7a3c9d4e1f2a3b4c5d6e"),
            Uuid::parse_str("0190f6c2-5b1e-7a3c-9d4e-1f2a3b4c5d6e").ok()
        );
        for no in [
            "0190f6c25b1e7a3c9d4e1f2a3b4c5d6e",
            "x0190F6C25B1E7A3C9D4E1F2A3B4C5D6E",
            "x0190",
            "i-0abc",
            "m1",
            "",
        ] {
            assert_eq!(sandbox_of_machine(no), None, "{no}");
        }
    }
}
