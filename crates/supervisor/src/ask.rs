//! `ask`: a question for the person, as an ordinary command (a link to the supervisor binary). It opens the question
//! (`POST /v1/questions` on agent.sock, which the supervisor passes to magpie) and blocks until the person answers,
//! then prints the answer. The question belongs to the command, not to the turn: nothing dismisses it for the person.
//! Cancelling the command leaves it open; `ask wait <id>` blocks on it again and `ask status <id>` looks once.
//!
//! - `ask` (the questions as JSON on stdin, or as the one argument): `AskUserQuestion`'s shape,
//!   `{"questions": [{"question", "header", "options": [{"label", "description"}], "multiSelect"}]}`;
//! - `ask wait <id>`: blocks until the answer;
//! - `ask status <id>`: the answer, or exit [`EXIT_WAITING`] while it waits.
//!
//! Exit 0 with the answer, [`EXIT_DECLINED`] when the person declined, 1 on a failure, 2 on bad usage.

use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::path::Path;

use http_body_util::{BodyExt as _, Full};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use serde_json::{Map, Value};
use supervisor_api::local::{AGENT_SOCKET, QUESTION_WAIT_MAX, QuestionAnswer, QuestionOpen, QuestionOpened};

/// The person declined the question.
pub const EXIT_DECLINED: i32 = 3;
/// `ask status`: still waiting.
pub const EXIT_WAITING: i32 = 4;

/// The command line, parsed.
#[derive(Debug, PartialEq)]
pub enum Command {
    /// Open a question with this input.
    Open(Map<String, Value>),
    Wait(String),
    Status(String),
}

/// Parses the arguments (after the program name); `stdin` is read only for `ask` with no argument.
///
/// # Errors
/// The usage text or why the questions are malformed.
pub fn parse(args: &[String], stdin: impl FnOnce() -> String) -> Result<Command, String> {
    match args {
        [w, id] if w == "wait" => Ok(Command::Wait(id.clone())),
        [s, id] if s == "status" => Ok(Command::Status(id.clone())),
        [] => questions(&stdin()).map(Command::Open),
        [json] if !json.starts_with('-') => questions(json).map(Command::Open),
        _ => Err(USAGE.to_owned()),
    }
}

const USAGE: &str = "usage: ask < questions.json | ask '<questions JSON>' | ask wait <id> | ask status <id>\n\
    questions: {\"questions\": [{\"question\": \"…?\", \"header\": \"≤12 chars\", \"options\": \
    [{\"label\": \"…\", \"description\": \"…\"}], \"multiSelect\": false}]}";

/// The questions, checked for the shape the question card shows.
///
/// # Errors
/// What is wrong with them.
pub fn questions(text: &str) -> Result<Map<String, Value>, String> {
    let v: Value =
        serde_json::from_str(text.trim()).map_err(|e| format!("the questions are not JSON: {e}\n{USAGE}"))?;
    let Value::Object(input) = v else {
        return Err(format!("the questions must be a JSON object\n{USAGE}"));
    };
    let qs = input
        .get("questions")
        .and_then(Value::as_array)
        .filter(|q| !q.is_empty())
        .ok_or_else(|| format!("\"questions\" must be a non-empty array\n{USAGE}"))?;
    for (i, q) in qs.iter().enumerate() {
        let text = q.get("question").and_then(Value::as_str).unwrap_or_default();
        if text.trim().is_empty() {
            return Err(format!("question {} has no \"question\" text", i + 1));
        }
        let options = q.get("options").and_then(Value::as_array);
        if options.is_some_and(|o| o.iter().any(|o| o.get("label").and_then(Value::as_str).is_none())) {
            return Err(format!("every option of question {} needs a \"label\"", i + 1));
        }
    }
    Ok(input)
}

