//! Model errors as Claude Code reports them: the `model.*` tag of an assistant transcript line Claude Code wrote in
//! place of a reply (`isApiErrorMessage`), and the code of a turn whose `result` is such an error.
//!
//! Claude Code labels each of these lines itself: `error` (`rate_limit`, `billing_error`, `authentication_failed`,
//! `invalid_request`, …), `apiError` (`usage_limit_reached`, `model_requires_usage_credits`,
//! `safety_monitor_blocked`, …) and, for a refusal by the model's safeguards, `message.stop_reason: "refusal"` with
//! `message.stop_details.category`. Those labels come first; the text (`API Error: 429 …`) is read only for a line
//! that has none. What Claude Code 2.1.325 writes:
//!
//! | case | text | labels |
//! |---|---|---|
//! | the safeguards flagged the message | `API Error: Sonnet 4.5's safeguards flagged this message. … Details: [cyber]` | `stop_reason: refusal`, `stop_details.category` |
//! | the subscription's usage limit | `You've hit your session limit · resets 3pm (UTC)` | `error: rate_limit`, `apiError: usage_limit_reached` |
//! | a model that needs usage credits | `You've hit your monthly spend limit. …` | `error: rate_limit`, `apiError: model_requires_usage_credits` |
//! | a short-lived 429 | `API Error: Server is temporarily limiting requests (not your usage limit) · …` | `error: rate_limit` |
//! | credit too low | `Credit balance is too low` | `error: billing_error` |
//! | login refused | `Please run /login · API Error: 401 …` | `error: authentication_failed` |

use serde_json::Value;

use crate::errors::{DataCode, ErrorInfo};

/// How Claude Code's text of a usage limit begins (its own list of them, 2.1.325).
const USAGE_LIMIT_OPENINGS: [&str; 5] = [
    "You've hit your",
    "You've reached your",
    "You're out of usage credits",
    "Your org is out of usage",
    "Claude AI usage limit reached",
];

/// `apiError` values that mean the account's usage is used up until a reset or more credits.
const USAGE_LIMIT_API_ERRORS: [&str; 3] = [
    "usage_limit_reached",
    "model_requires_usage_credits",
    "long_context_credits_required",
];

/// `apiError` values of a message the safeguards stopped.
const SAFEGUARD_API_ERRORS: [&str; 2] = ["safety_monitor_blocked", "dlp_request_denied"];

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// The text without Claude Code's `API Error:` opening, trimmed.
fn words(text: &str) -> String {
    let t = text.trim();
    t.strip_prefix("API Error:").map_or(t, str::trim_start).to_owned()
}

fn usage_limit_text(text: &str) -> bool {
    let w = words(text);
    USAGE_LIMIT_OPENINGS.iter().any(|o| w.starts_with(o))
}

fn safeguards_text(text: &str) -> bool {
    text.contains("safeguards flagged this message")
}

fn tagged(code: DataCode, text: &str, detail: Option<&str>) -> ErrorInfo {
    let mut e = ErrorInfo::new(code, words(text));
    e.detail = detail.filter(|d| !d.is_empty()).map(str::to_owned);
    e
}

/// The model error tag of an assistant transcript line with its text (`None`: the line is a reply).
#[must_use]
pub fn of_line(line: &Value, text: Option<&str>) -> Option<ErrorInfo> {
    let text = text.unwrap_or_default();
    let message = line.get("message");
    let stop_reason = message.and_then(|m| s(m, "stop_reason"));
    let category = message
        .and_then(|m| m.get("stop_details"))
        .and_then(|d| s(d, "category"));
    let flagged = line.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true);
    if !flagged {
        // a line from before Claude Code labelled its errors
        return of_text(text);
    }
    let api_error = s(line, "apiError").unwrap_or_default();
    let kind = s(line, "error").unwrap_or_default();
    if stop_reason == Some("refusal") || SAFEGUARD_API_ERRORS.contains(&api_error) || safeguards_text(text) {
        return Some(tagged(DataCode::ModelSafeguards, text, category));
    }
    if USAGE_LIMIT_API_ERRORS.contains(&api_error) || usage_limit_text(text) {
        return Some(tagged(DataCode::ModelUsageLimit, text, None));
    }
    let labelled = match kind {
        "rate_limit" => Some(DataCode::ModelRateLimited),
        "billing_error" => Some(DataCode::ModelBudget),
        "authentication_failed" => Some(DataCode::ModelAccess),
        _ => None,
    };
    let mut e = of_text(text).unwrap_or_else(|| ErrorInfo::new(DataCode::ModelRefused, words(text)));
    if let Some(code) = labelled {
        e.code = code;
    }
    Some(e)
}

/// The code of a turn whose `result` is an error, from the result's text and `stop_reason` (`None`: not a model
/// error).
#[must_use]
pub fn of_result(text: &str, stop_reason: Option<&str>) -> Option<ErrorInfo> {
    if stop_reason == Some("refusal") || safeguards_text(text) {
        return Some(tagged(DataCode::ModelSafeguards, text, None));
    }
    if usage_limit_text(text) {
        return Some(tagged(DataCode::ModelUsageLimit, text, None));
    }
    of_text(text)
}

