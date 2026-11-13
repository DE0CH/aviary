//! What differs between the harnesses a sandbox runs (Claude Code, Qwen Code). Both speak the same stream-json
//! protocol on stdin and stdout, so one driver ([`crate::cc::run::Driver`]) runs either; this module holds the
//! places where they are not the same.

pub use aviary_core::harness::Harness;

/// The main transcript of `session`, as a path of the mount (the harness's home first).
#[must_use]
pub fn main_transcript(h: Harness, session: &str) -> String {
    match h {
        Harness::Claude => format!("claude/projects/-workspace/{session}.jsonl"),
        Harness::Qwen => format!("qwen/projects/-workspace/chats/{session}.jsonl"),
    }
}

/// The keeper's name for the harness's process (what the start sequence finds of a previous supervisor).
#[must_use]
pub const fn process_name(h: Harness) -> &'static str {
    h.as_str()
}

/// The harness of a process name the keeper lists.
#[must_use]
pub fn of_process(name: &str) -> Option<Harness> {
    Harness::parse(name)
}

/// The slash command that compacts the conversation.
#[must_use]
pub const fn compact_command(h: Harness) -> &'static str {
    match h {
        Harness::Claude => "/compact",
        Harness::Qwen => "/compress",
    }
}

/// The tool that asks the person a question.
#[must_use]
pub const fn ask_tool(h: Harness) -> &'static str {
    match h {
        Harness::Claude => "AskUserQuestion",
        Harness::Qwen => "ask_user_question",
    }
}

/// The command that sends files to the person from a shell (Qwen Code, which has no `SendUserFile` tool).
pub const SEND_FILE: &str = "send-file";

/// Whether a tool call is a shell command that runs [`SEND_FILE`].
#[must_use]
pub fn runs_send_file(tu: &crate::cc::stream::ToolUse) -> bool {
    tu.name == "run_shell_command"
        && tu
            .input
            .get("command")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|c| c.contains(SEND_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_harness_has_its_transcript_and_its_names() {
        assert_eq!(
            main_transcript(Harness::Claude, "s"),
            "claude/projects/-workspace/s.jsonl"
        );
        assert_eq!(
            main_transcript(Harness::Qwen, "s"),
            "qwen/projects/-workspace/chats/s.jsonl"
        );
        for h in Harness::ALL {
            assert_eq!(of_process(process_name(h)), Some(h));
            // a transcript is a file of the harness's own home
            assert!(main_transcript(h, "s").starts_with(&format!("{h}/")));
        }
        assert_eq!(of_process("dockerd"), None);
        assert_eq!(compact_command(Harness::Qwen), "/compress");
        assert_eq!(ask_tool(Harness::Qwen), "ask_user_question");
        let call = |name: &str, command: &str| crate::cc::stream::ToolUse {
            id: "c1".into(),
            name: name.into(),
            input: serde_json::json!({"command": command}),
        };
        assert!(runs_send_file(&call("run_shell_command", "cd out && send-file a.mp4")));
        assert!(!runs_send_file(&call("run_shell_command", "ls out")));
        assert!(!runs_send_file(&call("Bash", "send-file a.mp4")));
    }
}
