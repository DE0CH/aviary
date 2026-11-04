//! The old cluster's Kubernetes API for the running sets (cutover 6.3, 7.2; bridge B7): the `app=ui` pods of
//! namespace `jarvis` listed from the API in every phase (Terminating ones included), and `pods/exec` into each
//! one's container `ui` over the API's WebSocket exec protocol (`v4.channel.k8s.io`: binary frames whose first byte
//! is the stream, 1 stdout, 2 stderr, 3 the final status), and `pods/proxy` to the old router's switch module (the
//! gate's report, the reverse hand-over's [`restart`]). Plain HTTP and `reqwest`/`tungstenite` only: the
//! Kubernetes client crates are hermit's (platform 4.5).
//!
//! Bindings: `MAGPIE_IMPORT_LEGACY_KUBE_URL` (the old API's address), `MAGPIE_IMPORT_LEGACY_KUBE_TOKEN` (the
//! `ServiceAccount` token of `jarvis/magpie-import`), and the old cluster's CA at [`CA_FILE`] (`ConfigMap`
//! `legacy-kube-ca`).

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde::Deserialize;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

use crate::holds::{self, RunningSet};
use crate::{Error, Result};

/// Where the old cluster's CA is mounted.
pub const CA_FILE: &str = "/etc/legacy-kube-ca/ca.crt";

/// The namespace and selector of the old ui pods.
pub const NAMESPACE: &str = "jarvis";
/// The old ui pods' label.
pub const SELECTOR: &str = "app=ui";
/// The container `holds.js` runs in.
pub const CONTAINER: &str = "ui";
/// The old router's port in a ui pod.
pub const UI_PORT: u16 = 8080;
/// The header the old router's switch module also takes its token in (claudecodeui `jarvis/router.mjs`).
pub const SWITCH_TOKEN_HEADER: &str = "x-jarvis-switch-token";
/// One exec's bound (cutover 6.3).
pub const EXEC_TIMEOUT: Duration = Duration::from_secs(8);

/// The old API.
#[derive(Debug, Clone)]
pub struct Kube {
    base: String,
    token: String,
    tls: Option<Arc<rustls::ClientConfig>>,
    http: reqwest::Client,
}

/// A ui pod.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pod {
    /// Its name.
    pub name: String,
    /// Being deleted (it can still hold children).
    pub terminating: bool,
}

fn err(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Source(format!("the old Kubernetes API: {what}: {e}"))
}

