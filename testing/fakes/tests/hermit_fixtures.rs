//! fake-hermit passes hermit-api's golden fixtures: every response-shaped fixture, scripted as the answer of its
//! route, is read back unchanged by hermit-api's client through the guarded router; every error fixture arrives as
//! the client's refusal with that body; and the request-shaped fixtures sent by the client are what the fake
//! recorded.
#![allow(clippy::unwrap_used, clippy::too_many_lines)] // tests

use std::collections::BTreeSet;
use std::time::Duration;

use aviary_core::auth::Caller;
use aviary_fakes::FakeHermit;
use aviary_fakes::fixtures::hermit as fx;
use aviary_fakes::script::Reply;
use hermit_api::admin::PurgeRequest;
use hermit_api::exec::ExecRequest;
use hermit_api::people::WarmRequest;
use hermit_api::sandbox::{PutSandboxRequest, ReleaseSandboxRequest, SandboxEventsQuery, SandboxQuery};
use hermit_api::sandbox_surface::ActivityReport;
use hermit_api::workspaces::{CloneRequest, EntriesQuery};
use hermit_api::{ClientError, SandboxIdentity, SelfAnswer};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

const CID: Uuid = Uuid::from_u128(0x0190_f6c2_4a0d_7b2c_8e1f_0a1b_2c3d_4e5f);

fn typed<T: DeserializeOwned>(name: &str) -> T {
    serde_json::from_value(fx(name)).unwrap()
}

fn v<T: Serialize>(t: &T) -> Value {
    serde_json::to_value(t).unwrap()
}

fn identity() -> SandboxIdentity {
    SandboxIdentity {
        id: Uuid::from_u128(0x0190_f6c2_5b1e_7a3c_9d4e_1f2a_3b4c_5d6e),
        conversation: CID,
        person: "staff@example.org".into(),
    }
}

struct Rig {
    running: aviary_fakes::hermit::Running,
    http: reqwest::Client,
}

impl Rig {
    async fn new() -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Self {
            running: FakeHermit::new().serve().await.unwrap(),
            http: reqwest::Client::new(),
        }
    }

    fn as_(&self, caller: Caller) -> hermit_api::Client {
        self.running.client(self.http.clone(), caller)
    }

    fn once(&self, route: &str, status: u16, fixture: &str) {
        self.running.fake.script().once(route, Reply::json(status, fx(fixture)));
    }

    fn last(&self, route: &str) -> aviary_fakes::Call {
        self.running.fake.script().calls_to(route).pop().unwrap()
    }
}

