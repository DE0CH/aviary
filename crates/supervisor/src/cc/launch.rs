//! Launching Claude Code (supervisor 6.1, 6.2): the argv, the environment and the keeper's `SPAWN`.

use std::collections::BTreeMap;

use gateway_api::models::Models;

use crate::boot::{AGENT_HOME, BootEnv};
use crate::harness::Harness;
use crate::keeper::SpawnSpec;

/// Claude Code, root-owned, outside every user-writable `PATH` entry, executed by absolute path (A11.F13).
pub const CLAUDE_BIN: &str = "/opt/claude-code/bin/claude";
/// Claude Code's working directory.
pub const WORKSPACE: &str = "/workspace";
/// The agent's uid and gid.
pub const AGENT_UID: u32 = 1000;
pub const AGENT_GID: u32 = 1000;
/// Claude Code's `oom_score_adj` (5.2).
pub const CLAUDE_OOM_SCORE_ADJ: i32 = 100;
/// Plan mode is never offered to the model (Deyao, 2026-11-05). `AskUserQuestion` is: a question nobody answers parks the
/// turn and frees the sandbox (DECISIONS 17), so it no longer hangs a turn (A11.F11). Nor are Claude Code's own
/// timers (`CronCreate`, `CronList`, `CronDelete`, `ScheduleWakeup`; Deyao, 2026-11-06): they live in the Claude Code
/// process and vanish when an idle sandbox is released; the AI arms magpie's schedules through the managed skill.
pub const DISALLOWED_TOOLS: &str = "EnterPlanMode,ExitPlanMode,CronCreate,CronList,CronDelete,ScheduleWakeup";

/// Qwen Code, root-owned as Claude Code is, executed by absolute path.
pub const QWEN_BIN: &str = "/opt/qwen-code/bin/qwen";
/// Qwen Code's tools that are never offered to the model, for the reasons of [`DISALLOWED_TOOLS`]: its own timers
/// (they live in the process and vanish with an idle sandbox) and its git worktree and pull-request helpers (the
/// workspace is no repository).
pub const QWEN_EXCLUDED_TOOLS: &[&str] = &[
    "cron_create",
    "cron_list",
    "cron_delete",
    "loop_wakeup",
    "enter_worktree",
    "exit_worktree",
    "autofix_pr",
];
/// The key name Qwen Code's managed settings name for the model route (a placeholder value: credentials live
/// behind `model`, never in the sandbox).
pub const QWEN_KEY_ENV: &str = "AVIARY_MODEL_KEY";

/// Which session a generation runs, in which harness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// The harness.
    pub harness: Harness,
    /// `--resume <id>` (a conversation with history in this harness) or `--session-id <id>` (a new session; for a
    /// conversation's first, the id is the conversation's).
    pub resume: bool,
    /// The harness's session id.
    pub id: String,
}

impl Session {
    /// `--resume <provider session id>`.
    #[must_use]
    pub fn resume(harness: Harness, id: impl Into<String>) -> Self {
        Self {
            harness,
            resume: true,
            id: id.into(),
        }
    }

    /// `--session-id <id>`.
    #[must_use]
    pub fn new(harness: Harness, id: impl Into<String>) -> Self {
        Self {
            harness,
            resume: false,
            id: id.into(),
        }
    }

