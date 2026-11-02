//! fake-supervisor passes supervisor-api's golden fixtures: each response fixture, scripted as the answer of its
//! route, is read back unchanged by hermit's `ControlClient`; the busy and not-ready errors arrive as the client's
//! outcomes; the request fixtures are what the fake recorded; the local socket's fixtures round-trip through the
//! local router.
#![allow(clippy::unwrap_used)] // tests

use aviary_fakes::FakeSupervisor;
use aviary_fakes::fixtures::supervisor as fx;
use aviary_fakes::script::Reply;
use serde::de::DeserializeOwned;
use serde_json::Value;
use supervisor_api::client::{ClientError, ControlClient};
use supervisor_api::control::{Outcome, PrepareRequest, ReleaseRequest, Resumed, RunSummary};

const TOKEN: &str = "hct_fake-control-token";

fn typed<T: DeserializeOwned>(name: &str) -> T {
    serde_json::from_value(fx(name)).unwrap()
}

fn v<T: serde::Serialize>(t: &T) -> Value {
    serde_json::to_value(t).unwrap()
}

async fn rig() -> (ControlClient, aviary_fakes::supervisor::Running) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let running = FakeSupervisor::new(TOKEN).serve().await.unwrap();
    (
        ControlClient::new(reqwest::Client::new(), running.url(), TOKEN),
        running,
    )
}

#[tokio::test]
async fn every_control_fixture_reads_back_unchanged() {
    let (c, running) = rig().await;
    let s = running.fake.script();
    for name in ["health", "health-starting"] {
        s.once("health", Reply::ok(fx(name)));
        assert_eq!(v(&c.health().await.unwrap()), fx(name), "{name}");
    }
    for name in ["state", "state-booting"] {
        s.once("state", Reply::ok(fx(name)));
        assert_eq!(v(&c.state().await.unwrap()), fx(name), "{name}");
    }
    for name in ["prepare-snapshot", "prepare-pause"] {
        s.once("prepare", Reply::ok(fx("prepare-answer")));
        assert_eq!(c.prepare(&typed::<PrepareRequest>(name)).await.unwrap(), Outcome::Done);
        assert_eq!(s.calls_to("prepare").pop().unwrap().body.unwrap(), fx(name), "{name}");
    }
    s.once("resumed", Reply::empty(204));
    c.resumed(&typed::<Resumed>("resumed")).await.unwrap();
    assert_eq!(s.calls_to("resumed").pop().unwrap().body.unwrap(), fx("resumed"));
    for name in ["release-idle", "release-host-lost"] {
        s.once("release", Reply::ok(fx("release-answer")));
        assert_eq!(c.release(&typed::<ReleaseRequest>(name)).await.unwrap(), Outcome::Done);
        assert_eq!(s.calls_to("release").pop().unwrap().body.unwrap(), fx(name), "{name}");
    }
}

#[tokio::test]
async fn the_error_fixtures_arrive_as_the_clients_outcomes() {
    let (c, running) = rig().await;
    let s = running.fake.script();
    s.once("release", Reply::json(409, fx("error-busy")));
    let run: RunSummary = serde_json::from_value(fx("error-busy")["run"].clone()).unwrap();
    assert_eq!(c.release(&typed("release-idle")).await.unwrap(), Outcome::Busy(run));
    s.once("prepare", Reply::json(503, fx("error-not-ready")));
    match c.prepare(&typed("prepare-snapshot")).await {
        Err(ClientError::Api(e)) => {
            assert_eq!(e.status, 503);
            assert_eq!(v(&e.body), fx("error-not-ready"));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_local_socket_fixtures_round_trip() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let fake = FakeSupervisor::new(TOKEN);
    let served = aviary_fakes::serve(fake.local_router()).await.unwrap();
    let http = reqwest::Client::new();
    let reply: Value = http
        .post(format!("{}{}", served.url(), supervisor_api::local::SEND_USER_FILE))
        .json(&fx("send-user-file"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reply["sent"], fx("send-user-file")["files"].as_array().unwrap().len());
    assert_eq!(fake.fds_received(), vec![0]);
    fake.script()
        .once("send_user_file", Reply::ok(fx("send-user-file-reply")));
    let reply: Value = http
        .post(format!("{}{}", served.url(), supervisor_api::local::SEND_USER_FILE))
        .json(&fx("send-user-file"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reply, fx("send-user-file-reply"));
    assert_eq!(
        fake.script().calls_to("send_user_file")[0].body.as_ref().unwrap(),
        &fx("send-user-file")
    );

    let secret = fx("secret");
    let key = secret["key"].as_str().unwrap();
    let url = format!("{}/v1/secrets/{key}", served.url());
    let res = http.get(&url).send().await.unwrap();
    assert_eq!(res.status(), 404);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body, fx("error-not-granted"));
    fake.grant_secret(key, secret["value"].as_str().unwrap());
    let got: Value = http.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(got, secret);
}
