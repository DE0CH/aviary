//! The persisted run state `/var/lib/supervisor/state/run.json` (supervisor 4.4): what the supervisor must not
//! forget across its own restart. Root 0600, replaced atomically (write, fsync, rename, fsync the directory),
//! written at most every 100 ms and at once on a state change. `snake_case` like every on-disk format; the wire is
//! camelCase. Everything else is rebuilt from magpie (lease, tree, commands) or from Claude Code's stdout file.

use std::collections::BTreeMap;
use std::io::{self, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, watch};

use crate::limits::RUN_STATE_THROTTLE;

/// The state directory on the scratch disk.
pub const STATE_DIR: &str = "/var/lib/supervisor/state";
/// Generation files: `/var/lib/supervisor/run/<n>/{stdout.ndjson, stderr.log}`.
pub const GEN_DIR: &str = "/var/lib/supervisor/run";
/// The file's version.
pub const VERSION: u32 = 1;

/// `run.json`. Unknown fields are ignored and missing ones default, so a newer supervisor's file is still read.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunState {
    pub version: u32,
    pub boot_id: String,
    pub lease_epoch: Option<u64>,
    /// The command poll resumes after this `cmdSeq`.
    pub cmd_after: u64,
    pub msg_seq: u64,
    #[serde(rename = "gen")]
    pub generation: Option<GenState>,
    pub run: RunPart,
    pub last_turn: Option<LastTurn>,
    pub tasks: Vec<TaskState>,
    pub asks: Vec<AskState>,
    /// Per `cmdSeq`: how far its delivery got (10.2).
    pub delivered: BTreeMap<String, Delivered>,
    /// Turns that ended and whose end report magpie has not answered yet: the heartbeat still names them, and a
    /// restarted supervisor sends their end again.
    pub ending: Vec<EndingState>,
    /// The descriptor `etag` the managed files on disk were written from.
    pub managed_etag: String,
}

/// The live Claude Code generation (4.5).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GenState {
    pub n: u32,
    pub pid: i32,
    pub started_at: String,
    /// The harness it is (absent in a `run.json` written before harnesses: Claude Code).
    pub harness: aviary_core::harness::Harness,
    pub session_id: Option<String>,
    pub stdout: String,
    pub stdout_offset: u64,
    pub initialized: bool,
    /// The descriptor `etag` of the managed files it started with (an idle point ends it when they changed, 6.4).
    pub etag: String,
    /// The model it runs, and whether the conversation's person picked it (`start_turn.model`) rather than the
    /// models route's default: a turn that wants another model ends it first.
    pub model: String,
    pub picked: bool,
}

/// The run state machine's persisted part (7.5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunPart {
    /// `idle`, `starting`, `running`, `stopping`, `held` (the wire's `ActivityState`).
    pub state: String,
    pub turn_id: Option<String>,
    pub origin: Option<String>,
    pub pending_inputs: u32,
    pub interrupt_sent_at: Option<String>,
    /// How long ago the interrupt was sent, on the monotonic clock (a restart resumes the 15 s grace from here).
    pub interrupt_elapsed_ms: Option<u64>,
    pub pending_uuids: Vec<String>,
    pub last_uuid: Option<String>,
    /// A person's turn written while an agent turn ran (7.5).
    pub queued_turn_id: Option<String>,
    /// Whether the turn's message is with the writer (absent: it is).
    pub handed: Option<bool>,
    /// The queued turn's interrupt, as the turn's, and whether its message is with the writer.
    pub queued_interrupt_elapsed_ms: Option<u64>,
    pub queued_handed: Option<bool>,
    /// The main thread's tool calls without a result yet (an `asking` run's open calls are all its questions).
    pub open_tools: Vec<String>,
    /// The images Claude Code's context holds since its last compaction (absent: not counted yet, the next
    /// generation counts them from the transcript).
    pub images: Option<u32>,
}

