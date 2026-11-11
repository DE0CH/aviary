//! Launching Claude Code (supervisor 6.1, 6.2): the argv, the environment and the keeper's `SPAWN`.

use std::collections::BTreeMap;

use gateway_api::models::Models;

use crate::boot::{AGENT_HOME, BootEnv};
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

/// Which session a generation runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Session {
    /// `--resume <provider session id>`: a conversation with history.
    Resume(String),
    /// `--session-id <conversation id>`: a new conversation; Claude Code's session id is the conversation id.
    New(String),
}

impl Session {
    /// The session id the generation's `system`/`init` must report.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Session::Resume(s) | Session::New(s) => s,
        }
    }
}

/// The argv of 6.1 (`bin` is [`CLAUDE_BIN`] in a sandbox). `project_settings` is the descriptor's
/// `claude.projectSettings`: the workspace's own settings are loaded (`--setting-sources=project`) instead of none.
#[must_use]
pub fn argv(bin: &str, session: &Session, project_settings: bool) -> Vec<String> {
    let mut a: Vec<String> = [
        bin,
        "--output-format",
        "stream-json",
        "--verbose",
        "--input-format",
        "stream-json",
        "--permission-prompt-tool",
        "stdio",
        "--permission-mode",
        "bypassPermissions",
        if project_settings {
            "--setting-sources=project"
        } else {
            "--setting-sources="
        },
        "--disallowedTools",
        DISALLOWED_TOOLS,
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    match session {
        Session::Resume(id) => a.extend(["--resume".to_owned(), id.clone()]),
        Session::New(id) => a.extend(["--session-id".to_owned(), id.clone()]),
    }
    a
}

/// The fixed table of 6.2 (it wins over the descriptor's `env` on a clash).
#[must_use]
pub fn fixed_env(models: &Models) -> Vec<(&'static str, String)> {
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
    if models.context_tokens > 0 {
        v.push(("CLAUDE_CODE_MAX_CONTEXT_TOKENS", models.context_tokens.to_string()));
    }
    v
}

/// Claude Code's environment: the image's basics, the descriptor's `env` unchanged, then [`fixed_env`].
/// `CLAUDE_CODE_ENTRYPOINT` is never set; nothing of the boot environment beyond `PATH`, `LANG` and `LC_ALL` passes.
#[must_use]
pub fn env(boot: &BootEnv, descriptor_env: &BTreeMap<String, String>, models: &Models) -> Vec<String> {
    let fixed = fixed_env(models);
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
    pub bin: String,
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
        project_settings: bool,
        env: Vec<String>,
        stdout: String,
        stderr: String,
        cgroup: Option<String>,
    ) -> SpawnSpec {
        SpawnSpec {
            name: "claude".into(),
            argv: argv(&self.bin, session, project_settings),
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

    #[test]
    fn argv_is_the_spec_s() {
        let a = argv(CLAUDE_BIN, &Session::New("c1".into()), false);
        assert_eq!(a[0], CLAUDE_BIN);
        assert!(a.windows(2).any(|w| w == ["--permission-mode", "bypassPermissions"]));
        assert!(a.iter().any(|x| x == "--setting-sources="));
        assert!(a.windows(2).any(|w| w == ["--disallowedTools", DISALLOWED_TOOLS]));
        assert_eq!(a[a.len() - 2..], ["--session-id".to_owned(), "c1".to_owned()]);
        for absent in [
            "--model",
            "--mcp-config",
            "--include-partial-messages",
            "--resume-session-at",
        ] {
            assert!(!a.iter().any(|x| x == absent), "{absent}");
        }
        let r = argv(CLAUDE_BIN, &Session::Resume("s9".into()), false);
        assert_eq!(r[r.len() - 2..], ["--resume".to_owned(), "s9".to_owned()]);
        let p = argv(CLAUDE_BIN, &Session::New("c1".into()), true);
        assert!(p.iter().any(|x| x == "--setting-sources=project"));
        assert!(!p.iter().any(|x| x == "--setting-sources="));
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
        let e = env(&boot, &d, &m);
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