/// What the command prints for the answer of question `id`, and its exit code. Every line of the answer ends with
/// its marker ([`magpie_api::sandbox::answer_marker`]): the chat shows the question where the AI first read it, and
/// a partial read (`tail -1` of a background task's output) still carries it.
#[must_use]
pub fn render(id: &str, answer: &Value) -> (String, i32) {
    let marker = magpie_api::sandbox::answer_marker(id);
    if answer.get("behavior").and_then(Value::as_str) == Some("deny") {
        let why = answer.get("message").and_then(Value::as_str).unwrap_or("");
        let line = format!("The question was declined. {why}");
        return (format!("{} {marker}", line.trim_end()), EXIT_DECLINED);
    }
    let input = answer.get("updatedInput");
    let answers = input.and_then(|i| i.get("answers")).and_then(Value::as_object);
    let Some(answers) = answers else {
        return (format!("Answer: {} {marker}", input.unwrap_or(answer)), 0);
    };
    let mut out = String::new();
    for (q, a) in answers {
        let a = a.as_str().map_or_else(|| a.to_string(), str::to_owned);
        let _ = writeln!(out, "{q}\n→ {a} {marker}");
    }
    if let Some(notes) = input.and_then(|i| i.get("annotations")).and_then(Value::as_object) {
        for (q, n) in notes {
            if let Some(n) = n.get("notes").and_then(Value::as_str).filter(|n| !n.is_empty()) {
                let _ = writeln!(out, "Notes on \"{q}\": {n} {marker}");
            }
        }
    }
    (out.trim_end().to_owned(), 0)
}

/// One HTTP request on agent.sock.
pub(crate) async fn call(
    socket: &Path,
    method: hyper::Method,
    path: &str,
    body: Option<Vec<u8>>,
) -> Result<(u16, Bytes), String> {
    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .map_err(|e| format!("the supervisor's socket: {e}"))?;
    let (mut send, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| e.to_string())?;
    tokio::spawn(conn);
    let req = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header(hyper::header::HOST, "supervisor")
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body.unwrap_or_default())))
        .map_err(|e| e.to_string())?;
    let res = send.send_request(req).await.map_err(|e| e.to_string())?;
    let status = res.status().as_u16();
    let body = res.into_body().collect().await.map_err(|e| e.to_string())?.to_bytes();
    Ok((status, body))
}

fn failure(status: u16, body: &[u8]) -> String {
    let msg = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    format!("the supervisor answered {status}: {msg}")
}

pub(crate) async fn open(socket: &Path, input: Map<String, Value>) -> Result<String, String> {
    let body = serde_json::to_vec(&QuestionOpen { input }).map_err(|e| e.to_string())?;
    let (status, b) = call(socket, hyper::Method::POST, "/v1/questions", Some(body)).await?;
    if status != 200 {
        return Err(failure(status, &b));
    }
    serde_json::from_slice::<QuestionOpened>(&b)
        .map(|o| o.ask_id)
        .map_err(|e| e.to_string())
}

/// The answer, once there; `None` after `wait_ms` without one.
async fn poll(socket: &Path, id: &str, wait_ms: u32) -> Result<Option<Value>, String> {
    let (status, b) = call(
        socket,
        hyper::Method::GET,
        &format!("/v1/questions/{id}?waitMs={wait_ms}"),
        None,
    )
    .await?;
    match status {
        200 => serde_json::from_slice::<QuestionAnswer>(&b)
            .map(|a| a.answer)
            .map_err(|e| e.to_string()),
        404 => Err(format!("no question {id}")),
        _ => Err(failure(status, &b)),
    }
}

/// Blocks until the answer. A failed poll (a release moving the conversation, magpie restarting) is retried: the
/// question stays open in magpie, so only the person's answer ends the wait.
pub(crate) async fn wait(socket: &Path, id: &str) -> Result<Value, String> {
    let wait_ms = u32::try_from(QUESTION_WAIT_MAX.as_millis()).unwrap_or(u32::MAX);
    let mut failures = 0u32;
    loop {
        match poll(socket, id, wait_ms).await {
            Ok(Some(a)) => return Ok(a),
            Ok(None) => failures = 0,
            Err(e) if e.starts_with("no question") => return Err(e),
            Err(e) => {
                failures += 1;
                if failures == 1 || failures.is_multiple_of(60) {
                    let _ = writeln!(std::io::stderr(), "ask: {e}; still waiting");
                }
                tokio::time::sleep(std::time::Duration::from_secs(1u64 << failures.min(4))).await;
            }
        }
    }
}