impl Default for RunPart {
    fn default() -> Self {
        Self {
            state: "idle".into(),
            turn_id: None,
            origin: None,
            pending_inputs: 0,
            interrupt_sent_at: None,
            interrupt_elapsed_ms: None,
            pending_uuids: Vec::new(),
            last_uuid: None,
            queued_turn_id: None,
            handed: None,
            open_tools: Vec::new(),
            images: None,
            queued_interrupt_elapsed_ms: None,
            queued_handed: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LastTurn {
    pub id: String,
    pub outcome: String,
    pub error: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskState {
    pub task_id: String,
    pub tool_use_id: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub description: String,
    pub started_at: String,
    /// `event` or `static`.
    pub source: String,
    pub until: Option<String>,
    /// Started inside a subagent or workflow.
    pub nested: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AskState {
    pub ask_id: String,
    pub cli_request_id: String,
    pub tool_name: String,
    pub since: String,
    pub tool_use_id: Option<String>,
    /// How long it has been open, on the monotonic clock (its 30 min timeout goes on from here after a restart).
    pub elapsed_ms: Option<u64>,
    /// The turn it was opened in (its reports' lane).
    pub lane: Option<String>,
}

/// A turn whose end report is not answered yet.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EndingState {
    pub turn_id: String,
    /// The end report (the wire's `TurnStateReport`).
    pub report: serde_json::Value,
    /// The uuids it waits for (8.2), with the generation that wrote them; `None`: it waits for the writes in
    /// flight only.
    pub uuids: Option<Vec<String>>,
    #[serde(rename = "gen")]
    pub generation: u32,
    /// It was interrupted (the heartbeat says `stopping`).
    pub stopping: bool,
    /// The heartbeat's `lastTurn` once magpie answered.
    pub last: LastTurn,
}

/// How far a command's delivery got: `received` (before any effect), `written` (with the `msg_id` and the
/// `pending_inputs` it implies, before the user message goes to the keeper), `running` (reported), `failed` (the
/// message was never handed over and the turn's end is reported; its negative ack is not answered yet) or `acked`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Delivered {
    pub state: String,
    pub msg_id: Option<String>,
    #[serde(rename = "gen")]
    pub generation: Option<u32>,
    pub pending_inputs_after: Option<u32>,
    /// The turn a `start_turn` command started (its person's turn is installed with the `written` record).
    pub turn_id: Option<String>,
    /// A `failed` record's ack error; none: the ack is `delivered: false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The file `run.json` in a state directory.
#[derive(Clone, Debug)]
pub struct StateFile {
    dir: PathBuf,
}

impl StateFile {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.dir.join("run.json")
    }

    /// The saved state, `None` on a first start. A file that does not parse is moved aside and treated as absent
    /// (logged): a restart then behaves like a first start rather than failing for ever.
    ///
    /// # Errors
    /// When the file exists but cannot be read.
    pub fn load(&self) -> io::Result<Option<RunState>> {
        let path = self.path();
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        match serde_json::from_slice::<RunState>(&bytes) {
            Ok(s) => Ok(Some(s)),
            Err(e) => {
                tracing::warn!(error = %e, "run state unreadable, moved aside");
                let _ = std::fs::rename(&path, self.dir.join("run.json.bad"));
                Ok(None)
            }
        }
    }

    /// Replaces the file atomically.
    ///
    /// # Errors
    /// Any write, sync or rename error.
    pub fn save(&self, s: &RunState) -> io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700));
        let tmp = self.dir.join("run.json.tmp");
        let body = serde_json::to_vec(s).map_err(io::Error::other)?;
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(&body)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, self.path())?;
        sync_dir(&self.dir)
    }
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

/// How urgently a change must reach the disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// A state change (a turn started, a command's delivery moved on): written at once.
    State,
    /// Progress (the stdout offset): written within [`RUN_STATE_THROTTLE`].
    Progress,
}

/// The run state in memory and its writer: [`Persisted::update`] changes it, [`Persisted::writer`] (a task) puts it
/// on disk.
#[derive(Debug)]
pub struct Persisted {
    state: Mutex<RunState>,
    /// (changes made, the last urgent change's number)
    changes: watch::Sender<(u64, u64)>,
    written: watch::Sender<u64>,
    wake: Notify,
}

