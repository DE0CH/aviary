//! Control messages on Claude Code's stdin (supervisor 7.2, 7.3, 7.6) in the Agent SDK's shapes, and the answer to
//! each control request Claude Code sends.

use serde_json::{Value, json};

fn line(v: &Value) -> Vec<u8> {
    let mut b = serde_json::to_vec(v).unwrap_or_default();
    b.push(b'\n');
    b
}

/// `initialize` (no hooks, no SDK MCP servers, no system prompt fields).
#[must_use]
pub fn initialize(request_id: &str) -> Vec<u8> {
    line(&json!({"type": "control_request", "request_id": request_id, "request": {"subtype": "initialize"}}))
}

/// `interrupt`.
#[must_use]
pub fn interrupt(request_id: &str) -> Vec<u8> {
    line(&json!({"type": "control_request", "request_id": request_id, "request": {"subtype": "interrupt"}}))
}

/// How the id of a `continue_last_turn` request begins (its answer ends the turn when the harness has nothing to go
/// on from).
pub const CONTINUE_PREFIX: &str = "continue-";

/// Qwen Code's `continue_last_turn`: it goes on from where its last turn was cut off, with no message (tool results
/// whose reply never came are sent again; tool calls nothing answered are closed). Its answer says `accepted`; the
/// turn's lines and its `result` follow as after a message.
#[must_use]
pub fn continue_last_turn(request_id: &str) -> Vec<u8> {
    line(&json!({"type": "control_request", "request_id": request_id,
                 "request": {"subtype": "continue_last_turn"}}))
}

/// `stop_task`.
#[must_use]
pub fn stop_task(request_id: &str, task_id: &str) -> Vec<u8> {
    line(&json!({"type": "control_request", "request_id": request_id,
                 "request": {"subtype": "stop_task", "task_id": task_id}}))
}

/// `set_model`: the model of Claude Code's model calls from the next one (what `/model` does in its terminal); the
/// conversation and its background tasks go on in the same process.
#[must_use]
pub fn set_model(request_id: &str, model: &str) -> Vec<u8> {
    line(&json!({"type": "control_request", "request_id": request_id,
                 "request": {"subtype": "set_model", "model": model}}))
}

/// `set_max_thinking_tokens`: `Some(0)` switches thinking off (requests carry no `thinking`), `None` on again.
#[must_use]
pub fn set_max_thinking_tokens(request_id: &str, tokens: Option<u32>) -> Vec<u8> {
    line(&json!({"type": "control_request", "request_id": request_id,
                 "request": {"subtype": "set_max_thinking_tokens", "max_thinking_tokens": tokens}}))
}

/// `apply_flag_settings` with `effortLevel`: Claude Code's effort level (requests carry it as
/// `output_config.effort`).
#[must_use]
pub fn set_effort(request_id: &str, level: &str) -> Vec<u8> {
    line(&json!({"type": "control_request", "request_id": request_id,
                 "request": {"subtype": "apply_flag_settings", "settings": {"effortLevel": level}}}))
}

/// Qwen Code's `set_effort`: its reasoning effort for the model calls from the next one (`none`: thinking off).
#[must_use]
pub fn qwen_set_effort(request_id: &str, level: &str) -> Vec<u8> {
    line(&json!({"type": "control_request", "request_id": request_id,
                 "request": {"subtype": "set_effort", "effort": level}}))
}

fn success(request_id: &str, response: &Value) -> Vec<u8> {
    line(&json!({"type": "control_response",
                 "response": {"subtype": "success", "request_id": request_id, "response": response}}))
}

/// `can_use_tool` answered `allow` with `updated_input`.
#[must_use]
pub fn allow(request_id: &str, updated_input: &Value, tool_use_id: Option<&str>) -> Vec<u8> {
    success(
        request_id,
        &json!({"behavior": "allow", "updatedInput": updated_input, "toolUseID": tool_use_id}),
    )
}

/// `can_use_tool` answered `deny`.
#[must_use]
pub fn deny(request_id: &str, message: &str, tool_use_id: Option<&str>) -> Vec<u8> {
    success(
        request_id,
        &json!({"behavior": "deny", "message": message, "toolUseID": tool_use_id}),
    )
}

