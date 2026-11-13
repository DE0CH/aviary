//! The driver end to end (supervisor 22, unit and integration): a real keeper on a thread, fake hermit and fake
//! magpie behind the reference gateway, the mock model, and a scripted fake `claude` speaking stream-json (this test
//! binary again, [`fake_claude`], started through a wrapper script whose fd 3 is the generation's stdout file).
//!
//! The fake reads the person's text up to the first blank line as a script of `;`-separated steps:
//! `say:X`, `echo` (the whole text and the image count), `fail`, `hang` (until an interrupt), `deaf` (ignores
//! interrupts), `crash:N`, `bg:ID` (a background Bash task), `bgdone:ID:MS` (one that completes after MS and wakes
//! the agent), `wake:MS` (an agent turn after MS), `slowwake:MS` (an agent turn after MS whose result comes 1.5 s
//! later), `ask` (an `AskUserQuestion` that needs a person: its `tool_use`, then the `can_use_tool`), `askbash` (the
//! same beside a Bash call still running), `sendsh` (a shell call running `send-file`, until an interrupt), `img:N` (a tool result carrying N images). The text `/compact` is the
//! command: `status`, 300 ms, then a second `init`, `compact_boundary`, the summary and a `result`. Like Claude Code 2.1.304, the fake answers an open question itself when
//! its stdin closes ("Tool permission stream closed before response received") and calls the model again.

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
    AnswerAsk, CommandBody, KillCommand, KillReason, Resume, SendMessage, StartFile, StartImage, StartTurn,
    TitleCommand,
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
        // one write per line: a SIGKILL between the text and its newline would glue the next generation's first
        // line (its argv) onto this one
        f.write_all(format!("{s}\n").as_bytes()).unwrap();
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
        if msg["message"]["content"] == "/compact" {
            self.note("prompt /compact");
            self.busy = true;
            self.system(json!({"type": "system", "subtype": "status", "status": "compacting"}));
            self.timers.push((
                std::time::Instant::now() + Duration::from_millis(300),
                "compacted:".into(),
            ));
            return;
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
                "sendsh" => {
                    self.assistant(
                        "sending",
                        &[
                            json!({"type": "tool_use", "id": "call_send", "name": "run_shell_command",
                                  "input": {"command": "send-file out.txt"}}),
                        ],
                    );
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
                "img" => {
                    let n: usize = arg.parse().unwrap();
                    self.assistant(
                        "reading",
                        &[json!({"type": "tool_use", "id": "toolu_img", "name": "Read", "input": {"file_path": "a.png"}})],
                    );
                    let image = json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "QUJD"}});
                    let v = json!({"type": "user", "uuid": Uuid::new_v4().to_string(), "parent_tool_use_id": null,
                                   "message": {"role": "user", "content": [
                                       {"type": "tool_result", "tool_use_id": "toolu_img", "content": vec![image; n]}]}});
                    out_line(&mut self.out, &v);
                }
                "wake" | "slowwake" => {
                    let ms: u64 = arg.parse().unwrap();
                    self.timers
                        .push((std::time::Instant::now() + Duration::from_millis(ms), format!("{cmd}:")));
                }
                "ask" | "askbash" => {
                    let mut tools = vec![json!({"type": "tool_use", "id": "toolu_ask", "name": "AskUserQuestion",
                                                "input": {"questions": [{"question": "Which?"}]}})];
                    if cmd == "askbash" {
                        tools.push(json!({"type": "tool_use", "id": "toolu_bash", "name": "Bash",
                                          "input": {"command": "sleep 600"}}));
                    }
                    self.assistant("a question", &tools);
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
            "compacted" => {
                // as Claude Code 2.1.322 answers `/compact`
                self.system(json!({"type": "system", "subtype": "status", "status": null}));
                self.system(json!({"type": "system", "subtype": "init", "session_id": self.session, "model": "m"}));
                self.system(
                    json!({"type": "system", "subtype": "compact_boundary", "uuid": Uuid::new_v4().to_string(),
                                   "compact_metadata": {"trigger": "manual", "pre_tokens": 9, "post_tokens": 3}}),
                );
                let v = json!({"type": "user", "uuid": Uuid::new_v4().to_string(), "parent_tool_use_id": null,
                               "isSynthetic": true, "message": {"role": "user",
                               "content": "This session is being continued from a previous conversation."}});
                out_line(&mut self.out, &v);
                let v = json!({"type": "user", "uuid": Uuid::new_v4().to_string(), "parent_tool_use_id": null,
                               "isReplay": true, "message": {"role": "user",
                               "content": "<local-command-stdout>Compacted </local-command-stdout>"}});
                out_line(&mut self.out, &v);
                self.result(true, "");
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
                if Path::new(".fake-initerror").exists() {
                    self.system(json!({"type": "control_response",
                                       "response": {"subtype": "error", "request_id": id, "error": "refused"}}));
                    return;
                }
                // `.fake-commands`: the answer names slash commands, as Claude Code's does
                let facts = if Path::new(".fake-commands").exists() {
                    json!({"commands": [
                        {"name": "compact", "description": "Keep a summary", "argumentHint": "<instructions>",
                         "builtin": true},
                        {"name": "retire-session", "description": "End the session.\nA second line.", "argumentHint": ""}]})
                } else {
                    json!({})
                };
                self.system(json!({"type": "control_response",
                                   "response": {"subtype": "success", "request_id": id, "response": facts}}));
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
            "set_max_thinking_tokens" | "apply_flag_settings" => {
                let r = &v["request"];
                let what = if sub == "set_max_thinking_tokens" {
                    r["max_thinking_tokens"].to_string()
                } else {
                    r["settings"].to_string()
                };
                self.note(&format!("thinking {sub} {what}"));
                self.system(json!({"type": "control_response",
                                   "response": {"subtype": "success", "request_id": id, "response": {}}}));
            }
            _ => {}
        }
    }
    fn response(&mut self, v: &Value) {
        if self.waiting != Some("answer") {
            return;
        }
        let r = &v["response"]["response"];
        self.note(&format!("answer {}", r["toolUseID"].as_str().unwrap_or("-")));
        let result = json!({"type": "user", "uuid": Uuid::new_v4().to_string(), "parent_tool_use_id": null,
                            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_ask",
                                                                     "content": "answered"}]}});
        out_line(&mut self.out, &result);
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
    fake.note(&format!(
        "model {}",
        aviary_core::env::try_required("ANTHROPIC_MODEL").unwrap_or_default()
    ));
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
                if fake.waiting == Some("answer") {
                    // Claude Code 2.1.304: the open question fails and the model is called again
                    fake.note("eof while asking: the question failed and the model was called again");
                    let v = json!({"type": "user", "uuid": Uuid::new_v4().to_string(), "parent_tool_use_id": null,
                                   "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_ask", "is_error": true,
                                   "content": "Tool permission request failed: AbortError: Tool permission stream closed before response received"}]}});
                    out_line(&mut fake.out, &v);
                    fake.assistant("No response requested.", &[]);
                    fake.result(true, "done");
                }
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
    world_with(HermitConfig::default()).await
}

/// A world whose descriptors compact the conversation at `images` images.
async fn world_compacting(images: u32) -> World {
    let mut cfg = HermitConfig::default();
    cfg.descriptor.claude.compact_at_images = Some(images);
    world_with(cfg).await
}

