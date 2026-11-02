//! The driver end to end (supervisor 22, unit and integration): a real keeper on a thread, fake hermit and fake
//! magpie behind the reference gateway, the mock model, and a scripted fake `claude` speaking stream-json (this test
//! binary again, [`fake_claude`], started through a wrapper script whose fd 3 is the generation's stdout file).
//!
//! The fake reads the person's text up to the first blank line as a script of `;`-separated steps:
//! `say:X`, `echo` (the whole text and the image count), `fail`, `hang` (until an interrupt), `deaf` (ignores
//! interrupts), `crash:N`, `bg:ID` (a background Bash task), `bgdone:ID:MS` (one that completes after MS and wakes
//! the agent), `wake:MS` (an agent turn after MS), `slowwake:MS` (an agent turn after MS whose result comes 1.5 s
//! later), `ask` (an `AskUserQuestion` that needs a person).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::io::{BufRead as _, Write as _};
use std::net::Ipv4Addr;
use std::os::fd::FromRawFd as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use aviary_core::auth::{Caller, Callers};
use aviary_fakes::hermit::{FakeHermit, HermitConfig};
use aviary_fakes::magpie::FakeMagpie;
use hermit_api::sandbox::PutSandboxRequest;
use magpie_api::sandbox::{
    AnswerAsk, CommandBody, KillCommand, KillReason, Resume, StartFile, StartImage, StartTurn, TitleCommand,
};
use serde_json::{Value, json};
use tokio::time::Instant;
use uuid::Uuid;

use super::*;
use crate::lifecycle::tests::FakeFs;
use crate::lifecycle::{Filesystem, Lifecycle};
use crate::state::{Persisted, RunState, StateFile, VERSION};

// ---- the fake claude ----

fn out_line(out: &mut std::fs::File, v: &Value) {
    let mut b = serde_json::to_vec(v).unwrap();
    b.push(b'\n');
    out.write_all(&b).unwrap();
    out.flush().unwrap();
}

struct Fake {
    out: std::fs::File,
    session: String,
    log: PathBuf,
    /// Steps still to run for the current input.
    steps: std::collections::VecDeque<String>,
    /// A person's message that arrived while busy.
    queued: std::collections::VecDeque<Value>,
    busy: bool,
    waiting: Option<&'static str>,
    timers: Vec<(std::time::Instant, String)>,
    inited: bool,
    bg: Vec<String>,
}

