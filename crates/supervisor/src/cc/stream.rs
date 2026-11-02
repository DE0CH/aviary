//! Claude Code's stdout (supervisor 7.1, 7.4): NDJSON in the generation's stdout file, read by tailing from a saved
//! offset, and the messages the driver uses. Only the fields the driver needs are decoded: a message's text, a tool
//! result's bytes and an image's base64 are skipped by the parser, never held as values.

use std::io::SeekFrom;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

use crate::limits::{STDOUT_PARSE_MAX, STDOUT_READ_BUFFER};

/// A `tool_use` block of a main-thread assistant message.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// Which task event (7.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskKind {
    Started,
    Updated,
    Notification,
    Progress,
}

/// A `system` task message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskMsg {
    pub kind: TaskKind,
    pub task_id: String,
    pub tool_use_id: Option<String>,
    pub task_type: Option<String>,
    pub description: Option<String>,
    pub workflow_name: Option<String>,
    /// `status`, or `patch.status` of a `task_updated`.
    pub status: Option<String>,
    pub summary: Option<String>,
    pub output_file: Option<String>,
}

/// A `result`: the end of one input's processing.
#[derive(Clone, Debug, PartialEq)]
pub struct ResultMsg {
    pub subtype: String,
    pub is_error: bool,
    pub result: Option<String>,
    pub usage: Option<magpie_api::sandbox::Usage>,
    pub duration_ms: Option<u64>,
}

/// One stdout message, as far as the driver needs it.
#[derive(Clone, Debug, PartialEq)]
pub enum Out {
    /// `system`/`init`.
    Init {
        session_id: Option<String>,
        model: Option<String>,
    },
    /// A complete `assistant` or `user` message.
    Message {
        assistant: bool,
        uuid: Option<String>,
        /// `parent_tool_use_id` is null: a main-thread message (written to the main transcript).
        main: bool,
        tool_uses: Vec<ToolUse>,
        /// The `<task-notification>…</task-notification>` text of a user message (a wake-up for a background task).
        task_notification: Option<String>,
    },
    /// `stream_event` (not produced in this release; activity all the same).
    StreamEvent,
    Task(TaskMsg),
    Result(ResultMsg),
    ControlRequest {
        request_id: String,
        subtype: String,
        request: Value,
    },
    ControlResponse {
        request_id: String,
        ok: bool,
        error: Option<String>,
    },
    ControlCancel {
        request_id: String,
    },
    KeepAlive,
    /// A `system` message the driver only logs (`api_retry`, `status`, `compact_boundary`, `hook_*`).
    Logged(String),
    /// A type the driver does not know (logged once per type).
    Other(String),
    /// Not JSON, or over the parse limit (scanned, not parsed).
    Unparsed,
}

impl Out {
    /// Output that opens an agent turn when nothing is pending (7.5): an `assistant`, `user` or `stream_event`.
    #[must_use]
    pub fn is_turn_output(&self) -> bool {
        matches!(self, Out::Message { .. } | Out::StreamEvent)
    }
}

#[derive(Deserialize)]
struct Patch {
    status: Option<String>,
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    name: Option<String>,
    input: Option<Value>,
}

/// `message.content`: a string (skipped) or blocks (only `tool_use` blocks kept).
#[derive(Default)]
struct Content(Vec<Block>);

