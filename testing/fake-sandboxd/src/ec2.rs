//! The EC2 calls hermit makes (`RunInstances`, `DescribeInstances`, `TerminateInstances`) in EC2's query protocol,
//! with host Pods as instances (the crate docs). The cluster is the state: an instance is a Pod labelled with its id,
//! its tags an annotation, its user data a Secret the Pod mounts as its config.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Form, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use k8s_openapi::api::core::v1::{Pod, Secret};
use kube::Api;
use kube::api::{DeleteParams, ListParams, PostParams};

use crate::host::{Facts, HostFile, host_pod_labels};
use crate::{HOST_LABEL, IP_ANNOTATION, iso, iso_now};

const INSTANCE_LABEL: &str = "fake-sandboxd/instance";
const TAGS_ANNOTATION: &str = "fake-sandboxd/tags";
const NS: &str = "http://ec2.amazonaws.com/doc/2016-11-15/";

/// What `ec2` needs to start a host Pod.
#[derive(Debug, Clone)]
pub struct Options {
    /// The image with `fake-sandboxd` on its PATH (aviary-testing).
    pub host_image: String,
    /// The service account host Pods run as (Pods: create, get, list, delete; Pods/log: get).
    pub service_account: String,
    /// What the hosts report (passed to each host Pod as flags).
    pub facts: Facts,
}

struct Ec2 {
    opts: Options,
    pods: Api<Pod>,
    secrets: Api<Secret>,
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Response><Errors><Error><Code>{}</Code><Message>{}</Message></Error></Errors><RequestID>{}</RequestID></Response>",
        xml_escape(code),
        xml_escape(message),
        request_id()
    );
    (status, [("content-type", "text/xml;charset=UTF-8")], body).into_response()
}

fn ok(action: &str, inner: &str) -> Response {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<{action}Response xmlns=\"{NS}\"><requestId>{}</requestId>{inner}</{action}Response>",
        request_id()
    );
    ([("content-type", "text/xml;charset=UTF-8")], body).into_response()
}

fn request_id() -> String {
    let h = random_hex(16);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

fn random_hex(n: usize) -> String {
    use ring::rand::SecureRandom as _;
    let mut b = vec![0u8; n];
    let _ = ring::rand::SystemRandom::new().fill(&mut b);
    b.iter().fold(String::new(), |mut s, x| {
        use std::fmt::Write as _;
        let _ = write!(s, "{x:02x}");
        s
    })
}

/// The query's indexed members `<prefix>.<n>.<rest>`, by `n`, as maps of `rest` to value.
fn members(q: &[(String, String)], prefix: &str) -> BTreeMap<u32, BTreeMap<String, String>> {
    let mut out: BTreeMap<u32, BTreeMap<String, String>> = BTreeMap::new();
    for (k, v) in q {
        let Some(rest) = k.strip_prefix(prefix).and_then(|r| r.strip_prefix('.')) else {
            continue;
        };
        let (n, field) = rest.split_once('.').unwrap_or((rest, ""));
        if let Ok(n) = n.parse() {
            out.entry(n).or_default().insert(field.to_owned(), v.clone());
        }
    }
    out
}

fn get<'a>(q: &'a [(String, String)], key: &str) -> Option<&'a str> {
    q.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// One instance as EC2 describes it.
#[derive(Debug, Clone)]
struct Inst {
    id: String,
    state: &'static str,
    ip: String,
    launched: String,
    tags: Vec<(String, String)>,
}

impl Inst {
    fn xml(&self) -> String {
        let code = match self.state {
            "pending" => 0,
            "running" => 16,
            "shutting-down" => 32,
            _ => 48,
        };
        let tags = self.tags.iter().fold(String::new(), |mut s, (k, v)| {
            let _ = write!(
                s,
                "<item><key>{}</key><value>{}</value></item>",
                xml_escape(k),
                xml_escape(v)
            );
            s
        });
        format!(
            "<item><instanceId>{}</instanceId><instanceState><code>{code}</code><name>{}</name></instanceState><privateIpAddress>{}</privateIpAddress><launchTime>{}</launchTime><tagSet>{tags}</tagSet></item>",
            self.id, self.state, self.ip, self.launched
        )
    }
}

fn inst_of(p: &Pod) -> Option<Inst> {
    let id = p.metadata.labels.as_ref()?.get(INSTANCE_LABEL)?.clone();
    let ann = p.metadata.annotations.as_ref()?;
    let ip = ann
        .get(IP_ANNOTATION)
        .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
        .and_then(|v| v.into_iter().next())
        .unwrap_or_default();
    let tags: Vec<(String, String)> = ann
        .get(TAGS_ANNOTATION)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let phase = p.status.as_ref().and_then(|s| s.phase.as_deref()).unwrap_or_default();
    let state = if p.metadata.deletion_timestamp.is_some() {
        "shutting-down"
    } else if phase == "Running" {
        "running"
    } else if phase == "Failed" || phase == "Succeeded" {
        "stopped"
    } else {
        "pending"
    };
    let launched = p
        .metadata
        .creation_timestamp
        .as_ref()
        .and_then(|t| time::OffsetDateTime::from_unix_timestamp(t.0.as_second()).ok())
        .map_or_else(iso_now, iso);
    Some(Inst {
        id,
        state,
        ip,
        launched,
        tags,
    })
}