impl Fake {
    fn note(&self, s: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .unwrap();
        writeln!(f, "{s}").unwrap();
    }
    fn assistant(&mut self, text: &str, tools: &[Value]) {
        let mut content = vec![json!({"type": "text", "text": text})];
        content.extend(tools.iter().cloned());
        let v = json!({"type": "assistant", "uuid": Uuid::new_v4().to_string(), "parent_tool_use_id": null,
                       "session_id": self.session, "message": {"role": "assistant", "content": content}});
        out_line(&mut self.out, &v);
    }
    fn result(&mut self, ok: bool, text: &str) {
        let v = if ok {
            json!({"type": "result", "subtype": "success", "is_error": false, "result": text, "duration_ms": 5,
                   "usage": {"input_tokens": 3, "output_tokens": 2}, "session_id": self.session})
        } else {
            json!({"type": "result", "subtype": "error_during_execution", "is_error": true, "result": text,
                   "duration_ms": 5, "session_id": self.session})
        };
        out_line(&mut self.out, &v);
        self.busy = false;
        self.waiting = None;
    }
    #[allow(clippy::needless_pass_by_value)] // called with json! literals
    fn system(&mut self, v: Value) {
        out_line(&mut self.out, &v);
    }
    fn user(&mut self, msg: &Value) {
        if self.busy {
            self.queued.push_back(msg.clone());
            return;
        }
        if !self.inited {
            self.inited = true;
            let v = json!({"type": "system", "subtype": "init", "session_id": self.session, "model": "m"});
            self.system(v);
        }
        let content = &msg["message"]["content"];
        let (text, images) = match content {
            Value::String(s) => (s.clone(), 0),
            Value::Array(b) => (
                b.iter().find_map(|x| x["text"].as_str()).unwrap_or("").to_owned(),
                b.iter().filter(|x| x["type"] == "image").count(),
            ),
            _ => (String::new(), 0),
        };
        self.note(&format!("prompt {}", text.split("\n\n").next().unwrap_or("")));
        let script = text.split("\n\n").next().unwrap_or("").to_owned();
        self.busy = true;
        self.steps = script.split(';').map(str::to_owned).collect();
        self.steps
            .push_front(format!("__text:{images}:{}", text.replace('\n', "\\n")));
        self.run();
    }
    /// Runs steps until one waits.
    fn run(&mut self) {
        let mut text = String::new();
        let mut images = 0;
        while let Some(step) = self.steps.pop_front() {
            let (cmd, arg) = step.split_once(':').unwrap_or((step.as_str(), ""));
            match cmd {
                "__text" => {
                    let (i, t) = arg.split_once(':').unwrap();
                    images = i.parse().unwrap();
                    text = t.replace("\\n", "\n");
                }
                "say" => self.assistant(arg, &[]),
                "echo" => self.assistant(&format!("{text}\nimages={images}"), &[]),
                "fail" => {
                    self.assistant("trying", &[]);
                    self.result(false, "boom");
                    return;
                }
                "hang" => {
                    self.assistant("working", &[]);
                    self.waiting = Some("interrupt");
                    return;
                }
                "deaf" => {
                    self.assistant("working", &[]);
                    self.waiting = Some("never");
                    return;
                }
                "crash" => {
                    self.note("crash");
                    eprintln!("fake claude: crashing as asked");
                    std::process::exit(arg.parse().unwrap_or(1));
                }
                "bg" | "bgdone" => {
                    let mut parts = arg.split(':');
                    let id = parts.next().unwrap().to_owned();
                    let tool = format!("toolu_{id}");
                    self.assistant(
                        "started",
                        &[json!({"type": "tool_use", "id": tool, "name": "Bash",
                                  "input": {"command": "sleep 100", "run_in_background": true}})],
                    );
                    self.system(json!({"type": "system", "subtype": "task_started", "task_id": id,
                                       "tool_use_id": tool, "task_type": "local_bash", "description": "sleep"}));
                    self.bg.push(id.clone());
                    if cmd == "bgdone" {
                        let ms: u64 = parts.next().unwrap().parse().unwrap();
                        self.timers.push((
                            std::time::Instant::now() + Duration::from_millis(ms),
                            format!("done:{id}"),
                        ));
                    }
                }
                "wake" | "slowwake" => {
                    let ms: u64 = arg.parse().unwrap();
                    self.timers
                        .push((std::time::Instant::now() + Duration::from_millis(ms), format!("{cmd}:")));
                }
                "ask" => {
                    self.system(json!({"type": "control_request", "request_id": "ask-1", "request": {
                        "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "requires_user_interaction": true,
                        "tool_use_id": "toolu_ask", "input": {"questions": [{"question": "Which?"}]}}}));
                    self.waiting = Some("answer");
                    return;
                }
                _ => {}
            }
        }
        self.result(true, "done");
        self.next_queued();
    }
    fn next_queued(&mut self) {
        if !self.busy
            && let Some(m) = self.queued.pop_front()
        {
            self.user(&m);
        }
    }
    fn timer(&mut self, what: &str) {
        let (cmd, arg) = what.split_once(':').unwrap();
        match cmd {
            "done" => {
                if !self.bg.iter().any(|b| b == arg) {
                    return;
                }
                self.bg.retain(|b| b != arg);
                self.system(json!({"type": "system", "subtype": "task_notification", "task_id": arg,
                                   "status": "completed", "summary": "finished"}));
                if self.busy {
                    return;
                }
                self.busy = true;
                let v = json!({"type": "user", "uuid": Uuid::new_v4().to_string(), "parent_tool_use_id": null,
                               "message": {"role": "user", "content": format!("<task-notification><task-id>{arg}</task-id></task-notification>")}});
                out_line(&mut self.out, &v);
                self.assistant(&format!("task {arg} done"), &[]);
                self.result(true, "agent");
                self.next_queued();
            }
            "wake" => {
                if self.busy {
                    self.timers
                        .push((std::time::Instant::now() + Duration::from_millis(50), "wake:".into()));
                    return;
                }
                self.busy = true;
                self.assistant("woke up", &[]);
                self.result(true, "agent");
                self.next_queued();
            }
            "slowwake" => {
                self.busy = true;
                self.assistant("woke up slowly", &[]);
                self.timers.push((
                    std::time::Instant::now() + Duration::from_millis(1500),
                    "slowend:".into(),
                ));
            }
            "slowend" => {
                self.result(true, "agent");
                self.next_queued();
            }
            _ => {}
        }
    }
    fn control(&mut self, v: &Value) {
        let id = v["request_id"].as_str().unwrap_or("").to_owned();
        let sub = v["request"]["subtype"].as_str().unwrap_or("");
        self.note(&format!("control {sub}"));
        match sub {
            "initialize" => {
                if Path::new(".fake-noinit").exists() {
                    return;
                }
                self.system(json!({"type": "control_response",
                                   "response": {"subtype": "success", "request_id": id, "response": {}}}));
            }
            "interrupt" => {
                if self.waiting == Some("never") {
                    return;
                }
                self.system(json!({"type": "control_response",
                                   "response": {"subtype": "success", "request_id": id, "response": {}}}));
                if self.busy {
                    let v = json!({"type": "user", "uuid": Uuid::new_v4().to_string(), "parent_tool_use_id": null,
                                   "message": {"role": "user", "content": [{"type": "text", "text": "[Request interrupted by user]"}]}});
                    out_line(&mut self.out, &v);
                    self.steps.clear();
                    self.result(false, "interrupted");
                    self.next_queued();
                }
            }
            "stop_task" => {
                let t = v["request"]["task_id"].as_str().unwrap_or("").to_owned();
                self.system(json!({"type": "control_response",
                                   "response": {"subtype": "success", "request_id": id, "response": {}}}));
                if self.bg.contains(&t) {
                    self.bg.retain(|b| *b != t);
                    self.system(json!({"type": "system", "subtype": "task_notification", "task_id": t,
                                       "status": "stopped"}));
                }
            }
            _ => {}
        }
    }
    fn response(&mut self, v: &Value) {
        if self.waiting != Some("answer") {
            return;
        }
        let r = &v["response"]["response"];
        let text = format!(
            "answered {} {}",
            r["behavior"].as_str().unwrap_or(""),
            r["updatedInput"]
        );
        self.assistant(&text, &[]);
        self.waiting = None;
        self.run();
    }
}

/// The fake `claude` (does nothing unless started by the wrapper script).
#[test]
#[ignore = "the fake claude: run by the driver tests through their wrapper script"]
fn fake_claude() {
    let Ok(argv_file) = aviary_core::env::try_required("FAKE_ARGV") else {
        return;
    };
    let argv: Vec<String> = std::fs::read_to_string(&argv_file)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let session = argv
        .windows(2)
        .find(|w| w[0] == "--session-id" || w[0] == "--resume")
        .map(|w| w[1].clone())
        .unwrap_or_default();
    // SAFETY: fd 3 is the generation's stdout file, opened by the wrapper for this process alone.
    #[allow(unsafe_code)]
    let out = unsafe { std::fs::File::from_raw_fd(3) };
    let mut fake = Fake {
        out,
        session,
        log: PathBuf::from(".fake.log"),
        steps: std::collections::VecDeque::new(),
        queued: std::collections::VecDeque::new(),
        busy: false,
        waiting: None,
        timers: Vec::new(),
        inited: false,
        bg: Vec::new(),
    };
    fake.note(&format!("argv {}", argv.join(" ")));
    let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(l) = line else { break };
            if tx.send(Some(l)).is_err() {
                return;
            }
        }
        let _ = tx.send(None);
    });
    loop {
        let now = std::time::Instant::now();
        let due: Vec<String> = fake
            .timers
            .iter()
            .filter(|(t, _)| *t <= now)
            .map(|(_, w)| w.clone())
            .collect();
        fake.timers.retain(|(t, _)| *t > now);
        for d in due {
            fake.timer(&d);
        }
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(Some(l)) => {
                let v: Value = serde_json::from_str(&l).unwrap_or(Value::Null);
                fake.note(&format!("stdin {}", v["type"].as_str().unwrap_or("?")));
                match v["type"].as_str() {
                    Some("user") => fake.user(&v),
                    Some("control_request") => fake.control(&v),
                    Some("control_response") => fake.response(&v),
                    _ => {}
                }
            }
            Ok(None) => {
                fake.note("eof");
                std::process::exit(0);
            }
            Err(_) => {}
        }
    }
}

