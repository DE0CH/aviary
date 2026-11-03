//! The end-to-end harness driver (platform 7.8): speaks Paseo to pigeon as the apps do, mints Access tokens at
//! fake-access, scripts mock-model, reads magpie's and hermit's admin routes, and probes Pods through the Kubernetes
//! API. Each scenario is a function of [`Harness`]; `aviary-e2e` runs the ones named (all by default) and fails on the
//! first that fails, after printing what it saw.

#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)] // a test driver: every error ends the run

pub mod scenarios;
pub mod ws;

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context as _, bail};
use k8s_openapi::api::core::v1::Pod;
use kube::Api;
use kube::api::{AttachParams, ListParams};
use serde_json::{Value, json};

/// The harness's namespace.
pub const NS: &str = "aviary-e2e";
/// pigeon's Service.
pub const PIGEON: &str = "http://pigeon";
/// magpie's Service.
pub const MAGPIE: &str = "http://magpie";
/// hermit's Service.
pub const HERMIT: &str = "http://hermit";
/// mock-model's Service.
pub const MOCK: &str = "http://mock-model";
/// fake-access (Access's team domain in the harness).
pub const ACCESS: &str = "https://access.aviary-e2e.svc.cluster.local";
/// The Access application's audience (testing/e2e/hermit.json).
pub const AUD: &str = "e2e0000000000000000000000000000000000000000000000000000000000e2e";
/// The model Claude Code's turns use (the models route's default, gateway-api's fixture).
pub const TURN_MODEL: &str = "example/model-large-1017";
/// Where the harness's `JuiceFS` mount is (the driver Pod mounts the node's copy read-only).
pub const JFS: &str = "/mnt/aviary/jfs";
/// An allowed person, an admin and someone the catalogue does not know.
pub const ALICE: &str = "alice@example.org";
pub const ADMIN: &str = "admin@example.org";
pub const STRANGER: &str = "mallory@elsewhere.test";

/// The driver's state: one HTTP client, the admin tokens and the cluster.
pub struct Harness {
    pub http: reqwest::Client,
    pub env: BTreeMap<String, String>,
    pub kube: kube::Client,
}

impl std::fmt::Debug for Harness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Harness(..)")
    }
}

/// Reads `KEY=VALUE` lines.
pub fn read_env(path: &std::path::Path) -> anyhow::Result<BTreeMap<String, String>> {
    let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
    Ok(text
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect())
}