// a failed call's answer is the EC2 error document itself
#[allow(clippy::result_large_err)]
impl Ec2 {
    async fn instances(&self) -> Result<Vec<Inst>, Response> {
        let l = self
            .pods
            .list(&ListParams::default().labels("app=fake-host"))
            .await
            .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, "InternalError", &e.to_string()))?;
        Ok(l.items.iter().filter_map(inst_of).collect())
    }

    fn host_pod(&self, file: &HostFile, id: &str, tags: &[(String, String)]) -> Result<Pod, String> {
        let name = format!("host-{}", file.host.name);
        let f = &self.opts.facts;
        let mut labels = host_pod_labels(&file.host.name);
        labels.insert(INSTANCE_LABEL.to_owned(), id.to_owned());
        let pod = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": name,
                "labels": labels,
                "annotations": {
                    IP_ANNOTATION: format!("[\"{}\"]", file.host.wg_address),
                    TAGS_ANNOTATION: serde_json::to_string(tags).map_err(|e| e.to_string())?,
                },
            },
            "spec": {
                "serviceAccountName": self.opts.service_account,
                "restartPolicy": "Always",
                "terminationGracePeriodSeconds": 5,
                "enableServiceLinks": false,
                "containers": [{
                    "name": "sandboxd",
                    "image": self.opts.host_image,
                    "imagePullPolicy": "IfNotPresent",
                    "command": ["fake-sandboxd", "host", "--config", "/etc/fake-sandboxd/host.json",
                        "--namespace", f.namespace, "--jfs-root", f.jfs_root,
                        "--cpus", f.cpus.to_string(), "--mem-total", f.mem_total.to_string(),
                        "--cpu-features", f.cpu_features, "--runsc", f.runsc,
                        "--gvisor-release", f.gvisor_release, "--sandboxd-version", f.version],
                    "volumeMounts": [{"name": "config", "mountPath": "/etc/fake-sandboxd", "readOnly": true}],
                    "readinessProbe": {"tcpSocket": {"port": sandboxd_api::PORT}, "periodSeconds": 1},
                }],
                "volumes": [{"name": "config", "secret": {"secretName": name}}],
            },
        });
        serde_json::from_value(pod).map_err(|e| e.to_string())
    }

    async fn run(&self, q: &[(String, String)]) -> Response {
        let Some(ud) = get(q, "UserData") else {
            return error(StatusCode::BAD_REQUEST, "MissingParameter", "UserData");
        };
        let Ok(ud) = STANDARD.decode(ud) else {
            return error(
                StatusCode::BAD_REQUEST,
                "InvalidParameterValue",
                "UserData is not base64",
            );
        };
        let file: HostFile = match serde_json::from_slice(&ud) {
            Ok(f) => f,
            Err(e) => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "InvalidParameterValue",
                    &format!("user data: {e}"),
                );
            }
        };
        // the tags of the instance's specification (hermit tags the volume the same)
        let mut tags = Vec::new();
        for spec in members(q, "TagSpecification").values() {
            if spec.get("ResourceType").map(String::as_str) != Some("instance") {
                continue;
            }
            let mut by_n: BTreeMap<u32, (String, String)> = BTreeMap::new();
            for (k, v) in spec {
                let Some(rest) = k.strip_prefix("Tag.") else { continue };
                let Some((n, field)) = rest.split_once('.') else {
                    continue;
                };
                let Ok(n) = n.parse() else { continue };
                let e = by_n.entry(n).or_default();
                match field {
                    "Key" => e.0.clone_from(v),
                    "Value" => e.1.clone_from(v),
                    _ => {}
                }
            }
            tags.extend(by_n.into_values());
        }
        let id = format!("i-{}", &random_hex(9)[..17]);
        let name = format!("host-{}", file.host.name);
        let secret: Secret = match serde_json::from_value(serde_json::json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": {"name": name, "labels": {"app": "fake-host", HOST_LABEL: name}},
            "data": {"host.json": STANDARD.encode(&ud)},
        })) {
            Ok(s) => s,
            Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, "InternalError", &e.to_string()),
        };
        let pod = match self.host_pod(&file, &id, &tags) {
            Ok(p) => p,
            Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, "InternalError", &e),
        };
        if let Err(e) = self.secrets.create(&PostParams::default(), &secret).await {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalError",
                &format!("secret: {e}"),
            );
        }
        let created = match self.pods.create(&PostParams::default(), &pod).await {
            Ok(p) => p,
            Err(e) => {
                let _ = self.secrets.delete(&name, &DeleteParams::default()).await;
                return error(StatusCode::INTERNAL_SERVER_ERROR, "InternalError", &format!("pod: {e}"));
            }
        };
        eprintln!("fake-ec2: {id} = {name} at {}", file.host.wg_address);
        let inst = inst_of(&created).unwrap_or(Inst {
            id: id.clone(),
            state: "pending",
            ip: file.host.wg_address.clone(),
            launched: iso_now(),
            tags,
        });
        ok(
            "RunInstances",
            &format!(
                "<reservationId>r-{}</reservationId><ownerId>000000000000</ownerId><instancesSet>{}</instancesSet>",
                &random_hex(9)[..17],
                inst.xml()
            ),
        )
    }

    async fn describe(&self, q: &[(String, String)]) -> Response {
        let filters: Vec<(String, Vec<String>)> = members(q, "Filter")
            .into_values()
            .map(|f| {
                let name = f.get("Name").cloned().unwrap_or_default();
                let values = f
                    .iter()
                    .filter(|(k, _)| k.starts_with("Value."))
                    .map(|(_, v)| v.clone())
                    .collect();
                (name, values)
            })
            .collect();
        let insts = match self.instances().await {
            Ok(i) => i,
            Err(r) => return r,
        };
        let items = insts
            .iter()
            .filter(|i| {
                filters.iter().all(|(name, values)| {
                    if name == "instance-state-name" {
                        values.iter().any(|v| v == i.state)
                    } else if let Some(key) = name.strip_prefix("tag:") {
                        i.tags.iter().any(|(k, v)| k == key && values.contains(v))
                    } else {
                        true
                    }
                })
            })
            .fold(String::new(), |mut s, i| {
                let _ = write!(
                    s,
                    "<item><reservationId>r-0</reservationId><ownerId>000000000000</ownerId><instancesSet>{}</instancesSet></item>",
                    i.xml()
                );
                s
            });
        ok(
            "DescribeInstances",
            &format!("<reservationSet>{items}</reservationSet>"),
        )
    }

    async fn terminate(&self, q: &[(String, String)]) -> Response {
        let ids: Vec<String> = members(q, "InstanceId")
            .into_values()
            .filter_map(|m| m.get("").cloned())
            .collect();
        let insts = match self.instances().await {
            Ok(i) => i,
            Err(r) => return r,
        };
        let pods = match self.pods.list(&ListParams::default().labels("app=fake-host")).await {
            Ok(l) => l.items,
            Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, "InternalError", &e.to_string()),
        };
        let mut items = String::new();
        for id in &ids {
            let pod = pods
                .iter()
                .find(|p| p.metadata.labels.as_ref().and_then(|l| l.get(INSTANCE_LABEL)) == Some(id));
            let prev = insts.iter().find(|i| &i.id == id).map_or("terminated", |i| i.state);
            if let Some(name) = pod.and_then(|p| p.metadata.name.clone()) {
                let _ = self.pods.delete(&name, &DeleteParams::default()).await;
                let _ = self.secrets.delete(&name, &DeleteParams::default()).await;
                eprintln!("fake-ec2: {id} ({name}) terminated");
            }
            let _ = write!(
                items,
                "<item><instanceId>{id}</instanceId><currentState><code>32</code><name>shutting-down</name></currentState><previousState><code>16</code><name>{prev}</name></previousState></item>"
            );
        }
        ok("TerminateInstances", &format!("<instancesSet>{items}</instancesSet>"))
    }
}

