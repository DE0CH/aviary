//! fake-supervisor: `supervisor_api::server::ControlServer` (the :7070 surface hermit calls, hermit 3.10) with
//! scripted answers on every route and a small model:
//!
//! - the control token: a call whose bearer does not hash to one of the accepted tokens answers 401
//!   `unauthenticated` (supervisor 5: it compares `sha256(bearer)` with the descriptor's `control.tokenSha256`);
//! - `health` and `state` from the current run state ([`FakeSupervisor::set_run`]) and readiness
//!   ([`FakeSupervisor::set_ready`]); before readiness, `prepare` and `release` answer 503 (not ready);
//! - `release` refuses with 409 `sandbox_busy` and the run while a turn, task or ask is open unless `force`, and
//!   otherwise answers `released: true` and moves to `released`; `prepare` answers `ready: true`; `resumed` 204;
//! - `exec` has no model (501 unless scripted);
//! - the local socket's routes ([`FakeSupervisor::local_router`], served over HTTP without descriptors):
//!   `send-user-file` answers `sent` = the number of files and records how many descriptors came with it; `secret`
//!   answers from [`FakeSupervisor::grant_secret`] and 404 `not_granted` otherwise.

use std::sync::{Arc, Mutex, MutexGuard};

use aviary_core::http::ApiError;
use axum::Router;
use axum::http::Method;
use hermit_api::sandbox_surface::ActivityState;
use serde::Serialize;
use serde_json::{Value, json};
use supervisor_api::control::{
    Health, PrepareAnswer, PrepareRequest, ReleaseAnswer, ReleaseRequest, Resumed, RunSummary, StateReport, busy,
    not_ready,
};
use supervisor_api::local::{SendUserFileReply, SendUserFileRequest};
use supervisor_api::server::{ControlServer, Ctx, LocalServer};
use supervisor_api::{ExecRequest, ExecResult, SecretValue, routes, sha256_hex};
use uuid::Uuid;

use crate::answer::not_supported;
use crate::fault::{self, Faults, Names};
use crate::script::{Call, Reply, Script};
use crate::serve::{Served, serve};

const DEFAULT_STATE: &str = include_str!("../../../crates/supervisor-api/fixtures/state.json");

#[derive(Debug)]
struct Model {
    accepted: Vec<String>,
    ready: bool,
    run: RunSummary,
    state: StateReport,
    secrets: std::collections::BTreeMap<String, String>,
    fds_received: Vec<usize>,
}

/// The fake supervisor.
#[derive(Debug)]
pub struct FakeSupervisor {
    script: Script,
    faults: Arc<Faults>,
    model: Mutex<Model>,
}

/// A [`FakeSupervisor`] served on a local port.
#[derive(Debug)]
pub struct Running {
    /// The fake.
    pub fake: Arc<FakeSupervisor>,
    /// The listener.
    pub served: Served,
}

impl Running {
    /// The base URL.
    #[must_use]
    pub fn url(&self) -> String {
        self.served.url()
    }
}

fn names() -> Names {
    Arc::new(|method: &Method, template: &str| {
        NAMES
            .iter()
            .find(|(m, t, _)| *m == method.as_str() && *t == template)
            .map(|(_, _, n)| *n)
    })
}

/// Each route of the control surface as (method, path, the trait method's name).
pub const NAMES: &[(&str, &str, &str)] = &[
    ("GET", routes::HEALTH, "health"),
    ("GET", routes::STATE, "state"),
    ("POST", routes::PREPARE, "prepare"),
    ("POST", routes::RESUMED, "resumed"),
    ("POST", routes::RELEASE, "release"),
    ("POST", routes::EXEC, "exec"),
    ("POST", supervisor_api::local::SEND_USER_FILE, "send_user_file"),
    ("GET", supervisor_api::local::SECRET, "secret"),
];

fn ser<T: Serialize>(v: &T) -> Option<Value> {
    serde_json::to_value(v).ok()
}

