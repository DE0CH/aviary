//! The operator page (hermit 3.11, 18): who may read it, the page itself (a static HTML file compiled in, with the
//! deployment's title, zone and node prefix filled in), its headers, and the sources the snapshot reads besides
//! hermit's own tables: magpie's ops routes, egress's ops summary (both consumed contracts, platform 15.3) and the
//! Kubernetes API (Deployments and pods of `ops.deployments`, Role `hermit-read`, 18.5).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::http::{HeaderValue, header};
use hermit_api::error::{self, ApiError, codes};
use hermit_api::server::Ctx;
use serde_json::Value;

use crate::Core;
use crate::fabric::Fut;

/// The page (ported from jarvis3 `web/ops-page.ts`).
const PAGE: &str = include_str!("ops_page.html");
/// The page's strict CSP (18.2, A3.F37).
pub const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; \
                       img-src 'self' data:; frame-ancestors 'none'";
/// Calls to magpie and egress (18.3).
pub const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(5);
/// The snapshot is cached this long per zone per replica (18.3).
pub const CACHE: Duration = Duration::from_secs(4);

/// The namespace's Deployments and the matching pods, as raw API objects; `None` from [`NoCluster`].
pub type ServiceObjects = Option<(Vec<Value>, Vec<Value>)>;

/// The Kubernetes reads of the `service` section (18.5): raw API objects, so the snapshot reads the fields the
/// API defines. `Ok(None)` only from [`NoCluster`] (tests); [`InCluster`] always reads or fails.
pub trait Cluster: Send + Sync + 'static {
    /// The namespace's Deployments (`apps/v1`, `items`) and the pods labelled `app in (apps)` (`v1`, `items`).
    fn service<'a>(&'a self, apps: &'a [String]) -> Fut<'a, Result<ServiceObjects, String>>;
}

/// The in-cluster API with the Pod's `ServiceAccount` (`hermit`): token and CA from the standard mount, read on every
/// call (projected tokens rotate).
#[derive(Debug)]
pub struct InCluster {
    namespace: String,
    dir: std::path::PathBuf,
}

const SA_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
const API: &str = "https://kubernetes.default.svc";

impl InCluster {
    #[must_use]
    pub fn new(namespace: String) -> Self {
        Self {
            namespace,
            dir: SA_DIR.into(),
        }
    }

    /// The client and the current token. hermit serves only inside the cluster, so a token it cannot read
    /// (automount off, a wrong mount) is an error of the `service` section, never "outside a cluster".
    fn client(&self) -> Result<(reqwest::Client, String), String> {
        use rustls::pki_types::CertificateDer;
        use rustls::pki_types::pem::PemObject as _;
        let token = std::fs::read_to_string(self.dir.join("token"))
            .map_err(|e| format!("the ServiceAccount token is not readable: {e}"))?;
        let mut roots = rustls::RootCertStore::empty();
        let certs = CertificateDer::pem_file_iter(self.dir.join("ca.crt")).map_err(|e| format!("ca.crt: {e}"))?;
        for c in certs {
            roots
                .add(c.map_err(|e| format!("ca.crt: {e}"))?)
                .map_err(|e| format!("ca.crt: {e}"))?;
        }
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth();
        let http = reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            .no_proxy()
            .timeout(UPSTREAM_TIMEOUT)
            .build()
            .map_err(|e| e.to_string())?;
        Ok((http, token.trim().to_owned()))
    }

    async fn list(http: &reqwest::Client, token: &str, url: &str) -> Result<Vec<Value>, String> {
        let res = http
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| format!("kubernetes: {e}"))?;
        let status = res.status();
        if !status.is_success() {
            return Err(format!("kubernetes: HTTP {status}"));
        }
        let v: Value = res.json().await.map_err(|e| format!("kubernetes: {e}"))?;
        Ok(v.get("items").and_then(Value::as_array).cloned().unwrap_or_default())
    }
}

impl Cluster for InCluster {
    fn service<'a>(&'a self, apps: &'a [String]) -> Fut<'a, Result<ServiceObjects, String>> {
        Box::pin(async move {
            let (http, token) = self.client()?;
            let ns = hermit_api::routes::encode_component(&self.namespace);
            let selector = hermit_api::routes::encode_component(&format!("app in ({})", apps.join(",")));
            let deps = format!("{API}/apis/apps/v1/namespaces/{ns}/deployments");
            let pods = format!("{API}/api/v1/namespaces/{ns}/pods?labelSelector={selector}");
            let (d, p) = tokio::join!(Self::list(&http, &token, &deps), Self::list(&http, &token, &pods));
            Ok(Some((d?, p?)))
        })
    }
}