async fn world_with(hermit: HermitConfig) -> World {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let hermit = FakeHermit::with_config(hermit);
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
        routes: Vec::new(),
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
    // the same stand-in is both harnesses: they speak one protocol, and the argv file says how each was started
    for name in ["claude", "qwen"] {
        let bin = dir.join(name);
        std::fs::write(&bin, &script).unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }
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
                qwen_bin: self.dir.join("qwen").to_string_lossy().into_owned(),
                cwd: self.ws.to_string_lossy().into_owned(),
                uid,
                gid,
            },
            managed_dir: self.dir.join("managed"),
            gen_dir: self.dir.join("gens"),
            claude_dir: self.dir.join("dot-claude"),
            qwen_managed_dir: self.dir.join("managed-qwen"),
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

    /// A turn of a conversation whose person picked `model` (`None`: none picked).
    fn turn_on(&self, text: &str, model: Option<&str>) -> (Uuid, u64) {
        let id = Uuid::now_v7();
        let seq = self.magpie.push_command(
            self.cid,
            Some(id),
            CommandBody::StartTurn(StartTurn {
                harness: magpie_api::Harness::default(),
                carry_over: None,
                text: text.into(),
                files: vec![],
                images: vec![],
                resume: Resume {
                    provider_session_id: Some(self.cid.to_string()),
                },
                sent_at: magpie_api::Timestamp::now(),
                answers: Vec::new(),
                model: model.map(str::to_owned),
                thinking: None,
            }),
        );
        (id, seq)
    }

    /// The model of each generation started so far (the fake's `ANTHROPIC_MODEL`).
    fn gen_models(&self) -> Vec<String> {
        self.fake_log()
            .into_iter()
            .filter_map(|l| l.strip_prefix("model ").map(str::to_owned))
            .collect()
    }

    /// A turn in `harness` on `model`, resuming that harness's session `resume` (`None`: it has none yet).
    fn turn_in(
        &self,
        text: &str,
        harness: magpie_api::Harness,
        model: Option<&str>,
        resume: Option<String>,
    ) -> (Uuid, u64) {
        let id = Uuid::now_v7();
        let seq = self.magpie.push_command(
            self.cid,
            Some(id),
            CommandBody::StartTurn(StartTurn {
                harness,
                carry_over: None,
                text: text.into(),
                files: vec![],
                images: vec![],
                resume: Resume {
                    provider_session_id: resume,
                },
                sent_at: magpie_api::Timestamp::now(),
                answers: Vec::new(),
                model: model.map(str::to_owned),
                thinking: None,
            }),
        );
        (id, seq)
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
                harness: magpie_api::Harness::default(),
                carry_over: None,
                text: text.into(),
                files,
                images,
                resume: Resume {
                    provider_session_id: resume,
                },
                sent_at: magpie_api::Timestamp::now(),
                answers: Vec::new(),
                model: None,
                thinking: None,
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

/// Live frames (8.3) from Claude Code's `stream_event`s: a block's start, its text at its byte offset, its end, each
/// under the message the thread is streaming; the main thread and a subagent's do not mix.
#[test]
fn stream_events_become_live_frames() {
    use magpie_api::live::{BlockType, FrameOp};
    let ev = |parent: Option<&str>, step: Step| Partial {
        parent_tool_use_id: parent.map(str::to_owned),
        step,
    };
    let mut out = PartialOut::default();
    // nothing before the message is known
    assert!(
        out.frame(ev(
            None,
            Step::Text {
                index: 0,
                text: "x".into()
            }
        ))
        .is_none()
    );
    assert!(out.frame(ev(None, Step::Message { id: "msg_1".into() })).is_none());
    let start = out
        .frame(ev(
            None,
            Step::Start {
                index: 0,
                block: BlockType::Text,
                name: None,
            },
        ))
        .unwrap();
    assert_eq!(
        (start.op, start.message_id.as_deref(), start.index, start.block),
        (FrameOp::Start, Some("msg_1"), Some(0), Some(BlockType::Text))
    );
    let a = out
        .frame(ev(
            None,
            Step::Text {
                index: 0,
                text: "你好".into(),
            },
        ))
        .unwrap();
    assert_eq!(
        (a.op, a.offset, a.text.as_deref()),
        (FrameOp::Delta, Some(0), Some("你好"))
    );
    // a subagent's message meanwhile, under its own call
    assert!(
        out.frame(ev(Some("toolu_a"), Step::Message { id: "msg_sub".into() }))
            .is_none()
    );
    let sub = out
        .frame(ev(
            Some("toolu_a"),
            Step::Start {
                index: 0,
                block: BlockType::ToolUse,
                name: Some("Bash".into()),
            },
        ))
        .unwrap();
    assert_eq!(
        (
            sub.message_id.as_deref(),
            sub.name.as_deref(),
            sub.parent_tool_use_id.as_deref()
        ),
        (Some("msg_sub"), Some("Bash"), Some("toolu_a"))
    );
    // the main thread goes on where it was: offsets are bytes
    let b = out
        .frame(ev(
            None,
            Step::Text {
                index: 0,
                text: ", world".into(),
            },
        ))
        .unwrap();
    assert_eq!(
        (b.message_id.as_deref(), b.offset, b.text.as_deref()),
        (Some("msg_1"), Some(6), Some(", world"))
    );
    let stop = out.frame(ev(None, Step::Stop { index: 0 })).unwrap();
    assert_eq!((stop.op, stop.index), (FrameOp::Stop, Some(0)));
    assert!(out.frame(ev(None, Step::Other)).is_none());
    // the next message starts its blocks from 0 again
    out.frame(ev(None, Step::Message { id: "msg_2".into() }));
    out.frame(ev(
        None,
        Step::Start {
            index: 0,
            block: BlockType::Thinking,
            name: None,
        },
    ));
    let c = out
        .frame(ev(
            None,
            Step::Text {
                index: 0,
                text: "hm".into(),
            },
        ))
        .unwrap();
    assert_eq!((c.message_id.as_deref(), c.offset), (Some("msg_2"), Some(0)));
}

/// The conversation's picked model (`start_turn.model`): a generation that runs another model ends before the turn,
/// which resumes the conversation on a new one started on the pick; the same pick again keeps the generation; with
/// none picked the next turn is back on the models route's default.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_runs_on_the_conversations_picked_model() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    until("pre-spawn", || !w.spawns().is_empty()).await;
    let default = w.gen_models()[0].clone();
    assert!(!default.is_empty(), "the models route's default");
    let (t1, _) = w.turn_on("say:one", None);
    assert_eq!(w.ended(t1).await["state"], "completed");
    w.idle(&sup).await;
    assert_eq!(
        w.gen_models().as_slice(),
        std::slice::from_ref(&default),
        "none picked: the pre-spawned generation runs it"
    );

    let (t2, _) = w.turn_on("say:two", Some("picked/model-b"));
    assert_eq!(w.ended(t2).await["state"], "completed", "{}", texts(&w));
    w.idle(&sup).await;
    assert_eq!(
        w.gen_models(),
        [default.clone(), "picked/model-b".to_owned()],
        "{}",
        texts(&w)
    );
    assert!(
        w.spawns()[1].contains(&format!("--resume {}", w.cid)),
        "the conversation goes on"
    );

    let (t3, _) = w.turn_on("say:three", Some("picked/model-b"));
    assert_eq!(w.ended(t3).await["state"], "completed");
    w.idle(&sup).await;
    assert_eq!(w.spawns().len(), 2, "the same pick keeps the generation");
    sup.persisted.flush().await;
    let saved = StateFile::new(w.dir.join("state")).load().unwrap().unwrap();
    let g = saved.generation.unwrap();
    assert_eq!((g.model.as_str(), g.picked), ("picked/model-b", true));

    let (t4, _) = w.turn_on("say:four", None);
    assert_eq!(w.ended(t4).await["state"], "completed");
    assert_eq!(
        w.gen_models(),
        [default.clone(), "picked/model-b".to_owned(), default],
        "none picked any more: the default again"
    );
    assert_eq!(w.prompts().len(), 4);
}

/// A turn in the other harness ends the running generation and starts that harness: Qwen Code with its own argv,
/// its managed settings written for the turn's model, its session reported with its own transcript's place. A turn
/// back in Claude Code starts Claude Code again on its session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_in_another_harness_starts_that_harness() {
    use magpie_api::Harness;
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    until("pre-spawn", || !w.spawns().is_empty()).await;
    assert!(
        w.spawns()[0].contains("--permission-mode bypassPermissions"),
        "Claude Code first"
    );

    let (t1, _) = w.turn_in("say:one", Harness::Qwen, Some("vendor/video"), None);
    assert_eq!(w.ended(t1).await["state"], "completed");
    let spawns = w.spawns();
    assert_eq!(spawns.len(), 2, "{spawns:?}");
    let q = &spawns[1];
    for part in [
        "--approval-mode yolo".to_owned(),
        "--model vendor/video".to_owned(),
        format!("--session-id {}", w.cid),
        "--exclude-tools=cron_create".to_owned(),
        "--append-system-prompt".to_owned(),
    ] {
        assert!(q.contains(&part), "{part} in {q}");
    }
    assert!(!q.contains("--permission-mode"), "{q}");
    let running = &w.states(t1)[0];
    assert_eq!(running["state"], "running");
    assert_eq!(running["harness"], "qwen");
    assert_eq!(running["providerSessionId"], w.cid.to_string());
    assert_eq!(
        running["transcriptPath"],
        format!("qwen/projects/-workspace/chats/{}.jsonl", w.cid)
    );
    let settings: Value =
        serde_json::from_slice(&std::fs::read(w.dir.join("managed-qwen/settings.json")).unwrap()).unwrap();
    assert_eq!(settings["model"]["name"], "vendor/video");
    assert!(w.dir.join("managed-qwen/skills").is_dir());
    w.idle(&sup).await;

    // back in Claude Code: the Qwen Code generation ends, Claude Code resumes its own session
    let (t2, _) = w.turn_in("say:two", Harness::Claude, None, Some(w.cid.to_string()));
    assert_eq!(w.ended(t2).await["state"], "completed");
    let spawns = w.spawns();
    assert_eq!(spawns.len(), 3, "{spawns:?}");
    assert!(spawns[2].contains(&format!("--resume {}", w.cid)) && spawns[2].contains("--permission-mode"));
    let running = &w.states(t2)[0];
    assert_eq!(running.get("harness"), None, "Claude Code is the unnamed harness");
    assert_eq!(
        running["transcriptPath"],
        format!("claude/projects/-workspace/{}.jsonl", w.cid)
    );
    w.idle(&sup).await;
}

/// Qwen Code has no `SendUserFile` tool: its shell runs `send-file`, which names no tool call, and the files land
/// on the shell call that runs it. A `send-file` no shell call runs is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_send_file_command_puts_its_files_on_the_shell_call_that_runs_it() {
    use std::os::fd::OwnedFd;
    use supervisor_api::local::{SendUserFileRequest, SentFile};
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let (t, _) = w.turn_in("sendsh", magpie_api::Harness::Qwen, Some("vendor/video"), None);
    until("running", || w.states(t).iter().any(|s| s["state"] == "running")).await;
    let file = w.dir.join("out.txt");
    std::fs::write(&file, b"the film").unwrap();
    let send = || {
        let fd: OwnedFd = std::fs::File::open(&file).unwrap().into();
        let req = SendUserFileRequest {
            tool_use_id: String::new(),
            caption: Some("here".into()),
            files: vec![SentFile {
                path: "/workspace/out.txt".into(),
                size: 8,
            }],
        };
        crate::lifecycle::Agent::send_user_file(&*sup.driver, req, vec![fd])
    };
    assert_eq!(send().await.unwrap().sent, 1);
    let kept = w.magpie.sent(w.cid);
    assert_eq!(kept.keys().collect::<Vec<_>>(), ["call_send"]);
    assert_eq!(kept["call_send"].request.files[0].path, "/workspace/out.txt");
    // the call has had its send: a second one in the same command is told so
    let refused = send().await.unwrap_err();
    assert!(refused.contains("once per shell command"), "{refused}");
    assert_eq!(w.magpie.sent(w.cid).len(), 1);
    w.magpie
        .push_command(w.cid, Some(t), CommandBody::Interrupt(magpie_api::Empty {}));
    assert_eq!(w.ended(t).await["state"], "canceled");
    w.idle(&sup).await;
}

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
    // decision 15: the descriptor's managed skills, no MCP file
    let skills = &sup.driver.hermit.descriptor().unwrap().claude.managed_skills;
    assert!(!skills.is_empty());
    for (k, text) in skills {
        assert_eq!(
            &std::fs::read_to_string(w.dir.join("managed/.claude/skills").join(k)).unwrap(),
            text,
            "{k}"
        );
    }
    assert!(!w.dir.join("managed/managed-mcp.json").exists());

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
        format!("claude/projects/-workspace/{}.jsonl", w.cid)
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
    let spawns = w.spawns();
    assert!(
        spawns.last().unwrap().contains(&format!("--resume {}", w.cid)),
        "{spawns:#?}\nfake:\n{}",
        w.fake_log().join("\n")
    );
}