impl Kube {
    /// The API at `base` (`https://…`, or `http://…` in tests) with `token`, trusting only `ca_pem` for `https`.
    ///
    /// # Errors
    /// A CA that does not parse.
    pub fn new(base: &str, token: &str, ca_pem: Option<&[u8]>) -> Result<Self> {
        use rustls::pki_types::CertificateDer;
        use rustls::pki_types::pem::PemObject as _;
        let tls = match ca_pem {
            Some(pem) => {
                let mut roots = rustls::RootCertStore::empty();
                for c in CertificateDer::pem_slice_iter(pem) {
                    roots
                        .add(c.map_err(|e| err("the CA", e))?)
                        .map_err(|e| err("the CA", e))?;
                }
                let cfg =
                    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                        .with_safe_default_protocol_versions()
                        .map_err(|e| err("TLS", e))?
                        .with_root_certificates(roots)
                        .with_no_client_auth();
                Some(Arc::new(cfg))
            }
            None => None,
        };
        let build = aviary_core::version::Build::new("magpie-import");
        let mut b = aviary_core::http::client_builder(&build, None).timeout(Duration::from_secs(10));
        if let Some(cfg) = &tls {
            b = b.tls_backend_preconfigured((**cfg).clone());
        }
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            token: token.to_owned(),
            tls,
            http: b.build().map_err(|e| err("client", e))?,
        })
    }

    /// From the bindings.
    ///
    /// # Errors
    /// An unreadable or malformed CA.
    pub fn from_env() -> Result<Self> {
        let ca = std::fs::read(CA_FILE)?;
        Self::new(
            &aviary_core::env::required("MAGPIE_IMPORT_LEGACY_KUBE_URL"),
            &aviary_core::env::required("MAGPIE_IMPORT_LEGACY_KUBE_TOKEN"),
            Some(&ca),
        )
    }

    /// The ui pods, every phase.
    ///
    /// # Errors
    /// The API unreachable or refusing.
    pub async fn ui_pods(&self) -> Result<Vec<Pod>> {
        #[derive(Deserialize)]
        struct List {
            items: Vec<Item>,
        }
        #[derive(Deserialize)]
        struct Item {
            metadata: Meta,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Meta {
            name: String,
            deletion_timestamp: Option<String>,
        }
        let url = format!(
            "{}/api/v1/namespaces/{NAMESPACE}/pods?{}",
            self.base,
            url_query(&[("labelSelector", SELECTOR)])
        );
        let res = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| err("list pods", e))?;
        if !res.status().is_success() {
            return Err(err("list pods", res.status()));
        }
        let list: List = res.json().await.map_err(|e| err("list pods", e))?;
        Ok(list
            .items
            .into_iter()
            .map(|i| Pod {
                name: i.metadata.name,
                terminating: i.metadata.deletion_timestamp.is_some(),
            })
            .collect())
    }

    /// A call to the old router in `pod` through the API server's `pods/proxy` (DESIGN 15.5 B7): `method` on `path`
    /// (with its query) at [`UI_PORT`], the switch token in `x-jarvis-switch-token` (the API server keeps
    /// `Authorization` for its own login), within `bound`. The status and the body.
    ///
    /// # Errors
    /// The API unreachable or refusing the proxy, or no answer within `bound`.
    pub async fn proxy(
        &self,
        method: reqwest::Method,
        pod: &str,
        path: &str,
        switch_token: &str,
        bound: Duration,
    ) -> Result<(u16, String)> {
        let url = format!(
            "{}/api/v1/namespaces/{NAMESPACE}/pods/{pod}:{UI_PORT}/proxy{path}",
            self.base
        );
        let res = self
            .http
            .request(method, url)
            .bearer_auth(&self.token)
            .header(SWITCH_TOKEN_HEADER, switch_token)
            .timeout(bound)
            .send()
            .await
            .map_err(|e| err("pods/proxy", e))?;
        let status = res.status().as_u16();
        let body = res.text().await.map_err(|e| err("pods/proxy", e))?;
        Ok((status, body))
    }

    /// Runs `argv` in `pod`'s container `container`; its standard output, within `bound`.
    ///
    /// # Errors
    /// The exec refused, failed, timed out or exited non-zero.
    pub async fn exec(&self, pod: &str, container: &str, argv: &[&str], bound: Duration) -> Result<String> {
        tokio::time::timeout(bound, self.exec_inner(pod, container, argv))
            .await
            .map_err(|_| err("exec", "timed out"))?
    }

    async fn exec_inner(&self, pod: &str, container: &str, argv: &[&str]) -> Result<String> {
        let mut q = url_query(&[("container", container), ("stdout", "true"), ("stderr", "true")]);
        for a in argv {
            q.push('&');
            q.push_str(&url_query(&[("command", a)]));
        }
        let ws_base = self
            .base
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1);
        let url = format!("{ws_base}/api/v1/namespaces/{NAMESPACE}/pods/{pod}/exec?{q}");
        let mut req = url.into_client_request().map_err(|e| err("exec", e))?;
        let h = req.headers_mut();
        h.insert(
            "authorization",
            format!("Bearer {}", self.token).parse().map_err(|e| err("exec", e))?,
        );
        h.insert(
            "sec-websocket-protocol",
            "v4.channel.k8s.io".parse().map_err(|e| err("exec", e))?,
        );
        let uri = req.uri().clone();
        let host = uri
            .host()
            .ok_or_else(|| err("exec", "no host"))?
            .trim_matches(['[', ']'])
            .to_owned();
        let port = uri.port_u16().unwrap_or(if self.tls.is_some() { 443 } else { 80 });
        let tcp = tokio::net::TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|e| err("exec connect", e))?;
        if let Some(cfg) = &self.tls {
            let name = rustls::pki_types::ServerName::try_from(host).map_err(|e| err("exec", e))?;
            let tls = tokio_rustls::TlsConnector::from(cfg.clone())
                .connect(name, tcp)
                .await
                .map_err(|e| err("exec TLS", e))?;
            let (ws, _) = tokio_tungstenite::client_async(req, tls)
                .await
                .map_err(|e| err("exec", e))?;
            read_exec(ws).await
        } else {
            let (ws, _) = tokio_tungstenite::client_async(req, tcp)
                .await
                .map_err(|e| err("exec", e))?;
            read_exec(ws).await
        }
    }
}