/// The model error tag of an `API Error: …` text (magpie 8.9; today's `jarvis-errors.ts` rules), with the usage
/// limit and the safeguards read from the words for a line without Claude Code's labels.
#[must_use]
pub fn of_text(text: &str) -> Option<ErrorInfo> {
    // Claude Code's line before 2.1 had no `API Error:` opening; a reply may well begin "You've reached your"
    if text.trim_start().starts_with("Claude AI usage limit reached") {
        return Some(tagged(DataCode::ModelUsageLimit, text, None));
    }
    let rest = text.trim().strip_prefix("API Error:")?.trim_start();
    if usage_limit_text(rest) {
        return Some(tagged(DataCode::ModelUsageLimit, rest, None));
    }
    if safeguards_text(rest) {
        return Some(tagged(DataCode::ModelSafeguards, rest, None));
    }
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let boundary = rest
        .as_bytes()
        .get(digits)
        .is_none_or(|c| !c.is_ascii_alphanumeric() && *c != b'_');
    let (status, body) = if digits == 3 && boundary {
        (rest[..3].parse::<u16>().ok(), rest[3..].trim_start())
    } else {
        (None, rest)
    };
    let parsed = body
        .find('{')
        .and_then(|i| serde_json::from_str::<Value>(&body[i..]).ok())
        .filter(Value::is_object);
    let inner = parsed.as_ref().and_then(|b| b.get("error")).filter(|e| e.is_object());
    let kind = inner.and_then(|e| s(e, "type")).unwrap_or_default();
    let message = match (&parsed, inner.and_then(|e| s(e, "message"))) {
        (_, Some(m)) => m.trim().to_owned(),
        (None, None) => body.trim().to_owned(),
        (Some(_), None) => String::new(),
    };
    // Claude Code's own wording of a 429 it gave no status: "Request rejected (429)", "temporarily limiting requests"
    let rejected_429 = status.is_none() && (body.contains("(429)") || body.contains("limiting requests"));
    let code = match status.unwrap_or(0) {
        _ if kind == "budget_exceeded" => DataCode::ModelBudget,
        402 => DataCode::ModelBudget,
        429 => DataCode::ModelRateLimited,
        _ if kind == "rate_limit_error" || rejected_429 => DataCode::ModelRateLimited,
        s if s >= 500 => DataCode::ModelUnavailable,
        _ if kind == "overloaded_error" || kind == "api_error" => DataCode::ModelUnavailable,
        401 | 403 => DataCode::ModelAccess,
        s if s >= 400 => DataCode::ModelRefused,
        _ => DataCode::ModelConnection,
    };
    let mut e = ErrorInfo::new(code, message);
    e.status = status;
    Some(e)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;

    /// The refusal line Claude Code 2.1.304 wrote under the fuse-gate spike (fabric/fuse-gate results
    /// `tomb-t-refusal`), trimmed to the fields read here.
    const REFUSAL: &str = "API Error: Sonnet 4.5's safeguards flagged this message. Our intentionally broad \
        safeguards allow us to deliver more capabilities faster, but can sometimes flag legitimate cybersecurity \
        work. Learn more: https://support.claude.com/en/articles/14604842-real-time-cyber-safeguards-on-claude\n\n\
        Details: `[cyber]`\n\nMessage ID: msg_00000002";

    fn line(extra: &Value, stop_reason: &str, stop_details: &Value) -> Value {
        let mut l = json!({"type": "assistant", "isApiErrorMessage": true,
            "message": {"model": "<synthetic>", "stop_reason": stop_reason, "stop_details": stop_details}});
        for (k, v) in extra.as_object().unwrap() {
            l[k] = v.clone();
        }
        l
    }

    #[test]
    fn a_refusal_by_the_safeguards_is_model_safeguards_with_claude_codes_words() {
        let l = line(
            &json!({}),
            "refusal",
            &json!({"type": "refusal", "category": "cyber", "explanation": "gate refusal"}),
        );
        let e = of_line(&l, Some(REFUSAL)).unwrap();
        assert_eq!(e.code, DataCode::ModelSafeguards);
        assert!(
            e.message.starts_with("Sonnet 4.5's safeguards flagged this message."),
            "{}",
            e.message
        );
        assert!(e.message.contains("Learn more: https://support.claude.com/"));
        assert_eq!(e.detail.as_deref(), Some("cyber"));
        // the result of that turn, and the text alone
        assert_eq!(
            of_result(REFUSAL, Some("refusal")).unwrap().code,
            DataCode::ModelSafeguards
        );
        assert_eq!(of_text(REFUSAL).unwrap().code, DataCode::ModelSafeguards);
        let blocked = line(
            &json!({"apiError": "safety_monitor_blocked"}),
            "stop_sequence",
            &Value::Null,
        );
        assert_eq!(of_line(&blocked, Some("x")).unwrap().code, DataCode::ModelSafeguards);
    }

    #[test]
    fn the_subscriptions_usage_limit_is_model_usage_limit_with_the_reset_time() {
        let text = "You've hit your session limit · resets 3pm (UTC)";
        let l = line(
            &json!({"error": "rate_limit", "apiError": "usage_limit_reached"}),
            "stop_sequence",
            &Value::Null,
        );
        let e = of_line(&l, Some(text)).unwrap();
        assert_eq!((e.code, e.message.as_str()), (DataCode::ModelUsageLimit, text));
        let credits = line(
            &json!({"error": "rate_limit", "apiError": "model_requires_usage_credits"}),
            "stop_sequence",
            &Value::Null,
        );
        assert_eq!(
            of_line(&credits, Some("You've hit your monthly spend limit."))
                .unwrap()
                .code,
            DataCode::ModelUsageLimit
        );
        // without the labels: the words, the result, Claude Code's older wording
        let bare = line(&json!({"error": "rate_limit"}), "stop_sequence", &Value::Null);
        assert_eq!(of_line(&bare, Some(text)).unwrap().code, DataCode::ModelUsageLimit);
        assert_eq!(of_result(text, None).unwrap().code, DataCode::ModelUsageLimit);
        assert_eq!(
            of_text("Claude AI usage limit reached|1763380800").unwrap().code,
            DataCode::ModelUsageLimit
        );
    }

    #[test]
    fn claude_codes_labels_decide_over_the_words() {
        let short = "API Error: Server is temporarily limiting requests (not your usage limit) · this may be a \
            temporary capacity issue.";
        let l = line(&json!({"error": "rate_limit"}), "stop_sequence", &Value::Null);
        assert_eq!(of_line(&l, Some(short)).unwrap().code, DataCode::ModelRateLimited);
        assert_eq!(
            of_text(short).unwrap().code,
            DataCode::ModelRateLimited,
            "and from the words alone"
        );
        let billing = line(&json!({"error": "billing_error"}), "stop_sequence", &Value::Null);
        let e = of_line(&billing, Some("Credit balance is too low")).unwrap();
        assert_eq!(
            (e.code, e.message.as_str()),
            (DataCode::ModelBudget, "Credit balance is too low")
        );
        let login = line(
            &json!({"error": "authentication_failed"}),
            "stop_sequence",
            &Value::Null,
        );
        assert_eq!(
            of_line(&login, Some("Please run /login · API Error: 401 x"))
                .unwrap()
                .code,
            DataCode::ModelAccess
        );
        // an error line with no label and no `API Error:` opening shows Claude Code's words
        let long = line(&json!({}), "stop_sequence", &Value::Null);
        let e = of_line(&long, Some("Prompt is too long")).unwrap();
        assert_eq!(
            (e.code, e.message.as_str()),
            (DataCode::ModelRefused, "Prompt is too long")
        );
        // a reply is no error, whatever its words
        let reply = json!({"type": "assistant", "message": {"model": "m", "stop_reason": "end_turn"}});
        assert!(of_line(&reply, Some("hello")).is_none());
        assert!(of_line(&reply, Some("You've reached your goal.")).is_none());
        assert!(of_result("boom", None).is_none());
    }

    #[test]
    fn api_errors_map_to_model_codes() {
        let cases = [
            (
                r#"API Error: 402 {"error":{"message":"budget used up"}}"#,
                DataCode::ModelBudget,
                Some(402),
                "budget used up",
            ),
            (
                r#"API Error: 400 {"error":{"type":"budget_exceeded","message":"m"}}"#,
                DataCode::ModelBudget,
                Some(400),
                "m",
            ),
            (
                "API Error: 429 slow down",
                DataCode::ModelRateLimited,
                Some(429),
                "slow down",
            ),
            (
                r#"API Error: 529 {"error":{"type":"overloaded_error"}}"#,
                DataCode::ModelUnavailable,
                Some(529),
                "",
            ),
            (
                r#"API Error: {"error":{"type":"api_error","message":"x"}}"#,
                DataCode::ModelUnavailable,
                None,
                "x",
            ),
            (
                "API Error: 403 forbidden",
                DataCode::ModelAccess,
                Some(403),
                "forbidden",
            ),
            (
                "API Error: 404 no such model",
                DataCode::ModelRefused,
                Some(404),
                "no such model",
            ),
            (
                "API Error: Connection error.",
                DataCode::ModelConnection,
                None,
                "Connection error.",
            ),
            (
                "API Error: Request rejected (429) · resets 3pm",
                DataCode::ModelRateLimited,
                None,
                "Request rejected (429) · resets 3pm",
            ),
            ("API Error: 4000 tokens", DataCode::ModelConnection, None, "4000 tokens"),
        ];
        for (text, code, status, message) in cases {
            let e = of_text(text).unwrap();
            assert_eq!(
                (e.code, e.status, e.message.as_str()),
                (code, status, message),
                "{text}"
            );
        }
        assert!(of_text("Not an error").is_none());
    }
}