impl FakeSupervisor {
    /// A ready, idle supervisor accepting `control_token`.
    ///
    /// # Panics
    /// Never: the state template is a checked-in fixture.
    #[must_use]
    pub fn new(control_token: &str) -> Arc<Self> {
        let mut state: StateReport =
            serde_json::from_str(DEFAULT_STATE).unwrap_or_else(|e| panic!("state fixture: {e}"));
        state.run.state = ActivityState::Idle;
        state.run.turn_id = None;
        state.run.tasks.clear();
        state.run.asks.clear();
        state.supervisor.boot_id = Uuid::new_v4().to_string();
        Arc::new(Self {
            script: Script::default(),
            faults: Arc::new(Faults::default()),
            model: Mutex::new(Model {
                accepted: vec![sha256_hex(control_token)],
                ready: true,
                run: RunSummary {
                    state: ActivityState::Idle,
                    turn_id: None,
                    tasks: 0,
                },
                state,
                secrets: std::collections::BTreeMap::new(),
                fds_received: Vec::new(),
            }),
        })
    }

    /// The router (supervisor-api's control router, with the faults applied).
    pub fn router(self: &Arc<Self>) -> Router {
        fault::layer(
            supervisor_api::server::control_router(self.clone()),
            self.faults.clone(),
            names(),
        )
    }

    /// The local socket's router (supervisor-api's, with the faults applied), for a test serving it on TCP or a
    /// Unix socket of its own.
    pub fn local_router(self: &Arc<Self>) -> Router {
        fault::layer(
            supervisor_api::server::local_router(self.clone()),
            self.faults.clone(),
            names(),
        )
    }

    /// Grant `key` to the local socket's `secret` route.
    pub fn grant_secret(&self, key: &str, value: &str) {
        self.lock().secrets.insert(key.to_owned(), value.to_owned());
    }

    /// How many descriptors came with each `send-user-file` request so far.
    #[must_use]
    pub fn fds_received(&self) -> Vec<usize> {
        self.lock().fds_received.clone()
    }

    /// Serve on a fresh local port.
    ///
    /// # Errors
    /// When no port can be bound.
    pub async fn serve(self: &Arc<Self>) -> std::io::Result<Running> {
        Ok(Running {
            fake: self.clone(),
            served: serve(self.router()).await?,
        })
    }

    /// The scripted answers and the call log.
    #[must_use]
    pub fn script(&self) -> &Script {
        &self.script
    }

    /// The faults.
    #[must_use]
    pub fn faults(&self) -> &Faults {
        &self.faults
    }

    fn lock(&self) -> MutexGuard<'_, Model> {
        self.model.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Accept `token` too (a rotation's overlap).
    pub fn accept_token(&self, token: &str) {
        self.lock().accepted.push(sha256_hex(token));
    }

    /// The run state `health`, `state` and `release` report.
    pub fn set_run(&self, state: ActivityState, turn_id: Option<Uuid>, tasks: u32) {
        let mut m = self.lock();
        m.run = RunSummary { state, turn_id, tasks };
        m.state.run.state = state;
        m.state.run.turn_id = turn_id;
    }

    /// Readiness (the descriptor read, the lease held, FUSE mounted).
    pub fn set_ready(&self, ready: bool) {
        self.lock().ready = ready;
    }

    /// The current run state.
    #[must_use]
    pub fn run(&self) -> RunSummary {
        self.lock().run
    }

    fn record(&self, route: &'static str, ctx: &Ctx, body: Option<Value>) -> Option<Reply> {
        self.script.take(Call {
            route,
            params: json!({}),
            query: None,
            body,
            bytes: None,
            headers: ctx.headers.clone(),
        })
    }

    fn authorised(&self, ctx: &Ctx) -> Result<(), ApiError> {
        let ok = ctx
            .bearer()
            .is_some_and(|t| supervisor_api::control_token_matches(t, &self.lock().accepted));
        if ok { Ok(()) } else { Err(ApiError::unauthenticated()) }
    }

    fn busy_now(m: &Model) -> bool {
        m.run.state.is_busy()
    }
}

#[allow(clippy::unused_async_trait_impl)] // every method is async, as the trait
impl ControlServer for FakeSupervisor {
    async fn health(&self, ctx: Ctx) -> Result<Health, ApiError> {
        if let Some(r) = self.record("health", &ctx, None) {
            return r.typed("health");
        }
        self.authorised(&ctx)?;
        let m = self.lock();
        Ok(Health {
            ok: true,
            ready: m.ready,
            protocol: supervisor_api::CONTROL.1,
            version: m.state.supervisor.version.clone(),
            boot_id: m.state.supervisor.boot_id.clone(),
            busy: Self::busy_now(&m),
            state: m.run.state,
        })
    }