async fn handle(State(e): State<Arc<Ec2>>, Form(q): Form<Vec<(String, String)>>) -> Response {
    match get(&q, "Action") {
        Some("RunInstances") => e.run(&q).await,
        Some("DescribeInstances") => e.describe(&q).await,
        Some("TerminateInstances") => e.terminate(&q).await,
        other => error(
            StatusCode::BAD_REQUEST,
            "InvalidAction",
            &format!("the harness's EC2 does not do {}", other.unwrap_or("nothing")),
        ),
    }
}

/// Serves the EC2 calls until the process ends.
///
/// # Errors
/// No cluster credentials, a bad certificate or a port in use.
pub async fn run(opts: Options, listen: std::net::SocketAddr, id: crate::tls::Identity) -> anyhow::Result<()> {
    let client = kube::Client::try_default().await?;
    let ns = opts.facts.namespace.clone();
    let state = Arc::new(Ec2 {
        opts,
        pods: Api::namespaced(client.clone(), &ns),
        secrets: Api::namespaced(client, &ns),
    });
    let router = Router::new().route("/", post(handle)).with_state(state);
    let listener = tokio::net::TcpListener::bind(listen).await?;
    eprintln!("fake-ec2 on {listen}");
    crate::tls::serve(listener, router, &id).await
}