// ---- the world ----

struct World {
    dir: PathBuf,
    ws: PathBuf,
    hermit: Arc<FakeHermit>,
    magpie: Arc<FakeMagpie>,
    model: Arc<mock_model::MockModel>,
    _served: (aviary_fakes::hermit::Running, aviary_fakes::magpie::Running),
    gateway: test_gateway::Running,
    cid: Uuid,
    keeper: crate::keeper::testing::Running,
}

async fn world() -> World {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let hermit = FakeHermit::with_config(HermitConfig::default());
    let magpie = FakeMagpie::new();
    let model = mock_model::MockModel::with_default_models();
    let hs = hermit.serve().await.unwrap();
    let ms = magpie.serve().await.unwrap();
    let ml = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_url = format!("http://{}", ml.local_addr().unwrap());
    let r = model.router();
    tokio::spawn(async move { axum::serve(ml, r).await });
    let cid = Uuid::now_v7();
    hs.client(reqwest::Client::new(), Caller::Magpie)
        .put_sandbox(
            cid,
            &PutSandboxRequest {
                person: "staff@example.org".into(),
            },
        )
        .await
        .unwrap();
    let token = |n: &str| format!("{n}-{}", Uuid::new_v4().simple());
    let cfg = test_gateway::Config {
        proxy_listen: "127.0.0.1:0".parse().unwrap(),
        api_listen: "127.0.0.1:0".parse().unwrap(),
        upstreams: test_gateway::Upstreams {
            magpie: ms.url(),
            hermit: hs.url(),
            tools: "http://127.0.0.1:9".into(),
            model: model_url,
        },
        credentials: test_gateway::Credentials {
            magpie: magpie.token(Caller::Gateway).unwrap(),
            hermit: hermit.token(Caller::Gateway).unwrap(),
            tools: token("tools"),
            model: token("model"),
        },
        callers: Callers::new([(Caller::Pigeon, vec![token("p")]), (Caller::Hermit, vec![token("h")])]).unwrap(),
        self_hosts: vec![],
        ops_summary: test_gateway::default_ops_summary(),
        drain: test_gateway::DrainBounds::default(),
        upload_cap: test_gateway::UPLOAD_CAP,
        upstream_idle: gateway_api::limits::IDLE_TIMEOUT,
        faults: test_gateway::Faults::default(),
    };
    let mut registry = prometheus_client::registry::Registry::default();
    let gateway = test_gateway::start(cfg, &mut registry).await.unwrap();
    gateway.keys_loaded().await;
    let keeper = crate::keeper::testing::Running::start(None);
    let dir = keeper.dir.clone();
    let ws = dir.join("workspace");
    std::fs::create_dir_all(&ws).unwrap();
    // the wrapper: the argv into a file, the generation's stdout on fd 3, the test harness's own output to a log
    let exe = std::env::current_exe().unwrap();
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{d}/argv.'$$\nexec 3>&1\nFAKE_ARGV='{d}/argv.'$$ exec '{exe}' cc::run::tests::fake_claude --exact --ignored --nocapture --test-threads=1 >>'{d}/fake-harness.log'\n",
        d = dir.display(),
        exe = exe.display()
    );
    let bin = dir.join("claude");
    std::fs::write(&bin, script).unwrap();
    std::os::unix::fs::PermissionsExt::set_mode(&mut std::fs::metadata(&bin).unwrap().permissions(), 0o755);
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    World {
        dir,
        ws,
        hermit,
        magpie,
        model,
        _served: (hs, ms),
        gateway,
        cid,
        keeper,
    }
}

/// A driver attached to the world, its lifecycle and the persisted state.
struct Sup {
    driver: Arc<Driver>,
    lifecycle: Arc<Lifecycle>,
    persisted: Arc<Persisted>,
    exits: tokio::task::AbortHandle,
}

impl Drop for Sup {
    /// Ends the driver's tasks and the exit poll, and lets the keeper calls already sent finish while the keeper
    /// still runs: a test keeper that stops leaves its sockets open, so a call sent after that would block (and keep
    /// the test runtime from shutting down) for ever.
    fn drop(&mut self) {
        self.driver.abort_all();
        self.exits.abort();
        std::thread::sleep(Duration::from_millis(300));
    }
}

impl World {
    fn boot(&self) -> BootEnv {
        let token = self.hermit.mint_token(self.cid, Ipv4Addr::LOCALHOST).unwrap();
        BootEnv {
            proxy: format!("http://sbx:{token}@{}", self.gateway.proxy_addr()),
            path: "/usr/bin:/bin".into(),
            lang: None,
            lc_all: None,
        }
    }

    fn config(&self) -> DriverConfig {
        // SAFETY: getuid/getgid have no memory effects.
        #[allow(unsafe_code)]
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        DriverConfig {
            launch: Launch {
                bin: self.dir.join("claude").to_string_lossy().into_owned(),
                cwd: self.ws.to_string_lossy().into_owned(),
                uid,
                gid,
            },
            managed_dir: self.dir.join("managed"),
            gen_dir: self.dir.join("gens"),
            uploads: Uploads {
                workspace: self.ws.clone(),
                uid,
                gid,
            },
        }
    }