    /// The session id the generation's `system`/`init` must report.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// What a generation's argv is made from beside its session.
#[derive(Clone, Debug, Default)]
pub struct Start {
    /// The descriptor's `claude.projectSettings`: the workspace's own settings are loaded instead of none (Claude
    /// Code: `--setting-sources=project`).
    pub project_settings: bool,
    /// The managed instructions. Claude Code reads them from its managed `CLAUDE.md`; Qwen Code has no managed
    /// instructions file, so they are its `--append-system-prompt`.
    pub instructions: String,
    /// The model the generation runs. Claude Code takes it from `ANTHROPIC_MODEL`; Qwen Code from `--model`.
    pub model: String,
}

/// The argv of 6.1 (`bin` is the harness's binary in a sandbox).
#[must_use]
pub fn argv(bin: &str, session: &Session, start: &Start) -> Vec<String> {
    let own = |v: &[&str]| -> Vec<String> { v.iter().map(|s| (*s).to_owned()).collect() };
    let mut a = match session.harness {
        Harness::Claude => own(&[
            bin,
            "--output-format",
            "stream-json",
            "--verbose",
            // the model's answer as it comes (`stream_event`), for the live frames (8.3)
            "--include-partial-messages",
            "--input-format",
            "stream-json",
            "--permission-prompt-tool",
            "stdio",
            "--permission-mode",
            "bypassPermissions",
            if start.project_settings {
                "--setting-sources=project"
            } else {
                "--setting-sources="
            },
            "--disallowedTools",
            DISALLOWED_TOOLS,
        ]),
        Harness::Qwen => {
            let mut a = own(&[
                bin,
                "--output-format",
                "stream-json",
                "--include-partial-messages",
                "--input-format",
                "stream-json",
                "--approval-mode",
                "yolo",
            ]);
            a.extend(QWEN_EXCLUDED_TOOLS.iter().map(|t| format!("--exclude-tools={t}")));
            a.extend(["--model".to_owned(), start.model.clone()]);
            if !start.instructions.is_empty() {
                a.extend(["--append-system-prompt".to_owned(), start.instructions.clone()]);
            }
            a
        }
    };
    a.extend([
        if session.resume { "--resume" } else { "--session-id" }.to_owned(),
        session.id.clone(),
    ]);
    a
}

/// The fixed table of 6.2 for a harness (it wins over the descriptor's `env` on a clash).
#[must_use]
pub fn fixed_env(harness: Harness, models: &Models) -> Vec<(&'static str, String)> {
    if harness == Harness::Qwen {
        // the model, its route and its window are in the managed settings ([`super::managed::qwen_settings`])
        return vec![
            ("HOME", AGENT_HOME.to_owned()),
            ("NO_PROXY", "localhost,127.0.0.1,::1".to_owned()),
            ("no_proxy", "localhost,127.0.0.1,::1".to_owned()),
            (QWEN_KEY_ENV, "sandbox".to_owned()),
            ("QWEN_CODE_SUPPRESS_YOLO_WARNING", "1".to_owned()),
        ];
    }
    let mut v: Vec<(&'static str, String)> = vec![
        ("HOME", AGENT_HOME.to_owned()),
        ("NO_PROXY", "localhost,127.0.0.1,::1".to_owned()),
        ("no_proxy", "localhost,127.0.0.1,::1".to_owned()),
        ("ANTHROPIC_BASE_URL", "http://model".to_owned()),
        // a placeholder: credentials live behind `model`, never in the sandbox
        ("ANTHROPIC_AUTH_TOKEN", "sandbox".to_owned()),
        ("ANTHROPIC_MODEL", models.default.clone()),
        ("ANTHROPIC_SMALL_FAST_MODEL", models.small.clone()),
        ("ANTHROPIC_DEFAULT_HAIKU_MODEL", models.small.clone()),
        ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1".to_owned()),
        ("DISABLE_AUTOUPDATER", "1".to_owned()),
        ("DISABLE_TELEMETRY", "1".to_owned()),
        ("DISABLE_ERROR_REPORTING", "1".to_owned()),
        ("CLAUDE_CODE_ATTACHMENTS_BY_PATH", "1".to_owned()),
        ("CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS", "5000".to_owned()),
        ("BASH_DEFAULT_TIMEOUT_MS", "120000".to_owned()),
        ("BASH_MAX_TIMEOUT_MS", "600000".to_owned()),
        ("BASH_MAX_OUTPUT_LENGTH", "30000".to_owned()),
        ("MAX_MCP_OUTPUT_TOKENS", "25000".to_owned()),
        ("API_TIMEOUT_MS", "600000".to_owned()),
    ];
    // the window of the model this Claude Code runs (`models.default`: the conversation's picked model, else the
    // route's default): its own when the route names one for it
    let context = models.context_of(&models.default);
    if context > 0 {
        v.push(("CLAUDE_CODE_MAX_CONTEXT_TOKENS", context.to_string()));
    }
    v
}

/// The harness's environment: the image's basics, the descriptor's `env` unchanged, then [`fixed_env`].
/// `CLAUDE_CODE_ENTRYPOINT` is never set; nothing of the boot environment beyond `PATH`, `LANG` and `LC_ALL` passes.
#[must_use]
pub fn env(
    harness: Harness,
    boot: &BootEnv,
    descriptor_env: &BTreeMap<String, String>,
    models: &Models,
) -> Vec<String> {
    let fixed = fixed_env(harness, models);
    let mut e = boot.child_env(AGENT_HOME, descriptor_env, &fixed);
    e.retain(|kv| {
        let k = kv.split('=').next().unwrap_or("");
        k != "HERMIT_SANDBOX_TOKEN" && !k.starts_with("SUPERVISOR_") && k != "CLAUDE_CODE_ENTRYPOINT"
    });
    e
}

/// Where a generation runs and what it gets.
#[derive(Clone, Debug)]
pub struct Launch {
    /// Claude Code.
    pub bin: String,
    /// Qwen Code.
    pub qwen_bin: String,
    pub cwd: String,
    pub uid: u32,
    pub gid: u32,
}

impl Launch {
    /// The sandbox's values.
    #[must_use]
    pub fn sandbox() -> Self {
        Self {
            bin: CLAUDE_BIN.to_owned(),
            qwen_bin: QWEN_BIN.to_owned(),
            cwd: WORKSPACE.to_owned(),
            uid: AGENT_UID,
            gid: AGENT_GID,
        }
    }