/// Waits until `f` answers `Some`, polling every `every`, at most `within`.
pub async fn eventually<T, F, Fut>(what: &str, within: Duration, every: Duration, mut f: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Option<T>>>,
{
    let until = tokio::time::Instant::now() + within;
    let mut last = String::new();
    loop {
        match f().await {
            Ok(Some(v)) => return Ok(v),
            Ok(None) => {}
            Err(e) => last = format!("{e:#}"),
        }
        if tokio::time::Instant::now() > until {
            bail!("{what}: not within {within:?} (last error: {last})");
        }
        tokio::time::sleep(every).await;
    }
}

impl Harness {
    /// The driver's client and cluster access.
    pub async fn new(env: BTreeMap<String, String>) -> anyhow::Result<Self> {
        let http = aviary_core::http::client_builder(&aviary_core::version::Build::new("aviary-e2e"), None)
            .timeout(Duration::from_secs(60))
            .build()?;
        let kube = kube::Client::try_default().await?;
        Ok(Self { http, env, kube })
    }

    pub fn token_of(&self, key: &str) -> anyhow::Result<&str> {
        self.env
            .get(key)
            .map(String::as_str)
            .with_context(|| format!("{key} missing"))
    }

    /// An Access token for `email` from fake-access (`kid` default: the first published key).
    pub async fn mint(&self, email: &str, exp_seconds: i64, kid: Option<&str>) -> anyhow::Result<String> {
        let name = email.split('@').next().unwrap_or(email);
        let mut body = json!({"email": email, "name": name, "aud": AUD, "expSeconds": exp_seconds});
        if let Some(k) = kid {
            body["kid"] = json!(k);
        }
        let r = self.http.post(format!("{ACCESS}/__mint")).json(&body).send().await?;
        let v: Value = r.error_for_status()?.json().await?;
        Ok(v["token"].as_str().context("no token")?.to_owned())
    }

    /// Publishes (or retires) a key of fake-access's JWKS.
    pub async fn publish_key(&self, kid: &str, published: bool) -> anyhow::Result<()> {
        self.http
            .post(format!("{ACCESS}/__keys"))
            .json(&json!({"kid": kid, "published": published}))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// A request to pigeon as the holder of `token` (the iOS app's header).
    pub fn pigeon(&self, method: reqwest::Method, path: &str, token: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{PIGEON}{path}"))
            .header("cf-access-token", token)
    }

    /// magpie's admin surface.
    pub fn magpie_admin(&self, path: &str) -> anyhow::Result<reqwest::RequestBuilder> {
        Ok(self
            .http
            .get(format!("{MAGPIE}{path}"))
            .header("aviary-protocol", "magpie-person=1")
            .header("aviary-admin", ADMIN)
            .bearer_auth(self.token_of("E2E_MAGPIE_ADMIN")?))
    }

    /// hermit with the monitor's token.
    pub fn hermit_monitor(&self, path: &str) -> anyhow::Result<reqwest::RequestBuilder> {
        Ok(self
            .http
            .get(format!("{HERMIT}{path}"))
            .header("aviary-protocol", "hermit-core=1")
            .bearer_auth(self.token_of("E2E_HERMIT_MONITOR")?))
    }

    /// Queues a mock-model reply.
    pub async fn script(&self, entry: Value) -> anyhow::Result<()> {
        self.http
            .post(format!("{MOCK}/__script"))
            .json(&entry)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// What mock-model received for a conversation.
    pub async fn model_requests(&self, conversation: &str) -> anyhow::Result<Vec<Value>> {
        let v: Value = self
            .http
            .get(format!("{MOCK}/__requests?conversation={conversation}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    fn pods(&self) -> Api<Pod> {
        Api::namespaced(self.kube.clone(), NS)
    }

    /// The running sandbox Pod of a conversation (by the hostPath of its `/workspace`).
    pub async fn sandbox_pod(&self, conversation: &str) -> anyhow::Result<Option<Pod>> {
        let l = self.pods().list(&ListParams::default().labels("app=sandbox")).await?;
        Ok(l.items.into_iter().find(|p| {
            p.metadata.deletion_timestamp.is_none()
                && p.spec.as_ref().is_some_and(|s| {
                    s.volumes.iter().flatten().any(|v| {
                        v.host_path
                            .as_ref()
                            .is_some_and(|h| h.path.ends_with(&format!("/ws/{conversation}")))
                    })
                })
        }))
    }

    /// The pod IP of one Pod of `app`.
    pub async fn pod_ip(&self, app: &str) -> anyhow::Result<String> {
        let l = self
            .pods()
            .list(&ListParams::default().labels(&format!("app={app}")))
            .await?;
        l.items
            .iter()
            .find_map(|p| p.status.as_ref()?.pod_ip.clone())
            .with_context(|| format!("no pod of {app}"))
    }

    /// Runs `cmd` in a Pod's first container: (exit status 0?, stdout + stderr).
    pub async fn exec(&self, pod: &str, cmd: &[&str]) -> anyhow::Result<(bool, String)> {
        use tokio::io::AsyncReadExt as _;
        let ap = AttachParams::default().stdout(true).stderr(true).stdin(false);
        let mut p = self.pods().exec(pod, cmd.iter().copied(), &ap).await?;
        let mut out = String::new();
        if let Some(mut s) = p.stdout() {
            s.read_to_string(&mut out).await?;
        }
        if let Some(mut s) = p.stderr() {
            s.read_to_string(&mut out).await?;
        }
        let status = match p.take_status() {
            Some(s) => s.await,
            None => None,
        };
        let ok = status.as_ref().and_then(|s| s.status.as_deref()) == Some("Success");
        Ok((ok, out))
    }

    /// Deletes a Pod (grace `grace` s).
    pub async fn delete_pod(&self, pod: &str, grace: u32) -> anyhow::Result<()> {
        let dp = kube::api::DeleteParams {
            grace_period_seconds: Some(grace),
            ..Default::default()
        };
        self.pods().delete(pod, &dp).await?;
        Ok(())
    }
}