/// A kill that lands after the `running` report but before the prompt is in Claude Code's stdin: the write fails
/// before the exit is handled, and the turn still ends with the kill's reason (a watchdog kill `failed run.error`, a
/// stop `canceled`), never as an exit nobody asked for (`run.exited`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_kill_during_the_prompt_write_ends_the_turn_with_the_kills_reason() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    sup.driver
        .race_kill_with_prompt
        .store(true, std::sync::atomic::Ordering::SeqCst);
    for (reason, state, code) in [
        (KillReason::Watchdog, "failed", Some("run.error")),
        (KillReason::Stop, "canceled", None),
    ] {
        let (t, s) = w.turn("deaf");
        until("running", || w.state_names(t) == ["running"]).await;
        let k = w
            .magpie
            .push_command(w.cid, Some(t), CommandBody::Kill(KillCommand { reason }));
        let end = w.ended(t).await;
        assert_eq!(end["state"], state, "{end}");
        assert_eq!(end["error"]["code"].as_str(), code, "{end}");
        assert_eq!(w.state_names(t), ["running", state], "one end");
        w.acked(k).await;
        assert!(
            w.acked(s).await.error.is_some(),
            "the kill landed before the prompt write"
        );
        w.idle(&sup).await;
    }
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
    // a Traditional Chinese title of one full directory entry (251 bytes and ".pdf")
    let long = format!("{}.pdf", "報".repeat(83));
    assert_eq!(long.len(), 253);
    let files = vec![
        StartFile {
            upload_id: Uuid::new_v4(),
            name: "Q3 report.pdf".into(),
            mime_type: "application/pdf".into(),
            size: pdf.len() as u64,
            workspace_path: "uploads/Q3 report.pdf".into(),
            blob: pdf_sha.clone(),
        },
        StartFile {
            upload_id: Uuid::new_v4(),
            name: long.clone(),
            mime_type: "application/pdf".into(),
            size: pdf.len() as u64,
            workspace_path: format!("uploads/{long}"),
            blob: pdf_sha,
        },
    ];
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
    assert_eq!(std::fs::read(w.ws.join("uploads").join(&long)).unwrap(), pdf);
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
async fn a_message_with_attachments_joins_the_running_turn() {
    let w = world().await;
    let _sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let pdf = b"%PDF-1.4 report".to_vec();
    let (pdf_sha, img, small) = (
        w.magpie.seed_blob(&pdf),
        w.magpie.seed_blob(b"orig"),
        w.magpie.seed_blob(b"copy"),
    );
    let message = |file: &str, image: &str| {
        CommandBody::SendAttached(magpie_api::sandbox::SendAttached {
            text: "echo".into(),
            files: vec![StartFile {
                upload_id: Uuid::new_v4(),
                name: "Q3 report.pdf".into(),
                mime_type: "application/pdf".into(),
                size: pdf.len() as u64,
                workspace_path: format!("uploads/{file}"),
                blob: pdf_sha.clone(),
            }],
            images: vec![StartImage {
                upload_id: Uuid::new_v4(),
                mime_type: "image/png".into(),
                workspace_path: format!("uploads/{image}"),
                blob: img.clone(),
                model_blob: small.clone(),
                model_mime: "image/png".into(),
            }],
            sent_at: magpie_api::Timestamp(time::OffsetDateTime::now_utc()),
        })
    };
    let (t, _) = w.turn("ask;say:after");
    let calls = w.magpie.clone();
    until("the ask", || !calls.script().calls_to("ask_open").is_empty()).await;
    // for a turn that is not the running one: not delivered, and no file is written
    let other = w
        .magpie
        .push_command(w.cid, Some(Uuid::now_v7()), message("no.pdf", "no.png"));
    assert_eq!(w.acked(other).await.delivered, Some(false));
    assert!(!w.ws.join("uploads/no.pdf").exists());
    // for the running turn: its files are in the workspace, then it is written to Claude Code
    let sent = w.magpie.push_command(w.cid, Some(t), message("Q3 report.pdf", "a.png"));
    assert_eq!(w.acked(sent).await.delivered, Some(true));
    assert_eq!(std::fs::read(w.ws.join("uploads/Q3 report.pdf")).unwrap(), pdf);
    assert_eq!(std::fs::read(w.ws.join("uploads/a.png")).unwrap(), b"orig");
    until("the message on Claude Code's stdin", || {
        w.fake_log().iter().filter(|l| l.as_str() == "stdin user").count() == 2
    })
    .await;
    assert!(
        w.states(t).iter().all(|s| s["state"] == "running"),
        "the turn still runs"
    );
    // a file that cannot be written: the error, nothing written to Claude Code (magpie holds the message)
    crate::uploads::FAIL_WRITES_WITH.store(libc::EDQUOT, std::sync::atomic::Ordering::SeqCst);
    let failed = w.magpie.push_command(w.cid, Some(t), message("b.pdf", "b.png"));
    let ack = w.acked(failed).await;
    crate::uploads::FAIL_WRITES_WITH.store(0, std::sync::atomic::Ordering::SeqCst);
    assert!(ack.error.is_some() && ack.delivered.is_none(), "{ack:?}");
    assert_eq!(w.fake_log().iter().filter(|l| l.as_str() == "stdin user").count(), 2);
    // what Claude Code got (this fake answers it in a run of its own after the turn, echoing it): the text with
    // the paths, and the image's model copy as a block
    let id = calls.script().calls_to("ask_open")[0].body.clone().unwrap()["askId"]
        .as_str()
        .unwrap()
        .to_owned();
    let a = w.magpie.push_command(
        w.cid,
        None,
        CommandBody::AnswerAsk(AnswerAsk {
            ask_id: id,
            allow: true,
            updated_input: None,
            message: None,
        }),
    );
    w.acked(a).await;
    assert_eq!(w.ended(t).await["state"], "completed");
    until("the message's own run", || {
        std::fs::read_to_string(w.dir.join("gens/1/stdout.ndjson")).is_ok_and(|o| o.contains("images=1"))
    })
    .await;
    let out = std::fs::read_to_string(w.dir.join("gens/1/stdout.ndjson")).unwrap();
    assert!(
        out.contains("<files_input>") && out.contains("/workspace/uploads/Q3 report.pdf"),
        "{out}"
    );
    assert!(out.contains("<image_uploads>"), "{out}");
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
                harness: magpie_api::Harness::default(),
                carry_over: None,
                text: "say:once".into(),
                files: vec![],
                images: vec![],
                resume: Resume {
                    provider_session_id: None,
                },
                sent_at: magpie_api::Timestamp::now(),
                answers: Vec::new(),
                model: None,
                thinking: None,
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
        // what step 5 persists in one change: the record, the pending input and the person's turn
        let mut prior = first.persisted.read(Clone::clone);
        prior.delivered.insert(
            seq.to_string(),
            Delivered {
                state: "written".into(),
                msg_id: Some(hex(&id)),
                generation: Some(n),
                pending_inputs_after: Some(1),
                turn_id: Some(turn.to_string()),
                error: None,
            },
        );
        prior.cmd_after = seq;
        prior.run.state = "running".into();
        prior.run.turn_id = Some(turn.to_string());
        prior.run.origin = Some("person".into());
        prior.run.pending_inputs = 1;
        drop(first);
        if keeper_had_it {
            // Claude Code answers while no supervisor runs, and the new one's tail reads the answer before the
            // command is replayed (the command poll is held back meanwhile)
            until("answered", || w.fake_log().iter().any(|l| l == "stdin user")).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        let hold = w.magpie.faults().on(
            "commands",
            aviary_fakes::fault::FaultKind::Unavailable { retry_after_secs: 1 },
        );
        let second = w.attach(Some(prior), crate::start::Reattach::Alive { n, pid }).await;
        if keeper_had_it {
            let end = w.ended(turn).await;
            assert_eq!(
                end["state"], "completed",
                "credited to the person's turn before the replay"
            );
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
        w.magpie.faults().remove(hold);
        w.acked(seq).await;
        assert!(
            w.magpie.script().calls_to("agent_turn").is_empty(),
            "no agent turn for the person's answer"
        );
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

/// 7.5: Claude Code answers before the keeper's acknowledgement of the prompt reaches the driver (the tail reads
/// the answer first): the answer is the person's turn, never a phantom agent turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_answer_read_before_the_prompt_write_returns_is_the_persons_turn() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    until("pre-spawn", || !w.spawns().is_empty()).await;
    STDIN_ACK_DELAY_MS.store(800, std::sync::atomic::Ordering::SeqCst);
    for i in 0..3 {
        let (t, s) = w.turn_with(&format!("say:fast {i}"), Some(w.cid.to_string()), vec![], vec![]);
        assert_eq!(w.ended(t).await["state"], "completed");
        assert_eq!(w.acked(s).await.delivered, Some(true));
        assert_eq!(w.state_names(t), ["running", "completed"]);
        w.idle(&sup).await;
    }
    STDIN_ACK_DELAY_MS.store(0, std::sync::atomic::Ordering::SeqCst);
    assert!(w.magpie.script().calls_to("agent_turn").is_empty(), "{}", texts(&w));
}

/// An interrupt (and a kill) for a person's turn queued behind an agent turn stops Claude Code: magpie already has
/// that turn `running`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::many_single_char_names)]
async fn a_queued_persons_turn_can_be_interrupted_and_killed() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    for kill in [false, true] {
        let (t, _) = w.turn_with("say:x;slowwake:100", Some(w.cid.to_string()), vec![], vec![]);
        w.ended(t).await;
        let before = w.magpie.script().calls_to("agent_turn").len();
        until("an agent turn", || {
            w.magpie.script().calls_to("agent_turn").len() > before
        })
        .await;
        let (p, _) = w.turn_with("deaf", Some(w.cid.to_string()), vec![], vec![]);
        until("the person's turn running", || {
            w.state_names(p).first().is_some_and(|s| s == "running")
        })
        .await;
        assert!(
            lock(&sup.driver.st).queued.as_ref().is_some_and(|q| q.id == p),
            "queued"
        );
        let body = if kill {
            CommandBody::Kill(KillCommand {
                reason: KillReason::Stop,
            })
        } else {
            CommandBody::Interrupt(magpie_api::Empty {})
        };
        let started = Instant::now();
        let c = w.magpie.push_command(w.cid, Some(p), body);
        let end = w.ended(p).await;
        assert_eq!(end["state"], "canceled", "kill: {kill}");
        w.acked(c).await;
        if kill {
            assert!(started.elapsed() < Duration::from_secs(8), "killed at once");
        } else {
            // the agent turn Claude Code was in, then the person's turn once it is the turn
            let n = w.fake_log().iter().filter(|l| *l == "control interrupt").count();
            assert_eq!(n, 2, "{}", texts(&w));
        }
        w.idle(&sup).await;
    }
}

/// A blob read that fails while magpie (or the gateway) is unavailable is retried; an interrupt or a forced release
/// that arrives while the turn is still being prepared stops it before its message is written.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::many_single_char_names)]
async fn a_turn_being_prepared_survives_an_outage_and_can_be_stopped() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    until("pre-spawn", || !w.spawns().is_empty()).await;
    let pdf = b"%PDF-1.4 x".to_vec();
    let sha = w.magpie.seed_blob(&pdf);
    let file = |name: &str| StartFile {
        upload_id: Uuid::new_v4(),
        name: name.into(),
        mime_type: "application/pdf".into(),
        size: pdf.len() as u64,
        workspace_path: format!("uploads/{name}"),
        blob: sha.clone(),
    };
    // a transient outage: retried, then the turn runs
    let fault = w.magpie.faults().on(
        "sandbox_blob_get",
        aviary_fakes::fault::FaultKind::Unavailable { retry_after_secs: 1 },
    );
    let (t, _) = w.turn_with("say:x", Some(w.cid.to_string()), vec![file("a.pdf")], vec![]);
    until("retried", || w.magpie.faults().hits() >= 2).await;
    w.magpie.faults().remove(fault);
    assert_eq!(w.ended(t).await["state"], "completed");
    assert_eq!(std::fs::read(w.ws.join("uploads/a.pdf")).unwrap(), pdf);
    w.idle(&sup).await;

    // an interrupt while preparing: canceled, nothing written to Claude Code
    let prompts = w.prompts().len();
    let fault = w.magpie.faults().on(
        "sandbox_blob_get",
        aviary_fakes::fault::FaultKind::Unavailable { retry_after_secs: 1 },
    );
    let (t, s) = w.turn_with("say:never", Some(w.cid.to_string()), vec![file("b.pdf")], vec![]);
    let d = sup.driver.clone();
    until("starting", || lock(&d.st).starting == Some(t)).await;
    let started = Instant::now();
    let i = w
        .magpie
        .push_command(w.cid, Some(t), CommandBody::Interrupt(magpie_api::Empty {}));
    // the outage goes on: the interrupt does not wait for it (7.8)
    assert_eq!(w.ended(t).await["state"], "canceled");
    assert_eq!(w.acked(s).await.delivered, Some(false));
    w.acked(i).await;
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    w.magpie.faults().remove(fault);
    assert_eq!(w.prompts().len(), prompts, "{}", texts(&w));
    assert!(lock(&d.st).cancel_starting.is_none());
    let parts: Vec<_> = std::fs::read_dir(w.ws.join("uploads"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(crate::uploads::PART_PREFIX))
        .collect();
    assert!(parts.is_empty(), "the stopped write's part file is gone");
    w.idle(&sup).await;

    // a forced release while preparing: failed sandbox.lost, nothing written, nothing spawned after it
    let spawns = w.spawns().len();
    let fault = w.magpie.faults().on(
        "sandbox_blob_get",
        aviary_fakes::fault::FaultKind::Unavailable { retry_after_secs: 1 },
    );
    let (t, _) = w.turn_with("say:never", Some(w.cid.to_string()), vec![file("c.pdf")], vec![]);
    until("starting", || lock(&d.st).starting == Some(t)).await;
    let started = Instant::now();
    let release = {
        let l = sup.lifecycle.clone();
        tokio::spawn(async move { l.release(crate::lifecycle::sigterm_release()).await })
    };
    let end = w.ended(t).await;
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert_eq!(end["state"], "failed");
    assert_eq!(end["error"]["code"], "sandbox.lost");
    assert_eq!(release.await.unwrap(), supervisor_api::control::Outcome::Done);
    w.magpie.faults().remove(fault);
    assert_eq!(w.prompts().len(), prompts);
    assert_eq!(w.spawns().len(), spawns);
    assert!(sup.driver.claude_pid().is_none());
}

/// The app completes slash commands after `/`: the first turn of a Claude Code carries the commands its
/// `initialize` answer named to magpie, once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_generations_slash_commands_ride_its_first_running_report() {
    let w = world().await;
    std::fs::write(w.ws.join(".fake-commands"), b"").unwrap();
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let (first, _) = w.turn("say:one");
    w.ended(first).await;
    w.idle(&sup).await;
    let running = |t| {
        w.states(t)
            .into_iter()
            .find(|s| s["state"] == "running")
            .expect("a running report")
    };
    assert_eq!(
        running(first)["commands"],
        json!([
            {"name": "compact", "description": "Keep a summary", "argumentHint": "<instructions>"},
            {"name": "retire-session", "description": "End the session.", "argumentHint": ""}])
    );
    // the same Claude Code's next turn says nothing new
    let (second, _) = w.turn("say:two");
    w.ended(second).await;
    assert!(running(second).get("commands").is_none());
}