    /// The keeper's `SPAWN` for generation files `stdout`/`stderr`.
    #[must_use]
    pub fn spec(
        &self,
        session: &Session,
        start: &Start,
        env: Vec<String>,
        stdout: String,
        stderr: String,
        cgroup: Option<String>,
    ) -> SpawnSpec {
        SpawnSpec {
            name: crate::harness::process_name(session.harness).into(),
            argv: argv(
                match session.harness {
                    Harness::Claude => &self.bin,
                    Harness::Qwen => &self.qwen_bin,
                },
                session,
                start,
            ),
            env,
            cwd: Some(self.cwd.clone()),
            uid: self.uid,
            gid: self.gid,
            setsid: true,
            oom_score_adj: CLAUDE_OOM_SCORE_ADJ,
            cgroup,
            stdout: Some(stdout),
            stderr: Some(stderr),
            stdin_pipe: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn models() -> Models {
        serde_json::from_str(include_str!("../../../gateway-api/fixtures/models.json")).unwrap()
    }

    /// A conversation on a model with its own context window (a 1M model picked beside 200k ones) runs Claude Code
    /// at that window; before, every model got the route's one number and a 1M model compacted at 200k.
    #[test]
    fn the_context_window_is_the_running_model_s() {
        let ctx = |m: &Models| {
            fixed_env(Harness::Claude, m)
                .into_iter()
                .find(|(k, _)| *k == "CLAUDE_CODE_MAX_CONTEXT_TOKENS")
                .map(|(_, v)| v)
        };
        let mut m = models();
        assert_eq!(ctx(&m), Some(m.context_tokens.to_string()));
        m.models[1].context_tokens = Some(1_000_000);
        assert_eq!(
            ctx(&m),
            Some(m.context_tokens.to_string()),
            "the default model names none"
        );
        // the conversation's picked model becomes `default` (run.rs on_picked_model)
        m.default.clone_from(&m.models[1].id);
        assert_eq!(ctx(&m).as_deref(), Some("1000000"));
        // a route that names no window, a model that does: only that model's Claude Code gets one
        m.context_tokens = 0;
        assert_eq!(ctx(&m).as_deref(), Some("1000000"));
        m.default.clone_from(&m.models[0].id);
        assert_eq!(ctx(&m), None);
    }

    #[test]
    fn argv_is_the_spec_s() {
        let plain = Start::default();
        let a = argv(CLAUDE_BIN, &Session::new(Harness::Claude, "c1"), &plain);
        assert_eq!(a[0], CLAUDE_BIN);
        assert!(a.windows(2).any(|w| w == ["--permission-mode", "bypassPermissions"]));
        assert!(a.iter().any(|x| x == "--setting-sources="));
        assert!(a.windows(2).any(|w| w == ["--disallowedTools", DISALLOWED_TOOLS]));
        assert_eq!(a[a.len() - 2..], ["--session-id".to_owned(), "c1".to_owned()]);
        assert!(a.iter().any(|x| x == "--include-partial-messages"));
        for absent in ["--model", "--mcp-config", "--resume-session-at"] {
            assert!(!a.iter().any(|x| x == absent), "{absent}");
        }
        let r = argv(CLAUDE_BIN, &Session::resume(Harness::Claude, "s9"), &plain);
        assert_eq!(r[r.len() - 2..], ["--resume".to_owned(), "s9".to_owned()]);
        let p = argv(
            CLAUDE_BIN,
            &Session::new(Harness::Claude, "c1"),
            &Start {
                project_settings: true,
                ..Start::default()
            },
        );
        assert!(p.iter().any(|x| x == "--setting-sources=project"));
        assert!(!p.iter().any(|x| x == "--setting-sources="));
    }

    /// Qwen Code is started as Claude Code is (stream-json both ways, every tool allowed, the same session flags),
    /// with what it takes on its command line instead of from managed files or the environment: its model and the
    /// managed instructions. Its own timers and worktree tools are left out.
    #[test]
    fn qwen_codes_argv() {
        let start = Start {
            project_settings: false,
            instructions: "You help the staff.".into(),
            model: "xiaomi/mimo-v2.6-pro".into(),
        };
        let a = argv(QWEN_BIN, &Session::new(Harness::Qwen, "c1"), &start);
        assert_eq!(a[0], QWEN_BIN);
        for pair in [
            ["--output-format", "stream-json"],
            ["--input-format", "stream-json"],
            ["--approval-mode", "yolo"],
            ["--model", "xiaomi/mimo-v2.6-pro"],
            ["--append-system-prompt", "You help the staff."],
        ] {
            assert!(a.windows(2).any(|w| w == pair), "{pair:?}");
        }
        assert!(a.iter().any(|x| x == "--include-partial-messages"));
        for t in QWEN_EXCLUDED_TOOLS {
            assert!(a.iter().any(|x| x == &format!("--exclude-tools={t}")), "{t}");
        }
        assert_eq!(a[a.len() - 2..], ["--session-id".to_owned(), "c1".to_owned()]);
        // Claude Code's flags mean nothing to it
        for absent in [
            "--permission-mode",
            "--disallowedTools",
            "--verbose",
            "--setting-sources=",
        ] {
            assert!(!a.iter().any(|x| x == absent), "{absent}");
        }
        let r = argv(QWEN_BIN, &Session::resume(Harness::Qwen, "q9"), &start);
        assert_eq!(r[r.len() - 2..], ["--resume".to_owned(), "q9".to_owned()]);
        // no instructions: no empty argument
        let bare = argv(QWEN_BIN, &Session::new(Harness::Qwen, "c1"), &Start::default());
        assert!(!bare.iter().any(|x| x == "--append-system-prompt"));
    }

    /// Qwen Code's environment carries no model settings (they are in its managed settings file) and none of
    /// Claude Code's variables.
    #[test]
    fn qwen_codes_env() {
        let boot = BootEnv {
            proxy: "http://sbx:secret@10.0.0.1:3128".into(),
            path: "/usr/local/bin:/usr/bin".into(),
            lang: None,
            lc_all: None,
        };
        let mut d = BTreeMap::new();
        d.insert("HTTPS_PROXY".to_owned(), "http://sbx:x@p:3128".to_owned());
        let e = env(Harness::Qwen, &boot, &d, &models());
        let get = |k: &str| {
            e.iter()
                .find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(str::to_owned))
        };
        assert_eq!(get("HOME").as_deref(), Some("/home/sandbox"));
        assert_eq!(get("HTTPS_PROXY").as_deref(), Some("http://sbx:x@p:3128"));
        assert_eq!(get(QWEN_KEY_ENV).as_deref(), Some("sandbox"));
        assert_eq!(get("NO_PROXY").as_deref(), Some("localhost,127.0.0.1,::1"));
        for absent in [
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_MODEL",
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
        ] {
            assert_eq!(get(absent), None, "{absent}");
        }
    }

    #[test]
    fn env_is_the_descriptor_s_then_the_fixed_table() {
        let boot = BootEnv {
            proxy: "http://sbx:secret@10.0.0.1:3128".into(),
            path: "/usr/local/bin:/usr/bin".into(),
            lang: Some("C.UTF-8".into()),
            lc_all: None,
        };
        let mut d = BTreeMap::new();
        d.insert("HTTPS_PROXY".to_owned(), "http://sbx:x@p:3128".to_owned());
        d.insert("NO_PROXY".to_owned(), "everything".to_owned());
        d.insert("PIP_INDEX_URL".to_owned(), "http://mirror/simple".to_owned());
        d.insert("HERMIT_SANDBOX_TOKEN".to_owned(), "never".to_owned());
        let m = models();
        let e = env(Harness::Claude, &boot, &d, &m);
        let get = |k: &str| {
            e.iter()
                .find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(str::to_owned))
        };
        assert_eq!(get("HOME").as_deref(), Some("/home/sandbox"));
        assert_eq!(get("PATH").as_deref(), Some("/usr/local/bin:/usr/bin"));
        assert_eq!(get("HTTPS_PROXY").as_deref(), Some("http://sbx:x@p:3128"));
        assert_eq!(get("PIP_INDEX_URL").as_deref(), Some("http://mirror/simple"));
        assert_eq!(get("NO_PROXY").as_deref(), Some("localhost,127.0.0.1,::1"));
        assert_eq!(get("ANTHROPIC_BASE_URL").as_deref(), Some("http://model"));
        assert_eq!(get("ANTHROPIC_MODEL").as_deref(), Some(m.default.as_str()));
        assert_eq!(get("ANTHROPIC_SMALL_FAST_MODEL").as_deref(), Some(m.small.as_str()));
        assert_eq!(
            get("CLAUDE_CODE_MAX_CONTEXT_TOKENS"),
            Some(m.context_tokens.to_string())
        );
        assert_eq!(get("HERMIT_SANDBOX_TOKEN"), None);
        assert_eq!(get("CLAUDE_CODE_ENTRYPOINT"), None);
        assert_eq!(
            get("HTTP_PROXY"),
            None,
            "the boot proxy is the supervisor's, not the agent's"
        );
        assert!(!e.iter().any(|kv| kv.contains("secret")));
    }
}