/// No cluster (tests that do not look at the `service` section).
#[derive(Debug, Default)]
pub struct NoCluster;

impl Cluster for NoCluster {
    fn service<'a>(&'a self, _apps: &'a [String]) -> Fut<'a, Result<ServiceObjects, String>> {
        Box::pin(async { Ok(None) })
    }
}

/// Where the snapshot's other sources are: magpie (`http://magpie`, the ops reads and the person purge) and egress
/// (`http://<fabric.egressService>`, its ops summary), each with hermit's pair token (8.4), and the cluster.
#[derive(Clone)]
pub struct Upstreams {
    pub magpie: magpie_api::client::Client,
    pub egress: String,
    pub egress_token: String,
    pub cluster: Arc<dyn Cluster>,
}

impl std::fmt::Debug for Upstreams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upstreams")
            .field("egress", &self.egress)
            .finish_non_exhaustive()
    }
}

/// The HTTP client of magpie and egress calls (5 s, no proxy).
#[must_use]
pub fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .tls_backend_preconfigured(aviary_core::http::tls_config())
        .no_proxy()
        .timeout(UPSTREAM_TIMEOUT)
        .build()
        .unwrap_or_default()
}

impl Upstreams {
    /// The fixed names of hermit's YYC deployment (8.4, 18.4).
    #[must_use]
    pub fn new(
        magpie_url: &str,
        magpie_token: String,
        egress_url: &str,
        egress_token: String,
        cluster: Arc<dyn Cluster>,
    ) -> Self {
        Self {
            magpie: magpie_api::client::Client::new(http(), magpie_url, magpie_token),
            egress: egress_url.trim_end_matches('/').to_owned(),
            egress_token,
            cluster,
        }
    }

    /// egress's `GET /v1/ops/summary?tz=&recentMinutes=` (platform 15.3), as egress sends it: `budget` and `egress`
    /// reach the snapshot unchanged, so a field hermit's types do not name (such as `egress.recent[].quiet`) is
    /// kept. `recent` must parse as [`gateway_api::ops::Recent`] and `budget` and `egress` must be objects.
    ///
    /// # Errors
    /// A message for `errors[...]`.
    pub async fn egress_summary(&self, tz: &str, recent_minutes: u32) -> Result<EgressSummary, String> {
        let url = format!(
            "{}{}?tz={}&recentMinutes={recent_minutes}",
            self.egress,
            gateway_api::ops::OPS_SUMMARY,
            hermit_api::routes::encode_component(tz)
        );
        let res = http()
            .get(url)
            .bearer_auth(&self.egress_token)
            .header("aviary-request", uuid::Uuid::now_v7().to_string())
            .send()
            .await
            .map_err(|e| format!("egress unreachable: {e}"))?;
        let status = res.status();
        if !status.is_success() {
            return Err(format!("egress: HTTP {status}"));
        }
        let mut v: Value = res.json().await.map_err(|e| format!("egress: bad body: {e}"))?;
        let recent: gateway_api::ops::Recent =
            serde_json::from_value(v["recent"].take()).map_err(|e| format!("egress: bad body: recent: {e}"))?;
        for k in ["budget", "egress"] {
            if !v[k].is_object() {
                return Err(format!("egress: bad body: {k} is not an object"));
            }
        }
        Ok(EgressSummary {
            budget: v["budget"].take(),
            egress: v["egress"].take(),
            recent,
        })
    }
}

/// egress's ops summary: `budget` and `egress` as egress sent them, `recent` read by hermit.
#[derive(Clone, Debug)]
pub struct EgressSummary {
    pub budget: Value,
    pub egress: Value,
    pub recent: gateway_api::ops::Recent,
}

/// One replica's ops state: the upstreams and the snapshot cache (one entry per zone).
pub struct OpsState {
    pub up: Upstreams,
    cache: Mutex<HashMap<String, (Instant, Arc<Value>)>>,
}

impl std::fmt::Debug for OpsState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpsState").finish_non_exhaustive()
    }
}