/// Reads an exec's streams to its end: its standard output when the status is `Success`.
async fn read_exec<S>(mut ws: tokio_tungstenite::WebSocketStream<S>) -> Result<String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut out = Vec::new();
    let mut status: Option<String> = None;
    while let Some(m) = ws.next().await {
        match m.map_err(|e| err("exec", e))? {
            Message::Binary(b) if !b.is_empty() => match b[0] {
                1 => out.extend_from_slice(&b[1..]),
                3 => status = Some(String::from_utf8_lossy(&b[1..]).into_owned()),
                _ => {}
            },
            Message::Close(_) => break,
            Message::Ping(p) => {
                let _ = ws.send(Message::Pong(p)).await;
            }
            _ => {}
        }
    }
    // v4: the status is a metav1.Status; Success is the only good one (a non-zero exit is a Failure)
    let ok = status
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .is_some_and(|v| v.get("status").and_then(serde_json::Value::as_str) == Some("Success"));
    if !ok {
        return Err(err("exec", status.unwrap_or_else(|| "no status".to_owned())));
    }
    String::from_utf8(out).map_err(|e| err("exec output", e))
}

fn url_query(pairs: &[(&str, &str)]) -> String {
    fn enc(s: &str) -> String {
        s.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect()
    }
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Each ui pod's children (`holds.js` in every ui pod at once, each within [`EXEC_TIMEOUT`]): the pod and its read.
///
/// # Errors
/// The pods could not be listed.
pub async fn pod_reads(kube: &Kube) -> Result<Vec<(Pod, Result<Vec<holds::Child>>)>> {
    let pods = kube.ui_pods().await?;
    Ok(futures_util::future::join_all(pods.into_iter().map(|p| async move {
        let out = kube
            .exec(&p.name, CONTAINER, &["node", "-e", holds::SCRIPT], EXEC_TIMEOUT)
            .await;
        let read = match out {
            Ok(o) => holds::parse(&o),
            Err(e) => {
                tracing::warn!(pod = %p.name, terminating = p.terminating, error = %e, "running set not read");
                Err(e)
            }
        };
        (p, read)
    }))
    .await)
}

/// One running-set read (cutover 6.3): [`pod_reads`]; a pod that could not be listed, entered or read makes the read
/// incomplete.
pub async fn running_set(kube: &Kube) -> RunningSet {
    match pod_reads(kube).await {
        Ok(reads) => RunningSet::of(reads.into_iter().map(|(_, r)| r).collect()),
        Err(e) => {
            tracing::warn!(error = %e, "ui pods not listed");
            RunningSet::of(vec![Err(e)])
        }
    }
}

/// The old router's restart route (DESIGN 9.6, claudecodeui `jarvis/router.mjs`): stops the person's `CloudCLI` child
/// in that pod only if nothing of theirs is busy.
pub const RESTART: &str = "/__jarvis/restart";

/// What a pod's restart route answered (`{"result": "none" | "busy" | "stopped", "why"?}`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Restart {
    /// The pod has no child of the person.
    None,
    /// The child was stopped (its running set was empty and no request of the person was outstanding).
    Stopped,
    /// The child is busy: why.
    Busy(String),
}

/// `POST /__jarvis/restart?person=<email>` on `pod` through `pods/proxy`, within `bound`.
///
/// # Errors
/// The API unreachable, a status other than 200, an answer that does not parse.
pub async fn restart(kube: &Kube, pod: &str, switch_token: &str, person: &str, bound: Duration) -> Result<Restart> {
    #[derive(Deserialize)]
    struct Answer {
        result: String,
        #[serde(default)]
        why: Option<String>,
    }
    let path = format!("{RESTART}?{}", url_query(&[("person", person)]));
    let (status, body) = kube
        .proxy(reqwest::Method::POST, pod, &path, switch_token, bound)
        .await?;
    if status != 200 {
        return Err(err("restart", format!("{pod} answered {status}")));
    }
    let a: Answer = serde_json::from_str(&body).map_err(|e| err("restart", e))?;
    match a.result.as_str() {
        "none" => Ok(Restart::None),
        "stopped" => Ok(Restart::Stopped),
        "busy" => Ok(Restart::Busy(a.why.unwrap_or_default())),
        other => Err(err("restart", format!("{pod} answered {other:?}"))),
    }
}