    async fn state(&self, ctx: Ctx) -> Result<StateReport, ApiError> {
        if let Some(r) = self.record("state", &ctx, None) {
            return r.typed("state");
        }
        self.authorised(&ctx)?;
        Ok(self.lock().state.clone())
    }

    async fn prepare(&self, ctx: Ctx, body: PrepareRequest) -> Result<PrepareAnswer, ApiError> {
        if let Some(r) = self.record("prepare", &ctx, ser(&body)) {
            return r.typed("prepare");
        }
        self.authorised(&ctx)?;
        let m = self.lock();
        if !m.ready {
            return Err(not_ready());
        }
        if Self::busy_now(&m) && !body.force {
            return Err(busy(m.run));
        }
        Ok(PrepareAnswer { ready: true })
    }

    async fn resumed(&self, ctx: Ctx, body: Resumed) -> Result<(), ApiError> {
        if let Some(r) = self.record("resumed", &ctx, ser(&body)) {
            return r.unit();
        }
        self.authorised(&ctx)
    }

    async fn release(&self, ctx: Ctx, body: ReleaseRequest) -> Result<ReleaseAnswer, ApiError> {
        if let Some(r) = self.record("release", &ctx, ser(&body)) {
            return r.typed("release");
        }
        self.authorised(&ctx)?;
        let mut m = self.lock();
        if !m.ready {
            return Err(not_ready());
        }
        if Self::busy_now(&m) && !body.force {
            return Err(busy(m.run));
        }
        m.run = RunSummary {
            state: ActivityState::Released,
            turn_id: None,
            tasks: 0,
        };
        m.state.run.state = ActivityState::Released;
        Ok(ReleaseAnswer { released: true })
    }

    async fn exec(&self, ctx: Ctx, body: ExecRequest) -> Result<ExecResult, ApiError> {
        match self.record("exec", &ctx, ser(&body)) {
            Some(r) => r.typed("exec"),
            None => Err(not_supported("exec")),
        }
    }
}

#[allow(clippy::unused_async_trait_impl)] // every method is async, as the trait
impl LocalServer for FakeSupervisor {
    async fn send_user_file(
        &self,
        ctx: Ctx,
        body: SendUserFileRequest,
        fds: Vec<std::os::fd::OwnedFd>,
    ) -> Result<SendUserFileReply, ApiError> {
        self.lock().fds_received.push(fds.len());
        match self.record("send_user_file", &ctx, ser(&body)) {
            Some(r) => r.typed("send_user_file"),
            None => Ok(SendUserFileReply {
                sent: u32::try_from(body.files.len()).unwrap_or(u32::MAX),
            }),
        }
    }

    async fn secret(&self, ctx: Ctx, key: String) -> Result<SecretValue, ApiError> {
        if let Some(r) = self.script.take(Call {
            route: "secret",
            params: json!({"key": key}),
            query: None,
            body: None,
            bytes: None,
            headers: ctx.headers.clone(),
        }) {
            return r.typed("secret");
        }
        let value =
            self.lock().secrets.get(&key).cloned().ok_or_else(|| {
                hermit_api::error::not_found(hermit_api::error::codes::NOT_GRANTED, "key not granted")
            })?;
        Ok(SecretValue { key, value })
    }
}