/// 7.9: initialisations Claude Code refuses count as a crash loop: the next turn is refused without a spawn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refused_initialisations_are_a_crash_loop() {
    let w = world().await;
    std::fs::write(w.ws.join(".fake-initerror"), b"").unwrap();
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let mut refused = false;
    for _ in 0..4 {
        let (t, _) = w.turn_with("say:x", Some(w.cid.to_string()), vec![], vec![]);
        let end = w.ended(t).await;
        assert_eq!(end["state"], "failed");
        w.idle(&sup).await;
        if end["error"]["message"] == "Claude Code would not start" {
            refused = true;
            break;
        }
        assert!(
            end["error"]["message"]
                .as_str()
                .unwrap()
                .contains("refused to initialise"),
            "{end}"
        );
    }
    assert!(refused, "{}", texts(&w));
    // the pre-spawn and two turns: three refused initialisations
    assert_eq!(w.spawns().len(), 3);
}

/// A transcript that holds a turn's end back (8.2) holds back nothing else: commands about other things are
/// acked, and the end follows once the lines are stored.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_waiting_for_its_lines_holds_back_only_itself() {
    #[derive(Default)]
    struct Gate(tokio::sync::Notify, std::sync::atomic::AtomicBool);
    impl Transcript for Gate {
        fn set_turn(&self, _turn: Option<Uuid>) {}
        fn wait_stored(&self, _uuids: Vec<String>) -> BoxFuture<'_, crate::cc::Stored> {
            Box::pin(async move {
                while !self.1.load(std::sync::atomic::Ordering::SeqCst) {
                    let _ = tokio::time::timeout(Duration::from_millis(50), self.0.notified()).await;
                }
                crate::cc::Stored::All
            })
        }
        fn facts(&self) -> magpie_api::sandbox::CcFacts {
            crate::cc::Stored0.facts()
        }
    }
    let w = world().await;
    let gate = Arc::new(Gate::default());
    let _sup = w.attach_with(None, crate::start::Reattach::Nothing, gate.clone()).await;
    let (t, _) = w.turn("say:x");
    until("answered", || w.prompts().len() == 1).await;
    let title = w.magpie.push_command(
        w.cid,
        None,
        CommandBody::Title(TitleCommand {
            user_prompt: format!("{} 你好", mock_model::TITLE_PROMPT_PREFIX),
            max_tokens: 100,
        }),
    );
    assert!(w.acked(title).await.response.is_some());
    assert_eq!(w.state_names(t), ["running"], "the end still waits");
    gate.1.store(true, std::sync::atomic::Ordering::SeqCst);
    gate.0.notify_waiters();
    assert_eq!(w.ended(t).await["state"], "completed");
}

#[test]
fn ids_derived_from_a_line_are_the_same_when_it_is_read_again() {
    let started = OffsetDateTime::now_utc();
    let a = agent_turn_id("boot", 3, 4096, started);
    assert_eq!(a, agent_turn_id("boot", 3, 4096, started));
    assert_ne!(a, agent_turn_id("boot", 3, 4097, started));
    assert_ne!(a, agent_turn_id("boot2", 3, 4096, started));
    assert_eq!(a.get_version_num(), 7);
    assert_eq!(ask_id("boot", 3, 10), ask_id("boot", 3, 10));
    assert_ne!(ask_id("boot", 3, 10), ask_id("boot", 4, 10));
}

/// A transcript whose end-of-turn wait (8.2) holds until the test opens it.
#[derive(Default)]
struct Gate(tokio::sync::Notify, std::sync::atomic::AtomicBool);

impl Gate {
    fn open(&self) {
        self.1.store(true, std::sync::atomic::Ordering::SeqCst);
        self.0.notify_waiters();
    }
}

impl Transcript for Gate {
    fn set_turn(&self, _turn: Option<Uuid>) {}
    fn wait_stored(&self, _uuids: Vec<String>) -> BoxFuture<'_, crate::cc::Stored> {
        Box::pin(async move {
            while !self.1.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = tokio::time::timeout(Duration::from_millis(50), self.0.notified()).await;
            }
            crate::cc::Stored::All
        })
    }
    fn facts(&self) -> magpie_api::sandbox::CcFacts {
        crate::cc::Stored0.facts()
    }
}

/// magpie 8.5 rule 2 (crates/magpie/src/turns/reports.rs `reconcile_active`): an acked turn the heartbeat neither
/// names nor reports as its `lastTurn`, with the sandbox idle, is failed "the sandbox no longer knows this turn".
fn rule_2_fails(hb: &Heartbeat, turn: Uuid) -> bool {
    hb.state == HeartbeatState::Idle && hb.turn_id.is_none() && hb.last_turn.as_ref().is_none_or(|l| l.id != turn)
}