impl OpsState {
    #[must_use]
    pub fn new(up: Upstreams) -> Self {
        Self {
            up,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The cached snapshot for `tz` when younger than 4 s.
    #[must_use]
    pub fn cached(&self, tz: &str) -> Option<Arc<Value>> {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(tz)
            .filter(|(at, _)| at.elapsed() < CACHE)
            .map(|(_, v)| v.clone())
    }

    pub fn store(&self, tz: &str, v: Arc<Value>) {
        let mut c = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        c.retain(|_, (at, _)| at.elapsed() < CACHE);
        c.insert(tz.to_owned(), (Instant::now(), v));
    }
}

/// Who may read `/ops/*` (3.11, 18.1): pigeon only for a person the current catalogue makes an admin (re-checked on
/// every request), the admin token, and for the snapshot the monitor token (the route table already limits the
/// monitor to it).
///
/// # Errors
/// 403 `not_admin`.
pub fn admit(core: &Core, ctx: &Ctx) -> Result<(), ApiError> {
    match ctx.caller {
        Some(hermit_api::routes::Caller::Pigeon) => {
            let person = ctx.person().and_then(hermit_api::people::normalise_email);
            if person
                .is_some_and(|p| core.catalogue.get().and_then(|c| c.role(&p)) == Some(hermit_api::people::Role::Admin))
            {
                Ok(())
            } else {
                Err(error::api(403, codes::NOT_ADMIN, "the operator page is for admins"))
            }
        }
        Some(hermit_api::routes::Caller::Admin | hermit_api::routes::Caller::Monitor) => Ok(()),
        _ => Err(error::api(403, codes::NOT_ADMIN, "the operator page is for admins")),
    }
}

/// The page with the deployment's values: `<product.name> Ops` and the default zone `ops.timeZone` (the snapshot
/// strips `ops.podNodePrefix` from node names). English throughout; no message contents. The values are JSON in
/// the script (escaped for it) and HTML-escaped in the title.
#[must_use]
pub fn page(core: &Core) -> String {
    let cfg = &core.cfg.file;
    // JSON inside <script>: `<` escaped so no value can close the element
    let js = |s: &str| {
        serde_json::to_string(s)
            .unwrap_or_else(|_| "\"\"".into())
            .replace('<', "\\u003c")
    };
    PAGE.replace("{{TITLE}}", &html_escape(&format!("{} Ops", cfg.product.name)))
        .replace("{{TZ_JSON}}", &js(&cfg.ops.time_zone))
        .replace("{{PRODUCT_JSON}}", &js(&cfg.product.name))
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// The page's own headers on `GET|HEAD /ops/` (18.2): the CSP and `no-referrer` (the contract router already sets
/// `no-store` and `nosniff`).
pub async fn page_headers(req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    let is_page = req.uri().path() == hermit_api::routes::OPS_PAGE;
    let mut res = next.run(req).await;
    if is_page && res.status().is_success() {
        let h = res.headers_mut();
        h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
        h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    }
    res
}

/// A time zone the snapshot may use (18.3): an IANA name `chrono-tz` knows, else `fallback`.
#[must_use]
pub fn safe_zone(tz: Option<&str>, fallback: &str) -> String {
    match tz.map(str::trim) {
        Some(z) if !z.is_empty() && z.len() < 64 && z.parse::<chrono_tz::Tz>().is_ok() => z.to_owned(),
        _ => fallback.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones() {
        let hk = "Asia/Hong_Kong";
        assert_eq!(safe_zone(Some("Europe/London"), hk), "Europe/London");
        assert_eq!(
            safe_zone(Some("America/Argentina/Buenos_Aires"), hk),
            "America/Argentina/Buenos_Aires"
        );
        assert_eq!(safe_zone(Some("x'; drop table"), hk), hk);
        assert_eq!(safe_zone(Some(""), hk), hk);
        assert_eq!(safe_zone(Some("Mars/Olympus"), hk), hk);
        assert_eq!(safe_zone(None, hk), hk);
    }

    #[tokio::test]
    async fn an_unreadable_service_account_token_is_an_error_not_no_cluster() {
        let c = InCluster {
            namespace: "ns".into(),
            dir: "/nonexistent/serviceaccount".into(),
        };
        let e = c.service(&["hermit".to_owned()]).await.unwrap_err();
        assert!(e.starts_with("the ServiceAccount token is not readable"), "{e}");
    }
}