    /// Starts a driver as the start sequence would (the lease, the descriptor, then `attach`).
    async fn attach(&self, prior: Option<RunState>, generation: crate::start::Reattach) -> Sup {
        self.attach_with(prior, generation, Arc::new(crate::cc::Stored0)).await
    }

    async fn attach_with(
        &self,
        prior: Option<RunState>,
        generation: crate::start::Reattach,
        transcript: Arc<dyn Transcript>,
    ) -> Sup {
        let boot = self.boot();
        let build = aviary_core::version::Build::new("supervisor");
        let http = boot.fixed_names_client(&build).unwrap();
        let hermit = Hermit::new(http.clone());
        let magpie = crate::magpie::client(http.clone());
        let driver = Driver::new(
            self.config(),
            magpie.clone(),
            Model::new(http),
            hermit.clone(),
            transcript,
        );
        let fs: Arc<dyn Filesystem> = Arc::new(FakeFs::default());
        let lifecycle = Lifecycle::new(driver.clone(), fs);
        let descriptor = hermit.fetch_initial().await;
        let lease = crate::magpie::take_lease(&magpie, self.cid, |_| {}).await;
        let boot_id = "b00700000000000a".to_owned();
        let persisted = Persisted::new(RunState {
            version: VERSION,
            boot_id: boot_id.clone(),
            ..prior.clone().unwrap_or_default()
        });
        tokio::spawn(persisted.clone().writer(StateFile::new(self.dir.join("state"))));
        let exits = crate::keeper::watch::Exits::new(self.keeper.client.clone(), 0);
        let exits_task = tokio::spawn(exits.clone().run()).abort_handle();
        driver
            .attach(AttachContext {
                prior,
                generation,
                lease,
                descriptor,
                persisted: persisted.clone(),
                keeper: self.keeper.client.clone(),
                exits,
                boot,
                cgroup: None,
                lifecycle: Arc::downgrade(&lifecycle),
                activity: None,
            })
            .await;
        lifecycle.set_ready(|r| {
            r.descriptor = true;
            r.lease = true;
            r.mounted = true;
            r.workspace = true;
            r.attached = true;
        });
        Sup {
            driver,
            lifecycle,
            persisted,
            exits: exits_task,
        }
    }

    fn turn(&self, text: &str) -> (Uuid, u64) {
        self.turn_with(text, None, vec![], vec![])
    }

    fn turn_with(
        &self,
        text: &str,
        resume: Option<String>,
        files: Vec<StartFile>,
        images: Vec<StartImage>,
    ) -> (Uuid, u64) {
        let id = Uuid::now_v7();
        let seq = self.magpie.push_command(
            self.cid,
            Some(id),
            CommandBody::StartTurn(StartTurn {
                text: text.into(),
                files,
                images,
                resume: Resume {
                    provider_session_id: resume,
                },
                sent_at: magpie_api::Timestamp::now(),
            }),
        );
        (id, seq)
    }

    /// The bodies of the state reports for `turn`.
    fn states(&self, turn: Uuid) -> Vec<Value> {
        self.magpie
            .script()
            .calls_to("turn_state")
            .into_iter()
            .filter(|c| c.param("turnId") == Some(turn.to_string().as_str()))
            .filter_map(|c| c.body)
            .collect()
    }

    fn state_names(&self, turn: Uuid) -> Vec<String> {
        self.states(turn)
            .iter()
            .map(|b| b["state"].as_str().unwrap_or("").to_owned())
            .collect()
    }