/// A turn whose end report waits for its lines (8.2), or for magpie, is still the turn in the heartbeat: magpie's
/// reconciliation must not fail it meanwhile. Once the end is answered, it is the `lastTurn`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_heartbeat_names_a_turn_whose_end_is_on_its_way() {
    let w = world().await;
    let gate = Arc::new(Gate::default());
    let sup = w.attach_with(None, crate::start::Reattach::Nothing, gate.clone()).await;
    let (t, _) = w.turn("say:x");
    until("answered", || w.prompts().len() == 1).await;
    let d = sup.driver.clone();
    until("ended locally", || {
        lock(&d.st).turn.is_none() && !lock(&d.st).ending.is_empty()
    })
    .await;
    for _ in 0..5 {
        let hb = sup.driver.heartbeat();
        assert_eq!(hb.turn_id, Some(t), "{hb:?}");
        assert_eq!(hb.state, HeartbeatState::Running);
        assert!(!rule_2_fails(&hb, t));
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // a restart meanwhile: run.json keeps the ending turn, and the next supervisor sends its end
    sup.persisted.flush().await;
    let saved = sup.persisted.read(|s| s.ending.clone());
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].turn_id, t.to_string());
    assert_eq!(saved[0].last.outcome, "completed");
    // the lines are stored: the end goes, and the heartbeat names the turn as its lastTurn
    gate.open();
    assert_eq!(w.ended(t).await["state"], "completed");
    until("answered by magpie", || lock(&d.st).ending.is_empty()).await;
    let hb = sup.driver.heartbeat();
    assert_eq!(hb.turn_id, None);
    assert_eq!(hb.last_turn.as_ref().map(|l| l.id), Some(t));
    assert!(!rule_2_fails(&hb, t));
}

/// A restarted supervisor sends the end of a turn the previous one had ended but not got answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_sends_the_end_magpie_had_not_answered() {
    let w = world().await;
    let first = w.attach(None, crate::start::Reattach::Nothing).await;
    let fault = w.magpie.faults().on(
        "turn_state",
        aviary_fakes::fault::FaultKind::Unavailable { retry_after_secs: 1 },
    );
    let (t, _) = w.turn("say:x");
    let d = first.driver.clone();
    until("ended locally", || {
        lock(&d.st).turn.is_none() && !lock(&d.st).ending.is_empty()
    })
    .await;
    first.persisted.flush().await;
    let prior = first.persisted.read(Clone::clone);
    let (n, pid) = {
        let st = lock(&first.driver.st);
        let g = st.live.as_ref().unwrap();
        (g.n, g.pid)
    };
    first.driver.abort_all();
    drop(first);
    w.magpie.faults().remove(fault);
    let second = w.attach(Some(prior), crate::start::Reattach::Alive { n, pid }).await;
    assert_eq!(w.ended(t).await["state"], "completed");
    let d = second.driver.clone();
    until("answered by magpie", || lock(&d.st).ending.is_empty()).await;
    assert_eq!(second.driver.heartbeat().last_turn.map(|l| l.id), Some(t));
}

/// 8.2: a result whose lines are not stored yet, then the generation dies before it writes them: the end is sent
/// (without `lastUuid`) rather than waited for for ever, and the turn's lane and a release's drain are free.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_end_waiting_for_lines_a_dead_generation_never_wrote_is_sent() {
    let w = world().await;
    let gate = Arc::new(Gate::default());
    let sup = w.attach_with(None, crate::start::Reattach::Nothing, gate.clone()).await;
    let (t, _) = w.turn("say:x");
    let d = sup.driver.clone();
    until("ended locally", || {
        lock(&d.st).turn.is_none() && !lock(&d.st).ending.is_empty()
    })
    .await;
    assert_eq!(w.state_names(t), ["running"]);
    let n = lock(&d.st).live.as_ref().unwrap().n;
    sup.driver.kill_gen(n, Why::Graceful).await;
    let started = Instant::now();
    let end = w.ended(t).await;
    assert_eq!(end["state"], "completed");
    assert!(end.get("lastUuid").is_none_or(Value::is_null), "{end}");
    assert!(started.elapsed() < Duration::from_secs(8), "{:?}", started.elapsed());
    until("answered by magpie", || lock(&d.st).ending.is_empty()).await;
    assert!(
        sup.driver
            .reporter()
            .drain(Instant::now() + Duration::from_secs(2))
            .await
    );
}

/// A supervisor restart while a generation initialises: that generation is ended before another starts, never left
/// running beside it on the same session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_during_initialisation_leaves_no_orphan() {
    let w = world().await;
    std::fs::write(w.ws.join(".fake-noinit"), b"").unwrap();
    let first = w.attach(None, crate::start::Reattach::Nothing).await;
    let d = first.driver.clone();
    until("spawned", || lock(&d.st).live.is_some()).await;
    first.persisted.flush().await;
    let prior = first.persisted.read(Clone::clone);
    let (n, pid) = {
        let st = lock(&first.driver.st);
        let g = st.live.as_ref().unwrap();
        assert!(!g.initialized);
        (g.n, g.pid)
    };
    assert!(prior.generation.as_ref().is_some_and(|g| !g.initialized));
    first.driver.abort_all();
    drop(first);
    std::fs::remove_file(w.ws.join(".fake-noinit")).unwrap();
    let second = w.attach(Some(prior), crate::start::Reattach::Alive { n, pid }).await;
    let rec = tokio::task::block_in_place(|| w.keeper.wait_exit(pid));
    assert_eq!(rec.pid, pid, "the stale generation is gone");
    let d = second.driver.clone();
    until("a new generation initialised", || {
        lock(&d.st).live.as_ref().is_some_and(|g| g.n > n && g.initialized)
    })
    .await;
    let (t, _) = w.turn_with("say:x", Some(w.cid.to_string()), vec![], vec![]);
    assert_eq!(w.ended(t).await["state"], "completed");
    assert_eq!(w.spawns().len(), 2, "{}", texts(&w));
}

/// A replayed `written` command with a file sends the bytes it recorded: nothing is downloaded or renamed onto
/// `/workspace/uploads` again (the agent edited the file since), even while magpie's blobs are unavailable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replay_with_a_file_rewrites_nothing() {
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
    first.driver.abort_all();
    first.persisted.flush().await;
    let pdf = b"%PDF-1.4 original".to_vec();
    let sha = w.magpie.seed_blob(&pdf);
    let file = StartFile {
        upload_id: Uuid::new_v4(),
        name: "a.pdf".into(),
        mime_type: "application/pdf".into(),
        size: pdf.len() as u64,
        workspace_path: "uploads/a.pdf".into(),
        blob: sha,
    };
    let (turn, seq) = w.turn_with("say:once", None, vec![file], vec![]);
    // what the first supervisor did before it went: the file written, the message kept, the record and the turn
    std::fs::create_dir_all(w.ws.join("uploads")).unwrap();
    std::fs::write(w.ws.join("uploads/a.pdf"), &pdf).unwrap();
    let path = w.ws.join("uploads/a.pdf").to_string_lossy().into_owned();
    let line = prompt::user_line(
        &prompt::content(
            prompt::text(
                "say:once",
                &[prompt::Attached {
                    path,
                    name: Some("a.pdf".into()),
                }],
                &[],
            ),
            &[],
        ),
        &magpie_api::Timestamp::now().to_wire(),
    );
    std::fs::write(w.dir.join(format!("gens/{n}/stdin-{seq}.ndjson")), &line).unwrap();
    let boot_id = "b00700000000000a";
    let mut prior = first.persisted.read(Clone::clone);
    prior.delivered.insert(
        seq.to_string(),
        Delivered {
            state: "written".into(),
            msg_id: Some(hex(&msg_id(boot_id, seq))),
            generation: Some(n),
            pending_inputs_after: Some(1),
            turn_id: Some(turn.to_string()),
            error: None,
        },
    );
    prior.cmd_after = seq;
    prior.run.state = "running".into();
    prior.run.turn_id = Some(turn.to_string());
    prior.run.origin = Some("person".into());
    prior.run.pending_inputs = 1;
    drop(first);
    // the agent edits its upload, and magpie's blobs are unavailable
    std::fs::write(w.ws.join("uploads/a.pdf"), b"edited by the agent").unwrap();
    let fault = w.magpie.faults().on(
        "sandbox_blob_get",
        aviary_fakes::fault::FaultKind::Unavailable { retry_after_secs: 1 },
    );
    let second = w.attach(Some(prior), crate::start::Reattach::Alive { n, pid }).await;
    assert_eq!(w.acked(seq).await.delivered, Some(true));
    assert_eq!(w.ended(turn).await["state"], "completed");
    assert_eq!(w.prompts(), ["prompt say:once"]);
    assert_eq!(w.magpie.faults().hits(), 0, "no blob was read");
    assert_eq!(
        std::fs::read(w.ws.join("uploads/a.pdf")).unwrap(),
        b"edited by the agent"
    );
    assert!(!w.dir.join(format!("gens/{n}/stdin-{seq}.ndjson")).exists());
    w.magpie.faults().remove(fault);
    w.idle(&second).await;
}

/// A message that cannot be kept for a replay fails its turn before anything is written: a `written` record always
/// has the bytes a replay sends again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_that_cannot_be_kept_fails_its_turn_unwritten() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let d = sup.driver.clone();
    until("initialised", || {
        lock(&d.st).live.as_ref().is_some_and(|g| g.initialized)
    })
    .await;
    let n = lock(&sup.driver.st).live.as_ref().unwrap().n;
    // the kept file's temporary name is taken by a directory: the write fails (as root too)
    let blocked: Vec<PathBuf> = (1..=64)
        .map(|s| w.dir.join(format!("gens/{n}/stdin-{s}.tmp")))
        .collect();
    for b in &blocked {
        std::fs::create_dir_all(b).unwrap();
    }
    let (t, seq) = w.turn("say:never");
    let end = w.ended(t).await;
    assert_eq!(end["state"], "failed");
    assert_eq!(end["error"]["code"], "run.error");
    assert!(
        end["error"]["message"].as_str().unwrap().contains("could not be kept"),
        "{end}"
    );
    assert!(w.acked(seq).await.error.is_some());
    assert!(w.prompts().is_empty(), "nothing written");
    // the record becomes `acked` once magpie answered the ack; it was never `written` (or `failed`) meanwhile
    let d = sup.driver.clone();
    until("no written record, acked", || {
        lock(&d.st).delivered.get(&seq).is_none_or(|d| d.state == "acked")
    })
    .await;
    for i in 1..=64 {
        let kept = w.dir.join(format!("gens/{n}/stdin-{i}.ndjson"));
        assert!(!kept.exists(), "{kept:?}");
    }
    for b in &blocked {
        std::fs::remove_dir(b).unwrap();
    }
    w.idle(&sup).await;
    let (t, _) = w.turn("say:kept");
    assert_eq!(w.ended(t).await["state"], "completed");
}