fn print_answer(id: &str, answer: &Value) -> i32 {
    let (text, code) = render(id, answer);
    let _ = writeln!(std::io::stdout(), "{text}");
    code
}

/// `ask …`: the exit code.
#[must_use]
pub fn cli(args: &[String]) -> i32 {
    let cmd = parse(args, || {
        let mut s = String::new();
        let _ = std::io::stdin().read_to_string(&mut s);
        s
    });
    let cmd = match cmd {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "{e}");
            return 2;
        }
    };
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return 1;
    };
    let socket = Path::new(AGENT_SOCKET);
    let result = rt.block_on(async {
        match cmd {
            Command::Open(input) => {
                let id = open(socket, input).await?;
                let mut out = std::io::stdout();
                let _ = writeln!(
                    out,
                    "Question {id} is open; waiting for the answer. If this command is cancelled the question stays \
                     open: `ask wait {id}` waits again, `ask status {id}` looks once."
                );
                let _ = out.flush();
                wait(socket, &id).await.map(|a| print_answer(&id, &a))
            }
            Command::Wait(id) => wait(socket, &id).await.map(|a| print_answer(&id, &a)),
            Command::Status(id) => {
                if let Some(a) = poll(socket, &id, 0).await? {
                    Ok(print_answer(&id, &a))
                } else {
                    let _ = writeln!(std::io::stdout(), "Question {id} is still waiting for the answer.");
                    Ok(EXIT_WAITING)
                }
            }
        }
    });
    result.unwrap_or_else(|e| {
        let _ = writeln!(std::io::stderr(), "ask: {e}");
        1
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_questions_come_from_stdin_or_the_argument() {
        let q =
            r#"{"questions":[{"question":"Which?","header":"Pick","options":[{"label":"A"}],"multiSelect":false}]}"#;
        assert!(matches!(parse(&[], || q.to_owned()), Ok(Command::Open(_))));
        assert!(matches!(parse(&args(&[q]), String::new), Ok(Command::Open(_))));
        assert_eq!(
            parse(&args(&["wait", "q-1"]), String::new),
            Ok(Command::Wait("q-1".into()))
        );
        assert_eq!(
            parse(&args(&["status", "q-1"]), String::new),
            Ok(Command::Status("q-1".into()))
        );
        assert!(parse(&args(&["--help"]), String::new).is_err());
    }

    #[test]
    fn malformed_questions_are_refused_before_anything_is_asked() {
        assert!(questions("not json").is_err());
        assert!(questions(r#"{"questions":[]}"#).is_err());
        assert!(questions(r#"{"questions":[{"header":"x"}]}"#).is_err());
        assert!(questions(r#"{"questions":[{"question":"Q?","options":[{"description":"no label"}]}]}"#).is_err());
    }

    #[test]
    fn an_answer_prints_each_question_with_its_choice() {
        let a = json!({"behavior": "allow", "updatedInput": {
            "questions": [], "answers": {"Which colour?": "Blue"},
            "annotations": {"Which colour?": {"notes": "navy if possible"}}}});
        let (text, code) = render("q-1", &a);
        assert_eq!(code, 0);
        assert_eq!(
            text,
            "Which colour?\n→ Blue (answer to q-1)\nNotes on \"Which colour?\": navy if possible (answer to q-1)"
        );
        // every line that carries the answer carries its marker; the question's own line is not the answer
        for line in text.lines().filter(|l| l.starts_with('→') || l.starts_with("Notes")) {
            assert!(line.ends_with("(answer to q-1)"), "{line}");
        }
    }

    #[test]
    fn a_declined_question_exits_apart() {
        let (text, code) = render("q-1", &json!({"behavior": "deny", "message": "not now"}));
        assert_eq!(code, EXIT_DECLINED);
        assert_eq!(text, "The question was declined. not now (answer to q-1)");
    }
}