impl<'de> Deserialize<'de> for Content {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Content;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a string or content blocks")
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Content, E> {
                Ok(Content::default())
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Content, E> {
                Ok(Content::default())
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Content, A::Error> {
                let mut out = Vec::new();
                while let Some(b) = seq.next_element::<Block>()? {
                    if b.kind.as_deref() == Some("tool_use") {
                        out.push(b);
                    }
                }
                Ok(Content(out))
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Deserialize)]
struct Msg {
    #[serde(default)]
    content: Content,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
    subtype: Option<String>,
    uuid: Option<String>,
    session_id: Option<String>,
    model: Option<String>,
    #[serde(default)]
    parent_tool_use_id: Option<String>,
    message: Option<Msg>,
    request_id: Option<String>,
    request: Option<Value>,
    response: Option<Value>,
    task_id: Option<String>,
    tool_use_id: Option<String>,
    description: Option<String>,
    task_type: Option<String>,
    workflow_name: Option<String>,
    status: Option<String>,
    patch: Option<Patch>,
    summary: Option<String>,
    output_file: Option<String>,
    #[serde(default)]
    is_error: bool,
    result: Option<String>,
    usage: Option<Value>,
    duration_ms: Option<u64>,
}

fn usage(v: &Value) -> magpie_api::sandbox::Usage {
    let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    magpie_api::sandbox::Usage {
        input_tokens: n("input_tokens"),
        output_tokens: n("output_tokens"),
        cache_read_input_tokens: n("cache_read_input_tokens"),
        cache_creation_input_tokens: n("cache_creation_input_tokens"),
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

/// The raw text from the first `<task-notification>` to the end of the last `</task-notification>` (at most 64 KiB).
fn notification(line: &[u8]) -> Option<String> {
    const OPEN: &[u8] = b"<task-notification>";
    const CLOSE: &[u8] = b"</task-notification>";
    let start = find(line, OPEN)?;
    let end = rfind(line, CLOSE).map_or(line.len(), |e| e + CLOSE.len()).max(start);
    let end = end.min(start + 64 * 1024);
    Some(String::from_utf8_lossy(&line[start..end]).into_owned())
}

/// Parses one stdout line (without its newline).
#[must_use]
pub fn parse(line: &[u8]) -> Out {
    let Ok(e) = serde_json::from_slice::<Envelope>(line) else {
        return Out::Unparsed;
    };
    match e.kind.as_str() {
        "assistant" | "user" => Out::Message {
            assistant: e.kind == "assistant",
            uuid: e.uuid,
            main: e.parent_tool_use_id.is_none(),
            tool_uses: e
                .message
                .map(|m| m.content.0)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|b| {
                    Some(ToolUse {
                        id: b.id?,
                        name: b.name?,
                        input: b.input.unwrap_or(Value::Null),
                    })
                })
                .collect(),
            task_notification: if e.kind == "user" { notification(line) } else { None },
        },
        "stream_event" => Out::StreamEvent,
        "keep_alive" => Out::KeepAlive,
        "result" => Out::Result(ResultMsg {
            subtype: e.subtype.unwrap_or_default(),
            is_error: e.is_error,
            result: e.result,
            usage: e.usage.as_ref().map(usage),
            duration_ms: e.duration_ms,
        }),
        "control_request" => {
            let subtype = e
                .request
                .as_ref()
                .and_then(|r| r.get("subtype"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            Out::ControlRequest {
                request_id: e.request_id.unwrap_or_default(),
                subtype,
                request: e.request.unwrap_or(Value::Null),
            }
        }
        "control_response" => {
            let r = e.response.unwrap_or(Value::Null);
            let s = |k: &str| r.get(k).and_then(Value::as_str).map(str::to_owned);
            Out::ControlResponse {
                request_id: s("request_id").unwrap_or_default(),
                ok: s("subtype").as_deref() == Some("success"),
                error: s("error"),
            }
        }
        "control_cancel_request" => Out::ControlCancel {
            request_id: e.request_id.unwrap_or_default(),
        },
        "system" => {
            let sub = e.subtype.unwrap_or_default();
            let kind = match sub.as_str() {
                "init" => {
                    return Out::Init {
                        session_id: e.session_id,
                        model: e.model,
                    };
                }
                "task_started" => TaskKind::Started,
                "task_updated" => TaskKind::Updated,
                "task_notification" => TaskKind::Notification,
                "task_progress" => TaskKind::Progress,
                _ => return Out::Logged(sub),
            };
            let Some(task_id) = e.task_id else {
                return Out::Logged(sub);
            };
            Out::Task(TaskMsg {
                kind,
                task_id,
                tool_use_id: e.tool_use_id,
                task_type: e.task_type,
                description: e.description,
                workflow_name: e.workflow_name,
                status: e.status.or_else(|| e.patch.and_then(|p| p.status)),
                summary: e.summary,
                output_file: e.output_file,
            })
        }
        other => Out::Other(other.to_owned()),
    }
}

/// One line read from the file: the parsed message and the offset just past its newline.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub out: Out,
    pub end: u64,
}

/// A tail of a generation's stdout file from an offset. Only complete lines are returned; a partial last line
/// stays unread until its newline arrives. A line longer than [`STDOUT_PARSE_MAX`] is scanned to its end and
/// returned as [`Out::Unparsed`].
#[derive(Debug)]
pub struct Tail {
    path: PathBuf,
    /// The offset of the first byte not yet returned (the start of `buf`, or of the skipped line).
    offset: u64,
    /// Bytes read past `offset` that do not yet end a line.
    buf: Vec<u8>,
    /// Inside a line over the parse limit: bytes are dropped until its newline.
    skipping: bool,
    /// The read position (`offset + buf.len()` unless skipping).
    read_at: u64,
}

impl Tail {
    #[must_use]
    pub fn new(path: PathBuf, offset: u64) -> Self {
        Self {
            path,
            offset,
            buf: Vec::new(),
            skipping: false,
            read_at: offset,
        }
    }

    /// Where the next returned line starts.
    #[must_use]
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Reads what the file holds now and returns every complete line.
    ///
    /// # Errors
    /// The file's read error (a missing file reads as empty).
    pub async fn read(&mut self) -> std::io::Result<Vec<Line>> {
        let mut f = match tokio::fs::File::open(&self.path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        f.seek(SeekFrom::Start(self.read_at)).await?;
        let mut out = Vec::new();
        let mut chunk = vec![0u8; STDOUT_READ_BUFFER];
        loop {
            let n = f.read(&mut chunk).await?;
            if n == 0 {
                return Ok(out);
            }
            self.read_at += n as u64;
            let mut rest = &chunk[..n];
            while !rest.is_empty() {
                if let Some(i) = rest.iter().position(|b| *b == b'\n') {
                    {
                        let end = self.read_at - (rest.len() - i - 1) as u64;
                        if self.skipping {
                            self.skipping = false;
                            out.push(Line {
                                out: Out::Unparsed,
                                end,
                            });
                        } else {
                            self.buf.extend_from_slice(&rest[..i]);
                            let line = std::mem::take(&mut self.buf);
                            if !line.iter().all(u8::is_ascii_whitespace) {
                                out.push(Line { out: parse(&line), end });
                            }
                        }
                        self.offset = end;
                        rest = &rest[i + 1..];
                    }
                } else {
                    {
                        if !self.skipping {
                            self.buf.extend_from_slice(rest);
                            if self.buf.len() > STDOUT_PARSE_MAX {
                                self.skipping = true;
                                self.buf = Vec::new();
                            }
                        }
                        rest = &[];
                    }
                }
            }
            if self.buf.capacity() > 2 * STDOUT_READ_BUFFER && self.buf.is_empty() {
                self.buf = Vec::new();
            }
        }
    }
}

/// The backstop poll of a tail (7.1): inotify wakes it at once, this catches anything inotify missed.
pub const TAIL_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// Wakes a tail when its file changes: inotify on the file (`IN_MODIFY`), plus the 250 ms poll as a backstop (and
/// alone when inotify is unavailable).
#[derive(Debug)]
pub struct Watch {
    fd: Option<tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>>,
}

impl Watch {
    #[must_use]
    pub fn new(path: &std::path::Path) -> Self {
        use std::os::fd::FromRawFd as _;
        use std::os::unix::ffi::OsStrExt as _;
        let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return Self { fd: None };
        };
        // SAFETY: inotify_init1 returns a new fd or -1; the path is a valid C string.
        #[allow(unsafe_code)]
        let fd = unsafe {
            let fd = libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC);
            if fd < 0 {
                return Self { fd: None };
            }
            let fd = std::os::fd::OwnedFd::from_raw_fd(fd);
            if libc::inotify_add_watch(std::os::fd::AsRawFd::as_raw_fd(&fd), c.as_ptr(), libc::IN_MODIFY) < 0 {
                return Self { fd: None };
            }
            fd
        };
        // SAFETY: the AsyncFd owns the fd, so it stays open for as long as it is registered.
        #[allow(unsafe_code)]
        let fd = unsafe { tokio::io::unix::AsyncFd::register(fd) }.ok();
        Self { fd }
    }

    /// Returns when the file may have grown (or after [`TAIL_POLL`]).
    pub async fn wait(&mut self) {
        let Some(a) = self.fd.as_ref() else {
            tokio::time::sleep(TAIL_POLL).await;
            return;
        };
        tokio::select! {
            r = a.readable() => {
                if let Ok(mut g) = r {
                    let mut buf = [0u8; 4096];
                    loop {
                        // SAFETY: reading into a local buffer from our own non-blocking fd.
                        #[allow(unsafe_code)]
                        let n = unsafe {
                            libc::read(std::os::fd::AsRawFd::as_raw_fd(a.get_ref()), buf.as_mut_ptr().cast(), buf.len())
                        };
                        if n <= 0 {
                            break;
                        }
                    }
                    g.clear_ready();
                }
            }
            () = tokio::time::sleep(TAIL_POLL) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages() {
        let a = parse(br#"{"type":"assistant","uuid":"u1","parent_tool_use_id":null,"message":{"role":"assistant","content":[{"type":"text","text":"hi"},{"type":"tool_use","id":"toolu_1","name":"ScheduleWakeup","input":{"delaySeconds":60}}]}}"#);
        let Out::Message {
            assistant,
            uuid,
            main,
            tool_uses,
            task_notification,
        } = a
        else {
            panic!("{a:?}")
        };
        assert!(assistant && main && task_notification.is_none());
        assert_eq!(uuid.as_deref(), Some("u1"));
        assert_eq!(tool_uses[0].name, "ScheduleWakeup");
        assert_eq!(tool_uses[0].input["delaySeconds"], 60);
        let sub =
            parse(br#"{"type":"assistant","uuid":"u2","parent_tool_use_id":"toolu_9","message":{"content":"x"}}"#);
        assert!(matches!(sub, Out::Message { main: false, .. }));
        let wake = parse(br#"{"type":"user","uuid":"u3","message":{"content":"<task-notification><tool-use-id>toolu_1</tool-use-id></task-notification>"}}"#);
        assert!(
            matches!(wake, Out::Message { task_notification: Some(t), assistant: false, .. } if t.contains("toolu_1"))
        );
        let big = format!(
            r#"{{"type":"user","uuid":"u4","message":{{"content":[{{"type":"tool_result","tool_use_id":"t","content":[{{"type":"image","source":{{"type":"base64","media_type":"image/png","data":"{}"}}}}]}}]}}}}"#,
            "A".repeat(100_000)
        );
        assert!(matches!(parse(big.as_bytes()), Out::Message { tool_uses, .. } if tool_uses.is_empty()));
    }

    #[test]
    fn system_result_control() {
        assert_eq!(
            parse(br#"{"type":"system","subtype":"init","session_id":"s","model":"m","tools":[]}"#),
            Out::Init {
                session_id: Some("s".into()),
                model: Some("m".into())
            }
        );
        let Out::Task(t) =
            parse(br#"{"type":"system","subtype":"task_updated","task_id":"t1","patch":{"status":"killed"}}"#)
        else {
            panic!()
        };
        assert_eq!((t.kind, t.status.as_deref()), (TaskKind::Updated, Some("killed")));
        let Out::Result(r) = parse(br#"{"type":"result","subtype":"success","is_error":false,"result":"done","duration_ms":12,"usage":{"input_tokens":3,"output_tokens":4}}"#) else {
            panic!()
        };
        assert_eq!(r.usage.unwrap().output_tokens, 4);
        assert_eq!(r.duration_ms, Some(12));
        assert!(matches!(
            parse(br#"{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"X"}}"#),
            Out::ControlRequest { subtype, .. } if subtype == "can_use_tool"
        ));
        assert_eq!(
            parse(
                br#"{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{}}}"#
            ),
            Out::ControlResponse {
                request_id: "init-1".into(),
                ok: true,
                error: None
            }
        );
        assert_eq!(parse(br#"{"type":"keep_alive"}"#), Out::KeepAlive);
        assert_eq!(
            parse(br#"{"type":"rate_limit_event"}"#),
            Out::Other("rate_limit_event".into())
        );
        assert_eq!(parse(b"not json"), Out::Unparsed);
        assert_eq!(
            parse(br#"{"type":"system","subtype":"api_retry"}"#),
            Out::Logged("api_retry".into())
        );
    }

    #[tokio::test]
    async fn tail_returns_whole_lines_from_an_offset() {
        use std::io::Write as _;
        let p = std::env::temp_dir().join(format!("tail-{}", uuid::Uuid::new_v4().simple()));
        let mut f = std::fs::File::create(&p).unwrap();
        let mut t = Tail::new(p.clone(), 0);
        assert_eq!(t.read().await.unwrap(), Vec::new());
        f.write_all(b"{\"type\":\"keep_alive\"}\n{\"type\":\"res").unwrap();
        let l = t.read().await.unwrap();
        assert_eq!(
            l,
            vec![Line {
                out: Out::KeepAlive,
                end: 22
            }]
        );
        f.write_all(b"ult\",\"subtype\":\"success\"}\n").unwrap();
        let l = t.read().await.unwrap();
        assert!(matches!(l[0].out, Out::Result(_)));
        let end = l[0].end;
        assert_eq!(end, std::fs::metadata(&p).unwrap().len());
        // a new tail from the saved offset sees nothing old
        let mut t2 = Tail::new(p.clone(), 22);
        assert_eq!(t2.read().await.unwrap().len(), 1);
        let _ = std::fs::remove_file(p);
    }
}