/// A kept message whose rename fails leaves no temporary file behind (only its generation's pruning would remove it).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_that_cannot_be_renamed_leaves_no_temporary_file() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let d = sup.driver.clone();
    until("initialised", || {
        lock(&d.st).live.as_ref().is_some_and(|g| g.initialized)
    })
    .await;
    let n = lock(&sup.driver.st).live.as_ref().unwrap().n;
    // the kept file's own name is taken by a directory: the write succeeds, the rename fails
    let blocked: Vec<PathBuf> = (1..=64)
        .map(|s| w.dir.join(format!("gens/{n}/stdin-{s}.ndjson/x")))
        .collect();
    for b in &blocked {
        std::fs::create_dir_all(b).unwrap();
    }
    let (t, seq) = w.turn("say:never");
    let end = w.ended(t).await;
    assert_eq!(end["state"], "failed");
    assert!(
        end["error"]["message"].as_str().unwrap().contains("could not be kept"),
        "{end}"
    );
    assert!(w.acked(seq).await.error.is_some());
    assert!(w.prompts().is_empty(), "nothing written");
    let tmps: Vec<_> = std::fs::read_dir(w.dir.join(format!("gens/{n}")))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|f| f.extension().is_some_and(|x| x == "tmp"))
        .collect();
    assert!(tmps.is_empty(), "{tmps:?}");
    for b in &blocked {
        std::fs::remove_dir(b).unwrap();
        std::fs::remove_dir(b.parent().unwrap()).unwrap();
    }
    w.idle(&sup).await;
}

/// A restart after a message that was never handed over (the writer refused it, or the turn was killed before the
/// hand-over) but before magpie answered its negative ack: the replay sends that ack again, on the same live
/// generation, and writes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_after_a_failed_hand_over_acks_it_failed_again() {
    for failure in [Some("writer gone"), None] {
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
        first.driver.abort_all();
        first.persisted.flush().await;
        let (turn, seq) = w.turn("say:never");
        let id = msg_id("b00700000000000a", seq);
        // what the first supervisor left: the turn ended (its end reported), the record `failed`, the ack unanswered
        let mut prior = first.persisted.read(Clone::clone);
        prior.delivered.insert(
            seq.to_string(),
            Delivered {
                state: "failed".into(),
                msg_id: Some(hex(&id)),
                generation: Some(n),
                pending_inputs_after: Some(1),
                turn_id: Some(turn.to_string()),
                error: failure.map(str::to_owned),
            },
        );
        prior.cmd_after = seq;
        drop(first);
        let second = w.attach(Some(prior), crate::start::Reattach::Alive { n, pid }).await;
        let ack = w.acked(seq).await;
        if let Some(e) = failure {
            assert_eq!(ack.error.as_deref(), Some(e));
        } else {
            assert_eq!(ack.delivered, Some(false));
            assert!(ack.error.is_none());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(w.prompts().is_empty(), "nothing written: {}", texts(&w));
        assert!(
            w.state_names(turn).is_empty(),
            "no turn started: {:?}",
            w.state_names(turn)
        );
        let d = second.driver.clone();
        until("acked", || {
            lock(&d.st).delivered.get(&seq).is_some_and(|r| r.state == "acked")
        })
        .await;
        w.idle(&second).await;
        assert_eq!(w.spawns().len(), 1, "re-attached, not respawned");
    }
}

/// A replayed turn whose interrupt was already sent gets no second one; a turn interrupted before its message was
/// handed over gets its interrupt once, right after the message.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replayed_interrupted_turn_is_interrupted_exactly_once() {
    for handed in [true, false] {
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
        first.driver.abort_all();
        first.persisted.flush().await;
        let (turn, seq) = w.turn("deaf");
        let boot_id = "b00700000000000a";
        let id = msg_id(boot_id, seq);
        let line = prompt::user_line(
            &prompt::content(prompt::text("deaf", &[], &[]), &[]),
            &magpie_api::Timestamp::now().to_wire(),
        );
        std::fs::write(w.dir.join(format!("gens/{n}/stdin-{seq}.ndjson")), &line).unwrap();
        if handed {
            // the first supervisor handed the message over and wrote the interrupt after it
            let client = w.keeper.client.clone();
            let message = line.clone();
            tokio::task::spawn_blocking(move || {
                client.stdin(pid, id, &message)?;
                client.stdin(pid, keeper::new_msg_id(), &control::interrupt("intr-first"))
            })
            .await
            .unwrap()
            .unwrap();
            until("interrupt read", || {
                w.fake_log().iter().any(|l| l == "control interrupt")
            })
            .await;
        }
        let mut prior = first.persisted.read(Clone::clone);
        prior.delivered.insert(
            seq.to_string(),
            Delivered {
                state: "written".into(),
                msg_id: Some(hex(&id)),
                generation: Some(n),
                pending_inputs_after: Some(1),
                turn_id: Some(turn.to_string()),
                error: None,
            },
        );
        prior.cmd_after = seq;
        prior.run.state = "running".into();
        prior.run.turn_id = Some(turn.to_string());
        prior.run.origin = Some("person".into());
        prior.run.pending_inputs = 1;
        prior.run.interrupt_sent_at = Some(magpie_api::Timestamp::now().to_wire());
        prior.run.interrupt_elapsed_ms = Some(0);
        prior.run.handed = Some(handed);
        drop(first);
        let second = w.attach(Some(prior), crate::start::Reattach::Alive { n, pid }).await;
        assert_eq!(w.acked(seq).await.delivered, Some(true), "handed: {handed}");
        until("the prompt", || w.prompts() == ["prompt deaf"]).await;
        until("an interrupt", || w.fake_log().iter().any(|l| l == "control interrupt")).await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let interrupts = w.fake_log().iter().filter(|l| *l == "control interrupt").count();
        assert_eq!(interrupts, 1, "handed: {handed}\n{}", texts(&w));
        assert_eq!(w.prompts(), ["prompt deaf"], "the message once");
        w.magpie.push_command(
            w.cid,
            Some(turn),
            CommandBody::Kill(KillCommand {
                reason: KillReason::Stop,
            }),
        );
        assert_eq!(w.ended(turn).await["state"], "canceled");
        w.idle(&second).await;
    }
}

/// A restored ask keeps its `toolUseID` (the answer carries it) and its turn's lane.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restored_ask_is_answered_with_its_tool_use_id() {
    let w = world().await;
    let first = w.attach(None, crate::start::Reattach::Nothing).await;
    let (t, _) = w.turn("ask");
    until("the ask", || !w.magpie.script().calls_to("ask_open").is_empty()).await;
    let ask = w.magpie.script().calls_to("ask_open")[0].body.clone().unwrap()["askId"]
        .as_str()
        .unwrap()
        .to_owned();
    first.persisted.flush().await;
    let prior = first.persisted.read(Clone::clone);
    assert_eq!(prior.asks[0].tool_use_id.as_deref(), Some("toolu_ask"));
    let (n, pid) = {
        let st = lock(&first.driver.st);
        let g = st.live.as_ref().unwrap();
        (g.n, g.pid)
    };
    first.driver.abort_all();
    drop(first);
    let second = w.attach(Some(prior), crate::start::Reattach::Alive { n, pid }).await;
    let a = w.magpie.push_command(
        w.cid,
        Some(t),
        CommandBody::AnswerAsk(AnswerAsk {
            ask_id: ask,
            allow: true,
            updated_input: Some(serde_json::Map::new()),
            message: None,
        }),
    );
    assert!(w.acked(a).await.error.is_none());
    assert_eq!(w.ended(t).await["state"], "completed");
    assert!(w.fake_log().iter().any(|l| l == "answer toolu_ask"), "{}", texts(&w));
    w.idle(&second).await;
}

/// 6.4: a configuration changed while no supervisor ran is applied at the restarted supervisor's first idle
/// point: the generation's own etag is kept in run.json, not taken from the files just written.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_configuration_changed_across_a_restart_ends_the_generation_when_idle() {
    let w = world().await;
    let first = w.attach(None, crate::start::Reattach::Nothing).await;
    let d = first.driver.clone();
    until("initialised", || {
        lock(&d.st).live.as_ref().is_some_and(|g| g.initialized)
    })
    .await;
    first.persisted.flush().await;
    let prior = first.persisted.read(Clone::clone);
    let (n, pid) = {
        let st = lock(&first.driver.st);
        let g = st.live.as_ref().unwrap();
        (g.n, g.pid)
    };
    assert_ne!(prior.managed_etag, "");
    assert_eq!(prior.generation.as_ref().unwrap().etag, prior.managed_etag);
    first.driver.abort_all();
    drop(first);
    // unchanged: the generation lives on
    let second = w
        .attach(Some(prior.clone()), crate::start::Reattach::Alive { n, pid })
        .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(second.driver.claude_pid(), Some(pid));
    second.persisted.flush().await;
    second.driver.abort_all();
    drop(second);
    // started under an older CLAUDE.md: ended at the idle point
    let mut stale = prior;
    stale.managed_etag = "an-older-etag".into();
    stale.generation.as_mut().unwrap().etag = "an-older-etag".into();
    let third = w.attach(Some(stale), crate::start::Reattach::Alive { n, pid }).await;
    let rec = tokio::task::block_in_place(|| w.keeper.wait_exit(pid));
    assert_eq!(rec.pid, pid);
    let (t, _) = w.turn_with("say:x", Some(w.cid.to_string()), vec![], vec![]);
    assert_eq!(w.ended(t).await["state"], "completed");
    assert!(third.driver.claude_pid().is_some_and(|p| p != pid));
}