#[tokio::test]
async fn every_response_fixture_reads_back_unchanged() {
    let rig = Rig::new().await;
    let magpie = rig.as_(Caller::Magpie);
    let pigeon = rig.as_(Caller::Pigeon);
    let gateway = rig.as_(Caller::Gateway).sandbox_identity(&identity());
    let admin = rig.as_(Caller::Admin).as_admin("admin@example.com");
    let mut used = BTreeSet::new();
    let mut check = |name: &str, got: Value| {
        assert_eq!(got, fx(name), "{name}");
        used.insert(name.to_owned());
    };

    for name in ["deployment", "deployment-no-app"] {
        rig.once("deployment", 200, name);
        check(name, v(&pigeon.deployment().await.unwrap()));
    }
    for name in ["person-staff", "person-admin", "person-not-allowed"] {
        rig.once("person", 200, name);
        check(name, v(&magpie.person("x@example.org").await.unwrap()));
    }
    rig.once("people", 200, "people");
    check("people", v(&magpie.people().await.unwrap()));
    rig.once("warm", 202, "warm-reply");
    check(
        "warm-reply",
        v(&pigeon.warm(&typed::<WarmRequest>("warm-request")).await.unwrap()),
    );
    assert_eq!(rig.last("warm").body.unwrap(), fx("warm-request"));

    let put: PutSandboxRequest = typed("put-sandbox-request");
    for (name, status) in [("sandbox-starting", 202), ("sandbox-ready", 200)] {
        rig.once("put_sandbox", status, name);
        let got = magpie.put_sandbox(CID, &put).await.unwrap();
        assert_eq!(got.status(), status, "{name}");
        check(name, v(got.sandbox()));
    }
    assert_eq!(rig.last("put_sandbox").body.unwrap(), fx("put-sandbox-request"));
    for name in ["sandbox-gone", "sandbox-never-ready"] {
        rig.once("get_sandbox", 200, name);
        let q: SandboxQuery = typed("query-sandbox");
        check(name, v(&magpie.get_sandbox(CID, q).await.unwrap()));
        assert_eq!(rig.last("get_sandbox").query.unwrap(), fx("query-sandbox"));
    }
    for name in ["release-sandbox-reply", "release-sandbox-reply-none"] {
        rig.once("release_sandbox", 200, name);
        let body: ReleaseSandboxRequest = typed("release-sandbox-request");
        check(name, v(&magpie.release_sandbox(CID, &body).await.unwrap()));
        assert_eq!(rig.last("release_sandbox").body.unwrap(), fx("release-sandbox-request"));
    }
    rig.once("snapshot_sandbox", 201, "snapshot-reply");
    check("snapshot-reply", v(&magpie.snapshot_sandbox(CID).await.unwrap()));
    for name in ["sandbox-events", "sandbox-events-empty"] {
        rig.once("sandbox_events", 200, name);
        let q: SandboxEventsQuery = typed("query-sandbox-events");
        check(name, v(&magpie.sandbox_events(q).await.unwrap()));
    }

    rig.once("get_workspace", 200, "workspace");
    check("workspace", v(&magpie.get_workspace(CID).await.unwrap()));
    rig.once("put_workspace", 201, "workspace");
    check("workspace", v(magpie.put_workspace(CID).await.unwrap().workspace()));
    rig.once("clone_workspace", 201, "workspace");
    let clone: CloneRequest = typed("clone-request");
    check("workspace", v(&magpie.clone_workspace(CID, &clone).await.unwrap()));
    assert_eq!(rig.last("clone_workspace").body.unwrap(), fx("clone-request"));
    for name in ["entries", "entries-last-page"] {
        rig.once("workspace_entries", 200, name);
        let q: EntriesQuery = typed("query-entries");
        check(name, v(&magpie.workspace_entries(CID, &q).await.unwrap()));
        assert_eq!(rig.last("workspace_entries").query.unwrap(), fx("query-entries"));
    }

    rig.once("token_keys", 200, "token-keys");
    check("token-keys", v(&gateway.token_keys().await.unwrap()));
    for name in ["token-liveness", "token-liveness-gone"] {
        rig.once("token_liveness", 200, name);
        let got = gateway
            .token_liveness(identity().id, Duration::from_secs(1))
            .await
            .unwrap();
        check(name, v(&got));
    }

    rig.once("sandbox_self", 200, "sandbox-self");
    match gateway.sandbox_self(None).await.unwrap() {
        SelfAnswer::Fresh(d) => check("sandbox-self", v(&*d)),
        other @ SelfAnswer::NotModified { .. } => panic!("{other:?}"),
    }
    for name in ["secret-keys", "secret-keys-granted"] {
        rig.once("sandbox_secrets", 200, name);
        check(name, v(&gateway.sandbox_secrets().await.unwrap()));
    }
    rig.once("sandbox_secret", 200, "secret-value");
    check("secret-value", v(&gateway.sandbox_secret("API_KEY").await.unwrap()));
    for name in ["activity-reply-ready", "activity-reply-releasing"] {
        for report in ["activity-report", "activity-report-idle"] {
            rig.once("sandbox_activity", 200, name);
            let body: ActivityReport = typed(report);
            check(name, v(&gateway.sandbox_activity(&body).await.unwrap()));
            assert_eq!(rig.last("sandbox_activity").body.unwrap(), fx(report));
        }
    }

    rig.once("admin_exec", 200, "exec-result");
    let exec: ExecRequest = typed("exec-request");
    check("exec-result", v(&admin.admin_exec(identity().id, &exec).await.unwrap()));
    assert_eq!(rig.last("admin_exec").body.unwrap(), fx("exec-request"));
    rig.once("admin_purge", 202, "admin-purge-reply");
    let purge: PurgeRequest = typed("admin-purge-request");
    check(
        "admin-purge-reply",
        v(&admin.admin_purge("gone@example.org", &purge).await.unwrap()),
    );
    assert_eq!(rig.last("admin_purge").body.unwrap(), fx("admin-purge-request"));
    for (route, name) in [
        ("admin_patch_host", "admin-host-patch"),
        ("admin_patch_host", "admin-host-patch-uncap"),
        ("admin_patch_host", "admin-host-patch-retire"),
    ] {
        rig.running
            .fake
            .script()
            .once(route, Reply::ok(serde_json::json!({"ok": true})));
        admin.admin_patch_host("aws-1", &typed(name)).await.unwrap();
        assert_eq!(rig.last(route).body.unwrap(), fx(name), "{name}");
    }
    for (route, name) in [
        ("admin_sandboxes_enabled", "admin-sandboxes-enabled"),
        ("admin_fresh_host", "admin-fresh-host"),
        ("admin_release", "admin-release"),
        ("admin_recycle", "admin-recycle"),
    ] {
        rig.running
            .fake
            .script()
            .once(route, Reply::ok(serde_json::json!({"ok": true})));
        match route {
            "admin_sandboxes_enabled" => admin.admin_sandboxes_enabled(&typed(name)).await.map(|_| ()),
            "admin_fresh_host" => admin.admin_fresh_host(&typed(name)).await.map(|_| ()),
            "admin_release" => admin.admin_release(identity().id, &typed(name)).await.map(|_| ()),
            _ => admin.admin_recycle(&typed(name)).await.map(|_| ()),
        }
        .unwrap();
        assert_eq!(rig.last(route).body.unwrap(), fx(name), "{name}");
    }

    let responses: BTreeSet<String> = [
        "deployment",
        "deployment-no-app",
        "person-staff",
        "person-admin",
        "person-not-allowed",
        "people",
        "warm-reply",
        "sandbox-starting",
        "sandbox-ready",
        "sandbox-gone",
        "sandbox-never-ready",
        "release-sandbox-reply",
        "release-sandbox-reply-none",
        "snapshot-reply",
        "sandbox-events",
        "sandbox-events-empty",
        "workspace",
        "entries",
        "entries-last-page",
        "token-keys",
        "token-liveness",
        "token-liveness-gone",
        "sandbox-self",
        "secret-keys",
        "secret-keys-granted",
        "secret-value",
        "activity-reply-ready",
        "activity-reply-releasing",
        "exec-result",
        "admin-purge-reply",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(used, responses);
}

#[tokio::test]
async fn every_error_fixture_arrives_as_the_refusal() {
    let rig = Rig::new().await;
    let magpie = rig.as_(Caller::Magpie);
    let gateway = rig.as_(Caller::Gateway).sandbox_identity(&identity());
    let refusal = |r: Result<(), ClientError>, status: u16, name: &str| match r {
        Err(ClientError::Api(e)) => {
            assert_eq!(e.status, status, "{name}");
            assert_eq!(v(&e.body), fx(name), "{name}");
        }
        other => panic!("{name}: {other:?}"),
    };
    let put: PutSandboxRequest = typed("put-sandbox-request");
    for (name, status) in [("error-sandboxes-disabled", 503), ("error-person-mismatch", 409)] {
        rig.once("put_sandbox", status, name);
        refusal(magpie.put_sandbox(CID, &put).await.map(|_| ()), status, name);
    }
    rig.once("sandbox_secret", 404, "error-not-granted");
    refusal(
        gateway.sandbox_secret("API_KEY").await.map(|_| ()),
        404,
        "error-not-granted",
    );
    rig.once("sandbox_events", 410, "error-cursor-expired");
    let q: SandboxEventsQuery = typed("query-sandbox-events-seq");
    refusal(magpie.sandbox_events(q).await.map(|_| ()), 410, "error-cursor-expired");
    assert_eq!(
        rig.last("sandbox_events").query.unwrap(),
        fx("query-sandbox-events-seq")
    );
}

#[tokio::test]
async fn the_guard_admits_only_each_callers_routes() {
    let rig = Rig::new().await;
    // pigeon may read the deployment but not the sandbox feed; no token is 401
    rig.as_(Caller::Pigeon).deployment().await.unwrap();
    let e = rig
        .as_(Caller::Pigeon)
        .sandbox_events(SandboxEventsQuery {
            after: hermit_api::sandbox::Cursor::Latest,
            limit: None,
            wait_ms: None,
        })
        .await
        .unwrap_err();
    assert_eq!(e.status(), Some(403));
    let anonymous = hermit_api::Client::new(rig.http.clone(), rig.running.url());
    assert_eq!(anonymous.deployment().await.unwrap_err().status(), Some(401));
}
