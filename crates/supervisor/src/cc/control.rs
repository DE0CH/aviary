//! Control messages on Claude Code's stdin (supervisor 7.2, 7.3, 7.6) in the Agent SDK's shapes, and the answer to
//! each control request Claude Code sends.

use serde_json::{Map, Value, json};

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

/// `stop_task`.
#[must_use]
pub fn stop_task(request_id: &str, task_id: &str) -> Vec<u8> {
    line(&json!({"type": "control_request", "request_id": request_id,
                 "request": {"subtype": "stop_task", "task_id": task_id}}))
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

/// What to do with a control request from Claude Code.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    /// Write this line now.
    Now(Vec<u8>),
    /// A tool that needs a person: open an ask (7.6).
    Ask {
        tool_name: String,
        tool_use_id: Option<String>,
        input: Map<String, Value>,
    },
}

/// The answer to `control_request {subtype, …}` (7.3, 7.6).
#[must_use]
pub fn answer(request_id: &str, subtype: &str, request: &Value) -> Answer {
    match subtype {
        "can_use_tool" => {
            let tool_use_id = request.get("tool_use_id").and_then(Value::as_str);
            let input = request.get("input").cloned().unwrap_or_else(|| json!({}));
            if request
                .get("requires_user_interaction")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                Answer::Ask {
                    tool_name: request
                        .get("tool_name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    tool_use_id: tool_use_id.map(str::to_owned),
                    input: input.as_object().cloned().unwrap_or_default(),
                }
            } else {
                // bypass semantics: the input unchanged
                Answer::Now(allow(request_id, &input, tool_use_id))
            }
        }
        "elicitation" => Answer::Now(success(request_id, &json!({"action": "decline"}))),
        "request_user_dialog" => Answer::Now(success(request_id, &json!({"behavior": "cancelled"}))),
        "hook_callback" => Answer::Now(error(request_id, "no hooks registered")),
        "mcp_message" => Answer::Now(error(request_id, "no SDK MCP servers")),
        _ => Answer::Now(error(request_id, "unsupported")),
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
        let Answer::Now(b) = answer(
            "r1",
            "can_use_tool",
            &json!({"tool_name": "Bash", "input": {"command": "ls"}, "tool_use_id": "t1"}),
        ) else {
            panic!()
        };
        let a = v(&b);
        assert_eq!(a["response"]["subtype"], "success");
        assert_eq!(a["response"]["request_id"], "r1");
        assert_eq!(a["response"]["response"]["behavior"], "allow");
        assert_eq!(a["response"]["response"]["updatedInput"]["command"], "ls");
        assert_eq!(a["response"]["response"]["toolUseID"], "t1");
        assert!(matches!(
            answer("r2", "can_use_tool", &json!({"tool_name": "AskUserQuestion", "requires_user_interaction": true, "input": {"q": 1}})),
            Answer::Ask { tool_name, .. } if tool_name == "AskUserQuestion"
        ));
        let Answer::Now(b) = answer("r3", "elicitation", &json!({})) else {
            panic!()
        };
        assert_eq!(v(&b)["response"]["response"]["action"], "decline");
        let Answer::Now(b) = answer("r4", "request_user_dialog", &json!({})) else {
            panic!()
        };
        assert_eq!(v(&b)["response"]["response"]["behavior"], "cancelled");
        for s in ["hook_callback", "mcp_message", "set_model_please"] {
            let Answer::Now(b) = answer("r5", s, &json!({})) else {
                panic!()
            };
            assert_eq!(v(&b)["response"]["subtype"], "error", "{s}");
        }
        assert_eq!(
            v(&deny("r6", "No answer was given.", None))["response"]["response"]["behavior"],
            "deny"
        );
        assert_eq!(v(&stop_task("s1", "t9"))["request"]["task_id"], "t9");
        assert_eq!(v(&initialize("init-1"))["request"]["subtype"], "initialize");
    }
}