/// The `SendUserFile` hook: a file over 64 MiB is refused by its size before it is read, and a send that meets a
/// migration (`409 conversation_migrating`) is retried within the budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sent_files_refuse_oversized_files_at_once_and_wait_out_a_migration() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let local = Arc::new(crate::local::Local {
        hermit: sup.driver.hermit.clone(),
        agent: sup.driver.clone(),
    });
    let sock = w.dir.join("agent.sock");
    let _srv = crate::sent_files::serve_with_fds(&sock, 0o666, supervisor_api::server::local_router(local)).unwrap();
    std::fs::create_dir_all(w.ws.join("out")).unwrap();
    // sparse: 4 GiB that would take minutes to hash
    let big = std::fs::File::create(w.ws.join("out/huge.bin")).unwrap();
    big.set_len(4 << 30).unwrap();
    drop(big);
    std::fs::write(w.ws.join("out/small.txt"), b"hello").unwrap();
    let paths = crate::sent_files::HookPaths {
        root: w.ws.clone(),
        socket: sock.clone(),
    };
    let hook = |input: Value| {
        let p = paths.clone();
        tokio::task::spawn_blocking(move || crate::sent_files::hook(&p, &input))
    };
    let started = Instant::now();
    let r = hook(json!({"tool_name": "SendUserFile", "tool_use_id": "toolu_big",
                        "tool_input": {"files": ["out/huge.bin"]}}))
    .await
    .unwrap();
    assert!(
        matches!(&r, crate::sent_files::HookOutcome::Failed(m) if m.contains("over 64 MiB")),
        "{r:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    w.magpie.script().once(
        "sent_files",
        aviary_fakes::script::Reply::error(409, "conversation_migrating", "moving").with_header("retry-after", "1"),
    );
    let r = hook(json!({"tool_name": "SendUserFile", "tool_use_id": "toolu_small",
                        "tool_input": {"files": ["out/small.txt"]}}))
    .await
    .unwrap();
    assert_eq!(r, crate::sent_files::HookOutcome::Sent(1));
    assert_eq!(w.magpie.script().calls_to("sent_files").len(), 2, "retried once");
}

#[test]
fn an_unanswered_ask_is_closed_with_the_timeout_answer() {
    // a long safety limit: a question that waits never holds a sandbox (DECISIONS 17)
    assert_eq!(
        super::ask_timeout_answer(),
        "User has not answered in 7 days, the session has timed out, ask again if needed."
    );
    assert_eq!(super::span(Duration::from_mins(30)), "30 minutes");
    assert_eq!(super::span(Duration::from_hours(1)), "1 hour");
}

// ---- DECISIONS 17: a question nobody answers never holds a sandbox ----

fn idle_release() -> supervisor_api::control::ReleaseRequest {
    supervisor_api::control::ReleaseRequest {
        reason: hermit_api::sandbox::ReleaseReason::Idle,
        code: hermit_api::sandbox::ReleaseCallCode::SandboxReleased,
        force: false,
        deadline_ms: 30_000,
    }
}

