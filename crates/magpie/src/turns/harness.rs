//! What differs between harnesses in the store's turn handling: where a session's main transcript is, and which
//! session a turn resumes.

use magpie_api::Harness;
use serde_json::Value;

/// The main transcript of `session`, relative to the harness's home.
#[must_use]
pub fn main_transcript(harness: &str, session: &str) -> String {
    match Harness::parse(harness).unwrap_or_default() {
        Harness::Claude => format!("projects/-workspace/{session}.jsonl"),
        Harness::Qwen => format!("projects/-workspace/chats/{session}.jsonl"),
    }
}

/// The session a turn in the harness `picked` resumes: the conversation's current one when that harness ran last
/// (`current` with its `session`), else the one kept from when it ran before (`kept`: `harness_sessions`), else
/// none (the harness starts a new session).
#[must_use]
pub fn session_of(picked: &str, current: &str, session: Option<String>, kept: &Value) -> Option<String> {
    if picked == current {
        session
    } else {
        kept.get(picked).and_then(Value::as_str).map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn each_harness_keeps_its_transcript_where_it_writes_it() {
        assert_eq!(main_transcript("claude", "s-1"), "projects/-workspace/s-1.jsonl");
        assert_eq!(main_transcript("qwen", "s-1"), "projects/-workspace/chats/s-1.jsonl");
    }

    #[test]
    fn a_turn_resumes_the_session_of_its_own_harness() {
        let kept = json!({"qwen": "q-1"});
        assert_eq!(
            session_of("claude", "claude", Some("c-1".into()), &kept),
            Some("c-1".into())
        );
        assert_eq!(
            session_of("qwen", "claude", Some("c-1".into()), &kept),
            Some("q-1".into())
        );
        assert_eq!(session_of("qwen", "claude", Some("c-1".into()), &json!({})), None);
        assert_eq!(session_of("claude", "claude", None, &json!({})), None);
    }
}