impl Persisted {
    #[must_use]
    pub fn new(state: RunState) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(state),
            changes: watch::channel((0, 0)).0,
            written: watch::channel(0).0,
            wake: Notify::new(),
        })
    }

    /// Reads the state.
    pub fn read<R>(&self, f: impl FnOnce(&RunState) -> R) -> R {
        f(&self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Changes the state; the writer puts it on disk.
    pub fn update<R>(&self, change: Change, f: impl FnOnce(&mut RunState) -> R) -> R {
        let r = f(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner));
        self.changes.send_modify(|(n, urgent)| {
            *n += 1;
            if change == Change::State {
                *urgent = *n;
            }
        });
        self.wake.notify_one();
        r
    }

    /// Waits until every change made so far is on disk.
    pub async fn flush(&self) {
        let target = self.changes.borrow().0;
        let mut rx = self.written.subscribe();
        let _ = rx.wait_for(|w| *w >= target).await;
    }

    /// The writer task: writes at once after a [`Change::State`], at most every [`RUN_STATE_THROTTLE`] otherwise.
    pub async fn writer(self: Arc<Self>, file: StateFile) {
        let mut last = tokio::time::Instant::now() - RUN_STATE_THROTTLE;
        let mut done = 0u64;
        loop {
            let (n, urgent) = *self.changes.borrow();
            if n == done {
                self.wake.notified().await;
                continue;
            }
            if urgent <= done {
                // only progress: wait out the throttle
                tokio::time::sleep_until(last + RUN_STATE_THROTTLE).await;
            }
            let (snapshot, n) = {
                let s = self.state.lock().unwrap_or_else(PoisonError::into_inner).clone();
                (s, self.changes.borrow().0)
            };
            let f = file.clone();
            let r = tokio::task::spawn_blocking(move || f.save(&snapshot)).await;
            match r {
                Ok(Ok(())) => {
                    done = n;
                    last = tokio::time::Instant::now();
                    self.written.send_replace(n);
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "run state write failed");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("runstate-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_spec_example_round_trips() {
        let example = r#"{"version": 1, "boot_id": "a1b2c3d4e5f60718", "lease_epoch": 7, "cmd_after": 45, "msg_seq": 12,
 "gen": {"n": 3, "pid": 812, "started_at": "2026-11-02T10:00:00Z", "session_id": "279aabf0-0000-4000-8000-000000000000", "stdout": "/var/lib/supervisor/run/3/stdout.ndjson",
         "stdout_offset": 18234411, "initialized": true},
 "run": {"state": "running", "turn_id": "0190f6c2-0000-7000-8000-000000000000", "origin": "person", "pending_inputs": 1,
         "interrupt_sent_at": null, "pending_uuids": ["u1"], "last_uuid": "u0"},
 "last_turn": {"id": "0190f6b9-0000-7000-8000-000000000000", "outcome": "completed", "error": null},
 "tasks": [{"task_id": "t1", "tool_use_id": "toolu_1", "type": "local_bash", "description": "build", "started_at": "2026-11-02T10:01:00Z",
            "source": "event", "until": null}],
 "asks": [{"ask_id": "a1", "cli_request_id": "r1", "tool_name": "AskUserQuestion", "since": "2026-11-02T10:02:00Z"}],
 "delivered": {"41": {"state": "acked"},
               "42": {"state": "written", "msg_id": "8f0c", "gen": 3, "pending_inputs_after": 1}},
 "a_newer_field": true}"#;
        let s: RunState = serde_json::from_str(example).unwrap();
        assert_eq!(s.generation.as_ref().unwrap().stdout_offset, 18_234_411);
        assert_eq!(s.delivered["42"].generation, Some(3));
        assert_eq!(s.tasks[0].kind, "local_bash");
        let back: RunState = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        let v = serde_json::to_value(&s).unwrap();
        assert!(v.get("gen").is_some() && v.get("generation").is_none());
    }

    #[test]
    fn save_is_atomic_0600_and_load_survives_garbage() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = dir();
        let f = StateFile::new(&d);
        assert_eq!(f.load().unwrap(), None);
        let s = RunState {
            version: VERSION,
            cmd_after: 9,
            ..RunState::default()
        };
        f.save(&s).unwrap();
        assert_eq!(f.load().unwrap(), Some(s));
        let mode = std::fs::metadata(f.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(!d.join("run.json.tmp").exists());
        std::fs::write(f.path(), b"{not json").unwrap();
        assert_eq!(f.load().unwrap(), None);
        assert!(d.join("run.json.bad").exists());
        let _ = std::fs::remove_dir_all(d);
    }

    #[tokio::test]
    async fn state_changes_are_written_at_once_progress_is_throttled() {
        let d = dir();
        let file = StateFile::new(&d);
        let p = Persisted::new(RunState::default());
        tokio::spawn(p.clone().writer(file.clone()));
        p.update(Change::State, |s| s.cmd_after = 1);
        tokio::time::timeout(Duration::from_millis(500), p.flush())
            .await
            .unwrap();
        assert_eq!(file.load().unwrap().unwrap().cmd_after, 1);
        // a burst of progress lands as one write, within the throttle
        for off in 0..1000 {
            p.update(Change::Progress, |s| s.msg_seq = off);
        }
        tokio::time::timeout(Duration::from_millis(500), p.flush())
            .await
            .unwrap();
        assert_eq!(file.load().unwrap().unwrap().msg_seq, 999);
        let _ = std::fs::remove_dir_all(d);
    }
}