async fn asked(w: &World) -> String {
    until("the ask", || !w.magpie.script().calls_to("ask_open").is_empty()).await;
    w.magpie.script().calls_to("ask_open")[0].body.clone().unwrap()["askId"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn state_seen(w: &World, turn: Uuid, state: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        if let Some(b) = w.states(turn).into_iter().find(|b| b["state"] == state) {
            return b;
        }
        assert!(
            Instant::now() < deadline,
            "turn {turn} not {state}: {:?}\nfake:\n{}",
            w.state_names(turn),
            texts(w)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The release of a run that waits only on its question parks the turn (reported `parked`, never ended, the ask
/// never resolved), ends Claude Code without closing its stdin (which would make it fail the question and call the
/// model), and the person's answer later resumes the same turn in a new sandbox: `--resume`, the answer as the
/// turn's next message, worded as `AskUserQuestion`'s own result, and the turn completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_question_nobody_answers_is_parked_and_its_answer_resumes_the_turn_in_a_new_sandbox() {
    let w = world().await;
    let first = w.attach(None, crate::start::Reattach::Nothing).await;
    let (t, _) = w.turn("ask;say:after");
    let ask = asked(&w).await;
    let d = first.driver.clone();
    until("asking", || d.status().borrow().state == ActivityState::Asking).await;
    let s = first.driver.status().borrow().clone();
    assert!(!s.busy() && s.busy_for_pause(), "{s:?}");
    assert!(s.idle_since.is_some(), "idle for hermit from the question on");
    assert_eq!(s.turn_id, Some(t));
    assert_eq!(
        first.lifecycle.release(idle_release()).await,
        supervisor_api::control::Outcome::Done
    );
    let parked = state_seen(&w, t, "parked").await;
    assert!(parked["durationMs"].is_u64(), "{parked}");
    assert_eq!(w.state_names(t), vec!["running", "parked"], "neither ended nor failed");
    assert!(
        w.magpie.script().calls_to("ask_resolved").is_empty(),
        "the ask stays open in magpie"
    );
    let log = texts(&w);
    assert!(!log.contains("eof"), "stdin was closed: {log}");
    drop(first);

    // the person answers: magpie gets a new sandbox (a fresh local disk, the same /workspace), whose supervisor
    // resumes the conversation
    let _ = std::fs::remove_dir_all(w.dir.join("gens"));
    let _ = std::fs::remove_dir_all(w.dir.join("state"));
    let second = w.attach(None, crate::start::Reattach::Nothing).await;
    let mut given = serde_json::Map::new();
    given.insert("questions".into(), json!([{"question": "Which?"}]));
    given.insert("answers".into(), json!({"Which?": "this one"}));
    let input = given.clone();
    w.magpie.push_command(
        w.cid,
        Some(t),
        CommandBody::StartTurn(StartTurn {
            harness: magpie_api::Harness::default(),
            carry_over: None,
            text: String::new(),
            files: vec![],
            images: vec![],
            resume: Resume {
                provider_session_id: Some(w.cid.to_string()),
            },
            sent_at: magpie_api::Timestamp::now(),
            answers: vec![magpie_api::sandbox::ParkedAnswer {
                ask_id: ask,
                tool_name: "AskUserQuestion".into(),
                tool_use_id: Some("toolu_ask".into()),
                input,
                allow: true,
                updated_input: Some(given),
                message: None,
            }],
            model: None,
            thinking: None,
        }),
    );
    assert_eq!(state_seen(&w, t, "completed").await["state"], "completed");
    assert_eq!(w.state_names(t), vec!["running", "parked", "running", "completed"]);
    let spawns = w.spawns();
    assert!(
        spawns.last().unwrap().contains(&format!("--resume {}", w.cid)),
        "{spawns:?}"
    );
    let prompts = w.prompts();
    assert!(
        prompts
            .last()
            .unwrap()
            .starts_with("prompt The user answered: \"Which?\"=\"this one\"."),
        "{prompts:?}"
    );
    w.idle(&second).await;
}

/// A question beside a tool call that still runs, or beside a background task, is not a wait on the person alone:
/// the run stays busy and an unforced release is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_question_beside_a_running_tool_or_a_background_task_keeps_its_sandbox() {
    for script in ["askbash", "bg:t1;ask"] {
        let w = world().await;
        let sup = w.attach(None, crate::start::Reattach::Nothing).await;
        let (t, _) = w.turn(script);
        asked(&w).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let s = sup.driver.status().borrow().clone();
        assert_eq!(s.state, ActivityState::Running, "{script}: {s:?}");
        assert!(s.busy(), "{script}");
        assert!(
            matches!(
                sup.lifecycle.release(idle_release()).await,
                supervisor_api::control::Outcome::Busy(_)
            ),
            "{script}"
        );
        assert_eq!(w.state_names(t), vec!["running"], "{script}");
        assert_eq!(sup.lifecycle.phase(), crate::lifecycle::Phase::Serving);
    }
}

/// The time a question waits is no working time: the turn's reported duration leaves it out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_time_a_question_waits_is_not_the_turns_working_time() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let started = Instant::now();
    let (t, _) = w.turn("ask;say:after");
    let ask = asked(&w).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let a = w.magpie.push_command(
        w.cid,
        None,
        CommandBody::AnswerAsk(AnswerAsk {
            ask_id: ask,
            allow: true,
            updated_input: Some(serde_json::Map::new()),
            message: None,
        }),
    );
    w.acked(a).await;
    let end = w.ended(t).await;
    let wall = started.elapsed();
    let worked = Duration::from_millis(end["durationMs"].as_u64().unwrap());
    assert!(wall >= Duration::from_secs(3));
    assert!(
        worked + Duration::from_millis(2_500) < wall,
        "worked {worked:?} of {wall:?}"
    );
    w.idle(&sup).await;
}

fn open_ask(st: &mut St, turn: Uuid) {
    st.asks.push(Ask {
        ask_id: "a1".into(),
        cli_request_id: "ask-1".into(),
        tool_name: "AskUserQuestion".into(),
        tool_use_id: Some("toolu_ask".into()),
        since: now(),
        since_mono: Instant::now(),
        lane: Some(turn),
    });
    st.sync_asks();
}

/// The silence ceiling and the turn's working time stand still while a question is open and go on after it.
#[tokio::test(start_paused = true)]
async fn no_turn_timer_runs_while_a_question_is_open() {
    let mut st = St::new();
    let id = Uuid::now_v7();
    st.turn = Some(Turn::new(id, TurnOrigin::Person));
    st.run = RunKind::Running;
    st.last_output_mono = Instant::now();
    tokio::time::advance(Duration::from_mins(10)).await;
    open_ask(&mut st, id);
    tokio::time::advance(Duration::from_hours(2)).await;
    let turn = st.turn.clone().unwrap();
    assert_eq!(
        st.worked(&turn),
        Duration::from_mins(10),
        "the open question's wait is not counted"
    );
    st.asks.clear();
    st.sync_asks();
    tokio::time::advance(Duration::from_mins(1)).await;
    assert_eq!(
        Instant::now().duration_since(st.last_output_mono),
        Duration::from_mins(11)
    );
    let turn = st.turn.clone().unwrap();
    assert_eq!(st.worked(&turn), Duration::from_mins(11));
    assert_eq!(turn.ask_wait, Duration::from_hours(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_sent_into_a_running_turn_is_written_at_once() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let (t, _) = w.turn("ask;say:after");
    let calls = w.magpie.clone();
    until("the ask", || !calls.script().calls_to("ask_open").is_empty()).await;
    // for a turn that is not the running one: not delivered (magpie sends it as a new turn)
    let other = w.magpie.push_command(
        w.cid,
        Some(Uuid::now_v7()),
        CommandBody::SendMessage(SendMessage {
            text: "say:nope".into(),
        }),
    );
    assert_eq!(w.acked(other).await.delivered, Some(false));
    // for the running turn: written to Claude Code at once, while the turn still waits
    let sent = w.magpie.push_command(
        w.cid,
        Some(t),
        CommandBody::SendMessage(SendMessage {
            text: "say:scheduled".into(),
        }),
    );
    assert_eq!(w.acked(sent).await.delivered, Some(true));
    until("the message on Claude Code's stdin", || {
        w.fake_log().iter().filter(|l| l.as_str() == "stdin user").count() == 2
    })
    .await;
    assert!(
        w.states(t).iter().all(|s| s["state"] == "running"),
        "the turn still runs"
    );
    // this fake leaves it for after the turn's result, as Claude Code does when the model stops before a tool
    // result: the turn ends with its own result, and the message is answered in a run of its own (an agent turn)
    let id = calls.script().calls_to("ask_open")[0].body.clone().unwrap()["askId"]
        .as_str()
        .unwrap()
        .to_owned();
    let a = w.magpie.push_command(
        w.cid,
        None,
        CommandBody::AnswerAsk(AnswerAsk {
            ask_id: id,
            allow: true,
            updated_input: None,
            message: None,
        }),
    );
    w.acked(a).await;
    assert_eq!(w.ended(t).await["state"], "completed");
    until("the message's own run", || {
        w.prompts().iter().any(|p| p == "prompt say:scheduled")
    })
    .await;
    until("an agent turn", || !calls.script().calls_to("agent_turn").is_empty()).await;
    w.idle(&sup).await;
    assert!(!w.prompts().iter().any(|p| p == "prompt say:nope"));
}

// ---- compaction by images ----

fn agent_turns(w: &World) -> Vec<Uuid> {
    w.magpie
        .script()
        .calls_to("agent_turn")
        .iter()
        .map(|c| serde_json::from_value(c.body.as_ref().unwrap()["turnId"].clone()).unwrap())
        .collect()
}

fn compactions(w: &World) -> usize {
    w.fake_log().iter().filter(|l| *l == "prompt /compact").count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_conversation_is_compacted_at_a_turns_end_once_it_holds_the_configured_images() {
    let w = world_compacting(3).await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    // two images: under the limit, the turn's end is an ordinary idle point
    let (t, _) = w.turn("img:2");
    assert_eq!(w.ended(t).await["state"], "completed");
    w.idle(&sup).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!((compactions(&w), agent_turns(&w).len()), (0, 0));
    assert_eq!(sup.persisted.read(|s| s.run.images), Some(2));
    // the third: at the turn's end Claude Code is told to compact, as a turn of origin agent that ends at the
    // compaction's result
    let (t, _) = w.turn("img:1");
    assert_eq!(w.ended(t).await["state"], "completed");
    until("the compaction's turn", || agent_turns(&w).len() == 1).await;
    assert_eq!(w.ended(agent_turns(&w)[0]).await["state"], "completed");
    w.idle(&sup).await;
    assert_eq!(compactions(&w), 1);
    assert_eq!(
        sup.persisted.read(|s| s.run.images),
        Some(0),
        "the compaction's boundary starts the count again"
    );
    // the count starts again, and a person's message during a compaction is answered after it
    let (t, _) = w.turn("img:2");
    assert_eq!(w.ended(t).await["state"], "completed");
    w.idle(&sup).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(compactions(&w), 1);
    let (t, _) = w.turn("img:4");
    assert_eq!(w.ended(t).await["state"], "completed");
    until("the second compaction", || compactions(&w) == 2).await;
    let (p, _) = w.turn("say:after the compaction");
    assert_eq!(w.ended(agent_turns(&w)[1]).await["state"], "completed");
    assert_eq!(w.ended(p).await["state"], "completed");
    assert_eq!(w.state_names(p), ["running", "completed"]);
    w.idle(&sup).await;
    assert_eq!(
        w.prompts().iter().rev().take(2).rev().collect::<Vec<_>>(),
        ["prompt /compact", "prompt say:after the compaction"]
    );
    assert_eq!(compactions(&w), 2);
    assert_eq!(agent_turns(&w).len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_the_setting_images_never_compact_and_a_prompts_own_images_count() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let (t, _) = w.turn("img:50");
    assert_eq!(w.ended(t).await["state"], "completed");
    w.idle(&sup).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!((compactions(&w), agent_turns(&w).len()), (0, 0));
    assert_eq!(sup.persisted.read(|s| s.run.images), Some(50));
    drop(sup);

    let w = world_compacting(2).await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let sha = w.magpie.seed_blob(b"\x89PNG\r\n\x1a\nimage");
    let image = |n: u32| StartImage {
        upload_id: Uuid::new_v4(),
        mime_type: "image/png".into(),
        workspace_path: format!("uploads/{n}.png"),
        blob: sha.clone(),
        model_blob: sha.clone(),
        model_mime: "image/png".into(),
    };
    let (t, _) = w.turn_with("say:two", None, vec![], vec![image(1), image(2)]);
    assert_eq!(w.ended(t).await["state"], "completed");
    until("the compaction", || compactions(&w) == 1).await;
    assert_eq!(w.ended(agent_turns(&w)[0]).await["state"], "completed");
    w.idle(&sup).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resumed_conversation_counts_the_images_its_transcript_holds() {
    let w = world_compacting(5).await;
    let session = Uuid::new_v4().to_string();
    let dir = w.dir.join("dot-claude/projects/-workspace");
    std::fs::create_dir_all(&dir).unwrap();
    let image = r#"{"type":"image","source":{"type":"base64","media_type":"image/png","data":"QUJD"}}"#;
    let result = |n: usize| {
        format!(
            r#"{{"type":"user","uuid":"u","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t","content":[{}]}}]}}}}"#,
            vec![image; n].join(",")
        )
    };
    // seven images before a compaction, three after it and a text that only quotes an image block
    let quoted = serde_json::to_string(&json!({"type": "user", "message": {"content": image}})).unwrap();
    let lines = [
        result(7),
        r#"{"type":"system","subtype":"compact_boundary","compactMetadata":{"trigger":"auto"}}"#.to_owned(),
        result(2),
        quoted,
        result(1),
    ];
    std::fs::write(dir.join(format!("{session}.jsonl")), lines.join("\n") + "\n").unwrap();
    assert_eq!(count_images(&dir.join(format!("{session}.jsonl"))).unwrap(), 3);

    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let (t, _) = w.turn_with("img:1", Some(session.clone()), vec![], vec![]);
    assert_eq!(w.ended(t).await["state"], "completed");
    w.idle(&sup).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        sup.persisted.read(|s| s.run.images),
        Some(4),
        "three in the transcript and the one just read"
    );
    assert_eq!(compactions(&w), 0);
    let (t, _) = w.turn_with("img:1", Some(session), vec![], vec![]);
    assert_eq!(w.ended(t).await["state"], "completed");
    until("the compaction at five", || compactions(&w) == 1).await;
    assert_eq!(w.ended(agent_turns(&w)[0]).await["state"], "completed");
    w.idle(&sup).await;
}

// ---- the conversation's thinking level ----

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_is_switched_to_the_conversations_thinking_level_before_the_turn() {
    let w = world().await;
    let sup = w.attach(None, crate::start::Reattach::Nothing).await;
    let levels = || -> Vec<String> {
        w.fake_log()
            .into_iter()
            .filter_map(|l| l.strip_prefix("thinking ").map(str::to_owned))
            .collect()
    };
    let turn = |text: &str, thinking: Option<&str>| {
        let id = Uuid::now_v7();
        w.magpie.push_command(
            w.cid,
            Some(id),
            CommandBody::StartTurn(StartTurn {
                harness: magpie_api::Harness::default(),
                carry_over: None,
                text: text.into(),
                files: vec![],
                images: vec![],
                resume: Resume {
                    provider_session_id: None,
                },
                sent_at: magpie_api::Timestamp::now(),
                answers: Vec::new(),
                model: None,
                thinking: thinking.map(str::to_owned),
            }),
        );
        id
    };
    // the mock model's fixture: the level it names for its default model
    let models: gateway_api::models::Models = serde_json::from_str(mock_model::DEFAULT_MODELS).unwrap();
    let own = models
        .models
        .iter()
        .find(|m| m.id == models.default)
        .and_then(|m| m.reasoning.clone())
        .unwrap();
    let effort = |level: &str| format!("apply_flag_settings {{\"effortLevel\":\"{level}\"}}");
    // no level picked: the one the models route names for the model, once, before the prompt
    let t = turn("say:one", None);
    assert_eq!(w.ended(t).await["state"], "completed");
    assert_eq!(levels(), ["set_max_thinking_tokens null".to_owned(), effort(&own)]);
    let log = w.fake_log();
    let at = |needle: &str| log.iter().position(|l| l.contains(needle)).unwrap();
    assert!(
        at("thinking apply_flag_settings") < at("prompt say:one"),
        "switched before the prompt"
    );
    // the same again: nothing is sent
    let t = turn("say:two", None);
    assert_eq!(w.ended(t).await["state"], "completed");
    assert_eq!(levels().len(), 2);
    // a picked level; `none` is thinking off; back to a level turns it on again
    let t = turn("say:three", Some("max"));
    assert_eq!(w.ended(t).await["state"], "completed");
    assert_eq!(
        levels()[2..],
        ["set_max_thinking_tokens null".to_owned(), effort("max")]
    );
    let t = turn("say:four", Some("none"));
    assert_eq!(w.ended(t).await["state"], "completed");
    assert_eq!(levels()[4..], ["set_max_thinking_tokens 0"]);
    let t = turn("say:five", Some("low"));
    assert_eq!(w.ended(t).await["state"], "completed");
    assert_eq!(
        levels()[5..],
        ["set_max_thinking_tokens null".to_owned(), effort("low")]
    );
    w.idle(&sup).await;
}