/// An error answer.
#[must_use]
pub fn error(request_id: &str, message: &str) -> Vec<u8> {
    line(&json!({"type": "control_response",
                 "response": {"subtype": "error", "request_id": request_id, "error": message}}))
}

/// What a tool call that would need the person is told: nobody answers a harness's own prompts, a question is the
/// `ask` command.
pub const NEEDS_THE_PERSON: &str = "Nobody answers this tool here: ask the user with the `ask` shell command.";

/// The line that answers `control_request {subtype, …}` (7.3). Every tool is allowed with its input unchanged
/// (bypass semantics), except one that would wait on the person (a harness's own question or plan tool, which the
/// launch does not offer): it is denied with [`NEEDS_THE_PERSON`], so no turn waits on a prompt nobody shows.
#[must_use]
pub fn answer(request_id: &str, subtype: &str, request: &Value) -> Vec<u8> {
    match subtype {
        "can_use_tool" => {
            let tool_use_id = request.get("tool_use_id").and_then(Value::as_str);
            let input = request.get("input").cloned().unwrap_or_else(|| json!({}));
            // Claude Code marks a tool that needs the person; Qwen Code asks permission only for its question
            // tool when every tool is allowed, and does not mark it
            let needs_person = request
                .get("requires_user_interaction")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || request.get("tool_name").and_then(Value::as_str) == Some("ask_user_question");
            if needs_person {
                deny(request_id, NEEDS_THE_PERSON, tool_use_id)
            } else {
                allow(request_id, &input, tool_use_id)
            }
        }
        "elicitation" => success(request_id, &json!({"action": "decline"})),
        "request_user_dialog" => success(request_id, &json!({"behavior": "cancelled"})),
        "hook_callback" => error(request_id, "no hooks registered"),
        "mcp_message" => error(request_id, "no SDK MCP servers"),
        _ => error(request_id, "unsupported"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(b: &[u8]) -> Value {
        assert_eq!(*b.last().unwrap(), b'\n');
        serde_json::from_slice(b).unwrap()
    }

    #[test]
    fn answers() {
        let b = answer(
            "r1",
            "can_use_tool",
            &json!({"tool_name": "Bash", "input": {"command": "ls"}, "tool_use_id": "t1"}),
        );
        let a = v(&b);
        assert_eq!(a["response"]["subtype"], "success");
        assert_eq!(a["response"]["request_id"], "r1");
        assert_eq!(a["response"]["response"]["behavior"], "allow");
        assert_eq!(a["response"]["response"]["updatedInput"]["command"], "ls");
        assert_eq!(a["response"]["response"]["toolUseID"], "t1");
        // a tool that would wait on the person is denied at once, pointing at the `ask` command: Claude Code marks
        // it, Qwen Code's question tool carries no mark
        for req in [
            json!({"tool_name": "AskUserQuestion", "requires_user_interaction": true, "input": {"q": 1}}),
            json!({"tool_name": "ask_user_question", "tool_use_id": "t9", "input": {"questions": []}}),
        ] {
            let r = v(&answer("r2", "can_use_tool", &req));
            assert_eq!(r["response"]["response"]["behavior"], "deny", "{req}");
            assert_eq!(r["response"]["response"]["message"], NEEDS_THE_PERSON);
        }
        assert_eq!(
            v(&answer("r3", "elicitation", &json!({})))["response"]["response"]["action"],
            "decline"
        );
        assert_eq!(
            v(&answer("r4", "request_user_dialog", &json!({})))["response"]["response"]["behavior"],
            "cancelled"
        );
        for s in ["hook_callback", "mcp_message", "set_model_please"] {
            assert_eq!(v(&answer("r5", s, &json!({})))["response"]["subtype"], "error", "{s}");
        }
        assert_eq!(
            v(&deny("r6", "No answer was given.", None))["response"]["response"]["behavior"],
            "deny"
        );
        assert_eq!(v(&stop_task("s1", "t9"))["request"]["task_id"], "t9");
        assert_eq!(v(&initialize("init-1"))["request"]["subtype"], "initialize");
        let e = v(&qwen_set_effort("c1", "high"));
        assert_eq!(
            (e["request"]["subtype"].as_str(), e["request"]["effort"].as_str()),
            (Some("set_effort"), Some("high"))
        );
    }
}