    async fn ended(&self, turn: Uuid) -> Value {
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            if let Some(b) = self.states(turn).last()
                && b["state"] != "running"
            {
                return b.clone();
            }
            assert!(
                Instant::now() < deadline,
                "turn {turn} did not end: {:?}\nfake:\n{}",
                self.state_names(turn),
                self.fake_log().join("\n")
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn fake_log(&self) -> Vec<String> {
        std::fs::read_to_string(self.ws.join(".fake.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn prompts(&self) -> Vec<String> {
        self.fake_log()
            .into_iter()
            .filter(|l| l.starts_with("prompt "))
            .collect()
    }

    fn spawns(&self) -> Vec<String> {
        self.fake_log().into_iter().filter(|l| l.starts_with("argv ")).collect()
    }

    async fn acked(&self, seq: u64) -> AckResult {
        let mut out = None;
        until(&format!("ack of {seq}"), || {
            out = self.magpie.acks(self.cid).get(&seq).cloned();
            out.is_some()
        })
        .await;
        out.unwrap()
    }

    async fn idle(&self, sup: &Sup) {
        let d = sup.driver.clone();
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            let s = d.status().borrow().clone();
            if s.state == ActivityState::Idle && s.turn_id.is_none() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "not idle: {s:?}\nfake:\n{}",
                self.fake_log().join("\n")
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(40);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn texts(w: &World) -> String {
    w.fake_log().join("\n")
}

// ---- the tests ----

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_runs_and_the_generation_lives_across_turns() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    // pre-spawned with --session-id <conversation> (no provider session yet), the managed files written
    until("pre-spawn", || !w.spawns().is_empty()).await;
    let argv = &w.spawns()[0];
    assert!(argv.contains(&format!("--session-id {}", w.cid)), "{argv}");
    assert!(argv.contains("--setting-sources= ") && argv.contains("--permission-mode bypassPermissions"));
    let md = std::fs::read_to_string(w.dir.join("managed/CLAUDE.md")).unwrap();
    assert_eq!(md, sup.driver.hermit.descriptor().unwrap().claude.managed_claude_md);
    assert!(w.dir.join("managed/managed-settings.json").exists());

    let (t1, s1) = w.turn("say:你好");
    let end = w.ended(t1).await;
    assert_eq!(end["state"], "completed", "{end}");
    assert!(end["lastUuid"].is_string());
    assert_eq!(end["held"], false);
    assert_eq!(end["usage"]["outputTokens"], 2);
    let running = &w.states(t1)[0];
    assert_eq!(running["state"], "running");
    assert_eq!(running["providerSessionId"], w.cid.to_string());
    assert_eq!(
        running["transcriptPath"],
        format!("projects/-workspace/{}.jsonl", w.cid)
    );
    assert_eq!(w.acked(s1).await.delivered, Some(true));
    w.idle(&sup).await;

    // the next turn resumes the same session: the same process, a second user message on the same stdin
    let (t2, _) = w.turn_with("say:again", Some(w.cid.to_string()), vec![], vec![]);
    assert_eq!(w.ended(t2).await["state"], "completed");
    assert_eq!(w.spawns().len(), 1, "{}", texts(&w));
    assert_eq!(w.prompts().len(), 2);
    // the heartbeat carries the last turn once its end report was answered
    let d = sup.driver.clone();
    until("lastTurn", || d.heartbeat().last_turn.is_some_and(|l| l.id == t2)).await;
    // run.json has it all
    sup.persisted.flush().await;
    let saved = StateFile::new(w.dir.join("state")).load().unwrap().unwrap();
    assert_eq!(saved.last_turn.unwrap().id, t2.to_string());
    assert!(saved.generation.unwrap().stdout_offset > 0);
    assert_eq!(saved.run.state, "idle");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_results_interrupts_and_kills() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let (t, _) = w.turn("fail");
    let end = w.ended(t).await;
    assert_eq!(end["state"], "failed");
    assert_eq!(end["error"]["code"], "run.error");
    assert!(end["error"]["message"].as_str().unwrap().contains("boom"));
    w.idle(&sup).await;

    // interrupt: the turn is canceled at the result that follows
    let (t, _) = w.turn("hang");
    until("running", || w.state_names(t) == ["running"]).await;
    let i = w
        .magpie
        .push_command(w.cid, Some(t), CommandBody::Interrupt(magpie_api::Empty {}));
    assert_eq!(w.ended(t).await["state"], "canceled");
    w.acked(i).await;
    assert!(texts(&w).contains("control interrupt"));
    w.idle(&sup).await;

    // kill (a stop not confirmed): canceled; magpie's watchdog: failed
    let (t, _) = w.turn("deaf");
    until("running", || w.state_names(t) == ["running"]).await;
    let k = w.magpie.push_command(
        w.cid,
        Some(t),
        CommandBody::Kill(KillCommand {
            reason: KillReason::Stop,
        }),
    );
    assert_eq!(w.ended(t).await["state"], "canceled");
    w.acked(k).await;
    w.idle(&sup).await;
    let (t, _) = w.turn("deaf");
    until("running", || w.state_names(t) == ["running"]).await;
    w.magpie.push_command(
        w.cid,
        Some(t),
        CommandBody::Kill(KillCommand {
            reason: KillReason::Watchdog,
        }),
    );
    let end = w.ended(t).await;
    assert_eq!(end["state"], "failed");
    assert_eq!(end["error"]["code"], "run.error");
    // every kill spawned a new generation, resumed
    let (t, _) = w.turn_with("say:back", Some(w.cid.to_string()), vec![], vec![]);
    assert_eq!(w.ended(t).await["state"], "completed");
    assert!(w.spawns().last().unwrap().contains(&format!("--resume {}", w.cid)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_interrupt_that_does_not_end_the_turn_kills_the_generation_after_15_s() {
    let w = world().await;
    let _sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let (t, _) = w.turn("deaf");
    until("running", || w.state_names(t) == ["running"]).await;
    let started = Instant::now();
    w.magpie
        .push_command(w.cid, Some(t), CommandBody::Interrupt(magpie_api::Empty {}));
    let end = w.ended(t).await;
    assert_eq!(end["state"], "canceled");
    let took = started.elapsed();
    assert!(
        took >= INTERRUPT_GRACE && took < INTERRUPT_GRACE + Duration::from_secs(8),
        "{took:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_crash_fails_the_turn_and_a_crash_loop_refuses_new_turns() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let (t, _) = w.turn("say:x;crash:3");
    let end = w.ended(t).await;
    assert_eq!(end["state"], "failed");
    assert_eq!(end["error"]["code"], "run.exited");
    let msg = end["error"]["message"].as_str().unwrap();
    assert!(msg.starts_with("exit 3") && msg.contains("crashing as asked"), "{msg}");
    assert_eq!(end["exitCode"], 3);
    assert!(
        end.get("lastUuid").is_none_or(Value::is_null),
        "a dead generation writes nothing more"
    );
    w.idle(&sup).await;
    for _ in 0..2 {
        let (t, _) = w.turn_with("crash:4", Some(w.cid.to_string()), vec![], vec![]);
        assert_eq!(w.ended(t).await["state"], "failed");
        w.idle(&sup).await;
    }
    // three exits in 60 s: refused at once, nothing spawned
    let spawned = w.spawns().len();
    let (t, s) = w.turn_with("say:x", Some(w.cid.to_string()), vec![], vec![]);
    let end = w.ended(t).await;
    assert_eq!(end["error"]["message"], "Claude Code would not start");
    assert!(w.acked(s).await.error.is_some());
    assert_eq!(w.spawns().len(), spawned);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_turns_and_a_start_turn_during_one() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let (t, _) = w.turn("say:later;wake:300");
    assert_eq!(w.ended(t).await["state"], "completed");
    // the wake-up opens a turn of origin agent, ended at its result
    let calls = w.magpie.clone();
    until("an agent turn", || !calls.script().calls_to("agent_turn").is_empty()).await;
    let agent: Uuid =
        serde_json::from_value(w.magpie.script().calls_to("agent_turn")[0].body.as_ref().unwrap()["turnId"].clone())
            .unwrap();
    assert_eq!(w.ended(agent).await["state"], "completed");
    w.idle(&sup).await;

    // a person's start_turn while an agent turn runs: written at once, the agent turn ends at its own result,
    // the person's turn at the next one
    let (t, _) = w.turn("say:x;slowwake:100");
    w.ended(t).await;
    until("a second agent turn", || {
        calls.script().calls_to("agent_turn").len() == 2
    })
    .await;
    let (p, _) = w.turn("say:mine");
    until("the person's turn running", || {
        w.state_names(p).first().is_some_and(|s| s == "running")
    })
    .await;
    let agent2: Uuid =
        serde_json::from_value(w.magpie.script().calls_to("agent_turn")[1].body.as_ref().unwrap()["turnId"].clone())
            .unwrap();
    assert_eq!(w.ended(agent2).await["state"], "completed");
    assert_eq!(w.ended(p).await["state"], "completed");
    assert_eq!(w.state_names(p), ["running", "completed"]);
    w.idle(&sup).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_hold_supersede_stop_tasks_and_a_task_that_ends() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let (t, _) = w.turn("bg:t1");
    let end = w.ended(t).await;
    assert_eq!(end["state"], "completed");
    assert_eq!(end["held"], true);
    assert_eq!(end["tasks"][0]["taskId"], "t1");
    let d = sup.driver.clone();
    until("held", || d.status().borrow().state == ActivityState::Held).await;
    assert!(sup.driver.status().borrow().busy());
    // a send supersedes the hold: stop_task, then the message
    let (t2, _) = w.turn_with("say:next", Some(w.cid.to_string()), vec![], vec![]);
    assert_eq!(w.ended(t2).await["state"], "completed");
    let log = texts(&w);
    let stop = log.find("control stop_task").expect("stop_task sent");
    let prompt = log.find("prompt say:next").unwrap();
    assert!(stop < prompt, "{log}");
    w.idle(&sup).await;

    // a cancel while held: stop_tasks
    let (t3, _) = w.turn_with("bg:t2", Some(w.cid.to_string()), vec![], vec![]);
    w.ended(t3).await;
    until("held", || d.status().borrow().state == ActivityState::Held).await;
    let s = w
        .magpie
        .push_command(w.cid, None, CommandBody::StopTasks(magpie_api::Empty {}));
    assert_eq!(w.acked(s).await.stopped, Some(1));
    w.idle(&sup).await;

    // a task that completes by itself ends the hold, then wakes the agent
    let (t4, _) = w.turn_with("bgdone:t3:1500", Some(w.cid.to_string()), vec![], vec![]);
    let end = w.ended(t4).await;
    assert_eq!(end["held"], true, "{end}\n{}", texts(&w));
    until("the wake-up turn", || {
        w.magpie.script().calls_to("agent_turn").len() == 1
    })
    .await;
    w.idle(&sup).await;
    // every task event reached magpie with its id
    until("task events", || {
        w.magpie
            .script()
            .calls_to("tasks")
            .iter()
            .flat_map(|c| {
                c.body.clone().unwrap()["events"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            })
            .any(|e| e["taskId"] == "t3" && e["event"] == "notification")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ask_is_relayed_and_answered() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let (t, _) = w.turn("ask;say:after");
    let calls = w.magpie.clone();
    until("the ask", || !calls.script().calls_to("ask_open").is_empty()).await;
    let ask = calls.script().calls_to("ask_open")[0].body.clone().unwrap();
    assert_eq!(ask["toolName"], "AskUserQuestion");
    assert_eq!(ask["toolUseId"], "toolu_ask");
    let id = ask["askId"].as_str().unwrap().to_owned();
    assert_eq!(sup.driver.status().borrow().asks, 1);
    // an unknown ask
    let bad = w.magpie.push_command(
        w.cid,
        None,
        CommandBody::AnswerAsk(AnswerAsk {
            ask_id: "nope".into(),
            allow: true,
            updated_input: None,
            message: None,
        }),
    );
    assert_eq!(w.acked(bad).await.error.as_deref(), Some("no_such_ask"));
    let mut input = serde_json::Map::new();
    input.insert("answers".into(), json!({"Which?": "this one"}));
    let a = w.magpie.push_command(
        w.cid,
        None,
        CommandBody::AnswerAsk(AnswerAsk {
            ask_id: id.clone(),
            allow: true,
            updated_input: Some(input),
            message: None,
        }),
    );
    w.acked(a).await;
    assert_eq!(w.ended(t).await["state"], "completed");
    let resolved = calls.script().calls_to("ask_resolved");
    assert_eq!(resolved[0].param("askId"), Some(id.as_str()));
    assert_eq!(resolved[0].body.as_ref().unwrap()["outcome"], "answered");
    w.idle(&sup).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn uploads_are_materialised_and_tagged() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let pdf = b"%PDF-1.4 report".to_vec();
    let img = b"\x89PNG original".to_vec();
    let small = b"\x89PNG model copy".to_vec();
    let (pdf_sha, img_sha, small_sha) = (
        w.magpie.seed_blob(&pdf),
        w.magpie.seed_blob(&img),
        w.magpie.seed_blob(&small),
    );
    let files = vec![StartFile {
        upload_id: Uuid::new_v4(),
        name: "Q3 report.pdf".into(),
        mime_type: "application/pdf".into(),
        size: pdf.len() as u64,
        workspace_path: "uploads/Q3 report.pdf".into(),
        blob: pdf_sha,
    }];
    let images = vec![StartImage {
        upload_id: Uuid::new_v4(),
        mime_type: "image/png".into(),
        workspace_path: "uploads/IMG_1.png".into(),
        blob: img_sha.clone(),
        model_blob: small_sha,
        model_mime: "image/png".into(),
    }];
    // a file of that name written by the model is replaced
    std::fs::create_dir_all(w.ws.join("uploads")).unwrap();
    std::fs::write(w.ws.join("uploads/IMG_1.png"), b"old").unwrap();
    let (t, _) = w.turn_with("echo", None, files.clone(), images.clone());
    assert_eq!(w.ended(t).await["state"], "completed");
    assert_eq!(std::fs::read(w.ws.join("uploads/Q3 report.pdf")).unwrap(), pdf);
    assert_eq!(std::fs::read(w.ws.join("uploads/IMG_1.png")).unwrap(), img);
    let leftovers: Vec<_> = std::fs::read_dir(w.ws.join("uploads"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(".part-"))
        .collect();
    assert!(leftovers.is_empty());
    w.idle(&sup).await;

    // a symlinked uploads directory fails the turn
    std::fs::rename(w.ws.join("uploads"), w.dir.join("moved")).unwrap();
    std::os::unix::fs::symlink(w.dir.join("moved"), w.ws.join("uploads")).unwrap();
    let (t, s) = w.turn_with("say:x", Some(w.cid.to_string()), files.clone(), vec![]);
    let end = w.ended(t).await;
    assert_eq!(end["error"]["code"], "run.error");
    assert!(w.acked(s).await.error.is_some());
    std::fs::remove_file(w.ws.join("uploads")).unwrap();
    w.idle(&sup).await;

    // a full workspace
    crate::uploads::FAIL_WRITES_WITH.store(libc::EDQUOT, std::sync::atomic::Ordering::SeqCst);
    let (t, _) = w.turn_with("say:x", Some(w.cid.to_string()), files, vec![]);
    let end = w.ended(t).await;
    crate::uploads::FAIL_WRITES_WITH.store(0, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(end["error"]["message"], crate::uploads::NO_SPACE);
    let echo = w.fake_log().into_iter().filter(|l| l.starts_with("prompt ")).count();
    assert_eq!(echo, 1, "the failed turns sent nothing to Claude Code");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_prompt_carries_the_tags_and_the_image_block() {
    let w = world().await;
    let _sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let img = w.magpie.seed_blob(b"orig");
    let small = w.magpie.seed_blob(b"copy");
    let (t, _) = w.turn_with(
        "echo",
        None,
        vec![],
        vec![StartImage {
            upload_id: Uuid::new_v4(),
            mime_type: "image/png".into(),
            workspace_path: "uploads/a.png".into(),
            blob: img,
            model_blob: small,
            model_mime: "image/png".into(),
        }],
    );
    w.ended(t).await;
    // the fake echoes what it got into its assistant message, which lands in stdout
    let out = std::fs::read_to_string(w.dir.join("gens/1/stdout.ndjson")).unwrap();
    assert!(out.contains("images=1"), "{out}");
    assert!(
        out.contains("<image_uploads>") && out.contains("(original name: a.png)"),
        "{out}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn titles_go_to_the_model_and_back_in_the_ack() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let s = w.magpie.push_command(
        w.cid,
        None,
        CommandBody::Title(TitleCommand {
            user_prompt: format!("{} 你好", mock_model::TITLE_PROMPT_PREFIX),
            max_tokens: 100,
        }),
    );
    let ack = w.acked(s).await;
    let r = ack.response.expect("the response");
    assert!(r.to_string().contains(mock_model::TITLE_TEXT), "{r}");
    let seen = w.model.requests(None);
    let title = seen
        .iter()
        .find(|x| x.text.starts_with(mock_model::TITLE_PROMPT_PREFIX))
        .unwrap();
    assert_eq!(title.messages, 1);
    assert!(title.system.is_some());
    // an unknown command type (a newer magpie's; the fake cannot serve one) is acked unsupported
    sup.driver
        .execute(Command {
            cmd_seq: 100,
            turn_id: None,
            body: CommandBody::Unknown,
        })
        .await;
    let calls = w.magpie.clone();
    until("the unsupported ack", || {
        calls
            .script()
            .calls_to("ack_command")
            .iter()
            .any(|c| c.param("cmdSeq") == Some("100") && c.body.as_ref().unwrap()["result"]["error"] == "unsupported")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::many_single_char_names)]
async fn a_restart_replays_a_written_prompt_exactly_once() {
    for keeper_had_it in [true, false] {
        let w = world().await;
        let first = w.attach(None, crate::start::Reattach::Nothing).await;
        let d = first.driver.clone();
        until("initialised", || {
            lock(&d.st).live.as_ref().is_some_and(|g| g.initialized)
        })
        .await;
        let (n, pid) = {
            let st = lock(&first.driver.st);
            let g = st.live.as_ref().unwrap();
            (g.n, g.pid)
        };
        let d = first.driver.clone();
        until("initialised", || {
            lock(&d.st).live.as_ref().is_some_and(|g| g.initialized)
        })
        .await;
        // the first supervisor recorded `written` and crashed: the keeper had the message, or it did not
        // the first supervisor is gone for good before the prompt reaches anyone
        first.driver.abort_all();
        first.persisted.flush().await;
        let (turn, seq) = w.turn("say:once");
        let boot_id = "b00700000000000a";
        let id = msg_id(boot_id, seq);
        if keeper_had_it {
            let s = StartTurn {
                text: "say:once".into(),
                files: vec![],
                images: vec![],
                resume: Resume {
                    provider_session_id: None,
                },
                sent_at: magpie_api::Timestamp::now(),
            };
            let line = prompt::user_line(
                &prompt::content(prompt::text(&s.text, &[], &[]), &[]),
                &s.sent_at.to_wire(),
            );
            let k = w.keeper.client.clone();
            tokio::task::spawn_blocking(move || k.stdin(pid, id, &line))
                .await
                .unwrap()
                .unwrap();
        }
        let mut prior = first.persisted.read(Clone::clone);
        prior.delivered.insert(
            seq.to_string(),
            Delivered {
                state: "written".into(),
                msg_id: Some(hex(&id)),
                generation: Some(n),
                pending_inputs_after: Some(1),
            },
        );
        prior.cmd_after = seq;
        drop(first);
        let second = w.attach(Some(prior), crate::start::Reattach::Alive { n, pid }).await;
        let end = w.ended(turn).await;
        assert_eq!(end["state"], "completed", "keeper had it: {keeper_had_it}");
        let prompts = w.prompts();
        assert_eq!(prompts, ["prompt say:once"], "keeper had it: {keeper_had_it}");
        w.idle(&second).await;
        assert_eq!(w.spawns().len(), 1, "re-attached, not respawned");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_finds_its_generation_dead_and_fails_the_turn() {
    let w = world().await;
    let first = w.attach(None, crate::start::Reattach::Nothing).await;
    let (turn, _) = w.turn("hang");
    until("running", || w.state_names(turn) == ["running"]).await;
    first.driver.stop_poll();
    first.persisted.flush().await;
    let prior = first.persisted.read(Clone::clone);
    let (n, pid) = {
        let st = lock(&first.driver.st);
        let g = st.live.as_ref().unwrap();
        (g.n, g.pid)
    };
    // the generation dies while no supervisor watches (the first one is gone for good)
    first.driver.abort_all();
    drop(first);
    let k = w.keeper.client.clone();
    tokio::task::spawn_blocking(move || k.signal(-pid, libc::SIGKILL))
        .await
        .unwrap()
        .unwrap();
    let rec = w.keeper.wait_exit(pid);
    let _second = w
        .attach(Some(prior), crate::start::Reattach::Exited { n, exit: Some(rec) })
        .await;
    let end = w.ended(turn).await;
    assert_eq!(end["state"], "failed");
    assert_eq!(end["error"]["code"], "run.exited");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reports_wait_out_a_magpie_outage_and_are_never_dropped() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    until("pre-spawn", || !w.spawns().is_empty()).await;
    let fault = w.magpie.faults().on(
        "turn_state",
        aviary_fakes::fault::FaultKind::Unavailable { retry_after_secs: 1 },
    );
    let (t, _) = w.turn("say:x");
    until("the turn answered", || w.prompts().len() == 1).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(w.state_names(t).is_empty(), "every report waits");
    assert!(w.magpie.faults().hits() >= 2, "and is retried");
    w.magpie.faults().remove(fault);
    assert_eq!(w.ended(t).await["state"], "completed");
    assert_eq!(w.state_names(t), ["running", "completed"], "delivered once");
    w.idle(&sup).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_hook_passes_descriptors_and_magpie_keeps_the_files() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let local = Arc::new(crate::local::Local {
        hermit: sup.driver.hermit.clone(),
        agent: sup.driver.clone(),
    });
    let sock = w.dir.join("agent.sock");
    let _srv = crate::sent_files::serve_with_fds(&sock, 0o666, supervisor_api::server::local_router(local)).unwrap();
    std::fs::create_dir_all(w.ws.join("out")).unwrap();
    std::fs::write(w.ws.join("out/chart.png"), b"png bytes").unwrap();
    std::fs::write(w.ws.join("out/data.csv"), b"a,b\n1,2\n").unwrap();
    std::fs::write(w.dir.join("secret.txt"), b"not yours").unwrap();
    std::os::unix::fs::symlink(w.dir.join("secret.txt"), w.ws.join("out/link.txt")).unwrap();
    let paths = crate::sent_files::HookPaths {
        root: w.ws.clone(),
        socket: sock.clone(),
    };
    let input = json!({"tool_name": "SendUserFile", "tool_use_id": "toolu_send1", "cwd": "/workspace",
                       "tool_input": {"files": ["out/chart.png", "file:///workspace/out/data.csv"], "caption": "圖表"}});
    let p = paths.clone();
    let r = tokio::task::spawn_blocking(move || crate::sent_files::hook(&p, &input))
        .await
        .unwrap();
    assert_eq!(r, crate::sent_files::HookOutcome::Sent(2));
    let sent = w.magpie.sent(w.cid);
    let s = &sent["toolu_send1"];
    assert_eq!(s.request.files.len(), 2);
    assert_eq!(s.request.files[0].path, "/workspace/out/chart.png");
    assert_eq!(s.request.files[0].mime_type, "image/png");
    assert_eq!(s.request.caption.as_deref(), Some("圖表"));
    assert_eq!(
        w.magpie.blob(&s.request.files[1].sha256).unwrap().as_ref(),
        b"a,b\n1,2\n"
    );
    // a symlink is refused before anything is sent; a path outside the workspace too
    for bad in ["out/link.txt", "/etc/passwd"] {
        let input = json!({"tool_name": "SendUserFile", "tool_use_id": "toolu_send2",
                           "tool_input": {"files": [bad]}});
        let p = paths.clone();
        let r = tokio::task::spawn_blocking(move || crate::sent_files::hook(&p, &input))
            .await
            .unwrap();
        assert!(matches!(r, crate::sent_files::HookOutcome::Failed(_)), "{bad}: {r:?}");
    }
    assert!(!w.magpie.sent(w.cid).contains_key("toolu_send2"));
    // not a SendUserFile, or no files: nothing
    let p = paths.clone();
    let r = tokio::task::spawn_blocking(move || crate::sent_files::hook(&p, &json!({"tool_name": "Bash"})))
        .await
        .unwrap();
    assert_eq!(r, crate::sent_files::HookOutcome::Skip);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_forced_release_during_a_turn_reports_sandbox_lost() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let (t, _) = w.turn("hang");
    until("running", || w.state_names(t) == ["running"]).await;
    // unforced: refused while busy
    let busy = sup
        .lifecycle
        .release(supervisor_api::control::ReleaseRequest {
            reason: hermit_api::sandbox::ReleaseReason::Idle,
            code: hermit_api::sandbox::ReleaseCallCode::SandboxReleased,
            force: false,
            deadline_ms: 30_000,
        })
        .await;
    assert!(matches!(busy, supervisor_api::control::Outcome::Busy(_)));
    let started = Instant::now();
    let done = sup.lifecycle.release(crate::lifecycle::sigterm_release()).await;
    assert_eq!(done, supervisor_api::control::Outcome::Done);
    assert!(started.elapsed() < crate::limits::SIGTERM_RELEASE_DEADLINE);
    let end = w.ended(t).await;
    assert_eq!(end["state"], "failed");
    assert_eq!(end["error"]["code"], "sandbox.lost");
    // Claude Code is gone and nothing new is admitted
    assert!(sup.driver.claude_pid().is_none());
    assert!(texts(&w).contains("eof") || texts(&w).contains("control interrupt"));
}
