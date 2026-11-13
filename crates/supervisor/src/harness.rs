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
    }
}
