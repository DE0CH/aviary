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

/// A `stream_event`: one step of the model's streamed answer (the Messages API's stream events), on the main thread
/// or a subagent's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Partial {
    /// The subagent's `Agent` call, when the event is of a subagent's thread.
    pub parent_tool_use_id: Option<String>,
    pub step: Step,
}

/// What a `stream_event` says, as far as live frames need it (magpie 7.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// `message_start`: the message whose blocks follow.
    Message { id: String },
    /// `content_block_start`.
    Start {
        index: u32,
        block: magpie_api::live::BlockType,
        /// A `tool_use` block's tool.
        name: Option<String>,
    },
    /// `content_block_delta` with text (`text_delta`) or thinking (`thinking_delta`).
    Text { index: u32, text: String },
    /// `content_block_stop`.
    Stop { index: u32 },
    /// Anything else (a tool input's JSON, a signature, `message_delta`, `message_stop`).
    Other,
}

fn partial(parent_tool_use_id: Option<String>, event: Option<&Value>) -> Out {
    Out::StreamEvent(Partial {
        parent_tool_use_id,
        step: event.map_or(Step::Other, step),
    })
}

fn step(ev: &Value) -> Step {
    use magpie_api::live::BlockType;
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let index = || {
        ev.get("index")
            .and_then(Value::as_u64)
            .and_then(|i| u32::try_from(i).ok())
    };
    match ev.get("type").and_then(Value::as_str) {
        Some("message_start") => ev
            .get("message")
            .and_then(|m| s(m, "id"))
            .map_or(Step::Other, |id| Step::Message { id }),
        Some("content_block_start") => {
            let (Some(index), Some(b)) = (index(), ev.get("content_block")) else {
                return Step::Other;
            };
            let block = match b.get("type").and_then(Value::as_str) {
                Some("text") => BlockType::Text,
                Some("thinking") => BlockType::Thinking,
                Some("tool_use") => BlockType::ToolUse,
                _ => return Step::Other,
            };
            let name = if block == BlockType::ToolUse {
                s(b, "name")
            } else {
                None
            };
            Step::Start { index, block, name }
        }
        Some("content_block_delta") => {
            let (Some(index), Some(d)) = (index(), ev.get("delta")) else {
                return Step::Other;
            };
            let text = match d.get("type").and_then(Value::as_str) {
                Some("text_delta") => s(d, "text"),
                Some("thinking_delta") => s(d, "thinking"),
                _ => None,
            };
            match text {
                Some(text) if !text.is_empty() => Step::Text { index, text },
                _ => Step::Other,
            }
        }
        Some("content_block_stop") => index().map_or(Step::Other, |index| Step::Stop { index }),
        _ => Step::Other,
    }
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
        /// The `tool_use_id` of each `tool_result` block (a user message): the calls that finished.
        tool_results: Vec<String>,
        /// The `<task-notification>…</task-notification>` text of a user message (a wake-up for a background task).
        task_notification: Option<String>,
        /// The image blocks the message carries, its tool results' included: what it adds to the images Claude Code
        /// sends with every model call from now on.
        images: u32,
    },
    /// `stream_event` (Claude Code with `--include-partial-messages`): the model's output as it comes.
    StreamEvent(Partial),
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
        /// The slash commands an `initialize` answer names (`response.commands`); `None` on any other answer.
        commands: Option<Vec<magpie_api::sandbox::SlashCommand>>,
    },
    ControlCancel {
        request_id: String,
    },
    KeepAlive,
    /// `system`/`compact_boundary`: Claude Code replaced the conversation so far by a summary (its images are gone
    /// from the context).
    Compacted,
    /// A `system` message the driver only logs (`api_retry`, `status`, `hook_*`).
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
        matches!(self, Out::Message { .. } | Out::StreamEvent(_))
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
    tool_use_id: Option<String>,
    /// A `tool_result`'s own content: only its image blocks are counted.
    #[serde(default)]
    content: Images,
}

/// How many `image` blocks a content holds: a string or anything but a list has none. The blocks' other members
/// (the base64 data) are skipped, never kept.
#[derive(Default)]
struct Images(u32);

impl<'de> Deserialize<'de> for Images {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Kind {
            #[serde(rename = "type")]
            kind: Option<String>,
        }
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Images;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("any content")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Images, A::Error> {
                let mut n = 0;
                // a block that is no object (a bare string) counts as none
                while let Some(b) = seq.next_element::<Value0<Kind>>()? {
                    if b.0.is_some_and(|k| k.kind.as_deref() == Some("image")) {
                        n += 1;
                    }
                }
                Ok(Images(n))
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Images, E> {
                Ok(Images(0))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Images, E> {
                Ok(Images(0))
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Images, E> {
                Ok(Images(0))
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Images, E> {
                Ok(Images(0))
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Images, E> {
                Ok(Images(0))
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Images, E> {
                Ok(Images(0))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Images, A::Error> {
                while map
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(Images(0))
            }
        }
        d.deserialize_any(V)
    }
}

/// `T` when the value is an object that reads as one, nothing otherwise (the value is consumed either way).
struct Value0<T>(Option<T>);

impl<'de, T: serde::de::DeserializeOwned> Deserialize<'de> for Value0<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(std::marker::PhantomData<T>);
        impl<'de, T: serde::de::DeserializeOwned> serde::de::Visitor<'de> for V<T> {
            type Value = Value0<T>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("any value")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Value0<T>, A::Error> {
                T::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(|t| Value0(Some(t)))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Value0<T>, A::Error> {
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(Value0(None))
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Value0<T>, E> {
                Ok(Value0(None))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Value0<T>, E> {
                Ok(Value0(None))
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Value0<T>, E> {
                Ok(Value0(None))
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Value0<T>, E> {
                Ok(Value0(None))
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Value0<T>, E> {
                Ok(Value0(None))
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Value0<T>, E> {
                Ok(Value0(None))
            }
        }
        d.deserialize_any(V(std::marker::PhantomData))
    }
}

/// `message.content`: a string (skipped) or blocks (only `tool_use` and `tool_result` blocks kept, the image blocks
/// counted: the message's own and those inside its tool results).
#[derive(Default)]
struct Content {
    blocks: Vec<Block>,
    images: u32,
}

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
                let mut out = Content::default();
                while let Some(b) = seq.next_element::<Block>()? {
                    match b.kind.as_deref() {
                        Some("image") => out.images += 1,
                        Some("tool_result") => {
                            out.images += b.content.0;
                            out.blocks.push(b);
                        }
                        Some("tool_use") => out.blocks.push(b),
                        _ => {}
                    }
                }
                Ok(out)
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
    /// A transcript line of a subagent's thread (stdout marks those by `parent_tool_use_id`).
    #[serde(default, rename = "isSidechain")]
    is_sidechain: bool,
    message: Option<Msg>,
    request_id: Option<String>,
    request: Option<Value>,
    response: Option<Value>,
    /// A `stream_event`'s Messages API event.
    event: Option<Value>,
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
    /// A `result`'s usage per model (`{"<model>": {"inputTokens": …, "contextWindow": …}}`).
    #[serde(default, rename = "modelUsage")]
    model_usage: Option<Value>,
    duration_ms: Option<u64>,
}

fn usage(v: &Value, per_model: Option<&Value>) -> magpie_api::sandbox::Usage {
    let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    magpie_api::sandbox::Usage {
        input_tokens: n("input_tokens"),
        output_tokens: n("output_tokens"),
        cache_read_input_tokens: n("cache_read_input_tokens"),
        cache_creation_input_tokens: n("cache_creation_input_tokens"),
        context_window: per_model.and_then(context_window),
    }
}

/// The context window of the conversation's model in a `result`'s `modelUsage`: Claude Code states each model's
/// `contextWindow` there, the small model's (titles, summaries) included, so the conversation's is the entry that
/// read the most input. `None` when no entry names a window.
fn context_window(per_model: &Value) -> Option<u64> {
    let n = |m: &Value, k: &str| m.get(k).and_then(Value::as_u64).unwrap_or(0);
    per_model
        .as_object()?
        .values()
        .filter(|m| n(m, "contextWindow") > 0)
        .max_by_key(|m| n(m, "inputTokens") + n(m, "cacheReadInputTokens") + n(m, "cacheCreationInputTokens"))
        .map(|m| n(m, "contextWindow"))
}

/// A `control_response` line: whose answer it is, whether it succeeded, and what an `initialize` answer names.
fn control_response(r: &Value) -> Out {
    let s = |k: &str| r.get(k).and_then(Value::as_str).map(str::to_owned);
    Out::ControlResponse {
        request_id: s("request_id").unwrap_or_default(),
        ok: s("subtype").as_deref() == Some("success"),
        error: s("error"),
        commands: r
            .get("response")
            .and_then(|x| x.get("commands"))
            .and_then(Value::as_array)
            .map(|a| slash_commands(a)),
    }
}

/// The commands of an `initialize` answer, as magpie keeps them: named ones, at most
/// [`magpie_api::sandbox::COMMANDS_MAX`], each description cut to its first line and
/// [`magpie_api::sandbox::COMMAND_DESCRIPTION_MAX`] characters (a skill's is a paragraph).
fn slash_commands(list: &[Value]) -> Vec<magpie_api::sandbox::SlashCommand> {
    use magpie_api::sandbox::{COMMAND_DESCRIPTION_MAX, COMMANDS_MAX, SlashCommand};
    let text = |c: &Value, k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").trim().to_owned();
    list.iter()
        .filter_map(|c| {
            let name = text(c, "name");
            if name.is_empty() || name.chars().count() > 200 {
                return None;
            }
            let description = text(c, "description");
            Some(SlashCommand {
                name,
                description: description
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(COMMAND_DESCRIPTION_MAX)
                    .collect(),
                argument_hint: text(c, "argumentHint").chars().take(200).collect(),
            })
        })
        .take(COMMANDS_MAX)
        .collect()
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
        "assistant" | "user" => {
            let content = e.message.map(|m| m.content).unwrap_or_default();
            let images = content.images;
            let (uses, results): (Vec<Block>, Vec<Block>) = content
                .blocks
                .into_iter()
                .partition(|b| b.kind.as_deref() == Some("tool_use"));
            Out::Message {
                assistant: e.kind == "assistant",
                uuid: e.uuid,
                main: e.parent_tool_use_id.is_none() && !e.is_sidechain,
                tool_uses: uses
                    .into_iter()
                    .filter_map(|b| {
                        Some(ToolUse {
                            id: b.id?,
                            name: b.name?,
                            input: b.input.unwrap_or(Value::Null),
                        })
                    })
                    .collect(),
                tool_results: results.into_iter().filter_map(|b| b.tool_use_id).collect(),
                task_notification: if e.kind == "user" { notification(line) } else { None },
                images,
            }
        }
        "stream_event" => partial(e.parent_tool_use_id, e.event.as_ref()),
        "keep_alive" => Out::KeepAlive,
        "result" => Out::Result(ResultMsg {
            subtype: e.subtype.unwrap_or_default(),
            is_error: e.is_error,
            result: e.result,
            usage: e.usage.as_ref().map(|u| usage(u, e.model_usage.as_ref())),
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
        "control_response" => control_response(&e.response.unwrap_or(Value::Null)),
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
                "compact_boundary" => return Out::Compacted,
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

    /// A `result` names the window Claude Code runs each model at; the conversation's is the one that read the most
    /// input (the small model's entry is there too).
    #[test]
    fn a_result_names_the_conversations_context_window() {
        let Out::Result(r) = parse(br#"{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"input_tokens":3,"output_tokens":4}}"#) else {
            panic!()
        };
        assert_eq!(
            r.usage.unwrap().context_window,
            None,
            "a result that names no model's window"
        );
        // the window of the conversation's model: the entry that read the most input, not the small model's
        let Out::Result(r) = parse(br#"{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"input_tokens":3,"output_tokens":4},"modelUsage":{"claude-haiku-5-5":{"inputTokens":900,"outputTokens":20,"cacheReadInputTokens":0,"cacheCreationInputTokens":0,"contextWindow":200000},"z-ai/glm-5.3":{"inputTokens":10,"outputTokens":400,"cacheReadInputTokens":52000,"cacheCreationInputTokens":3000,"contextWindow":1000000}}}"#) else {
            panic!()
        };
        assert_eq!(r.usage.unwrap().context_window, Some(1_000_000));
    }

    #[test]
    fn messages() {
        let a = parse(br#"{"type":"assistant","uuid":"u1","parent_tool_use_id":null,"message":{"role":"assistant","content":[{"type":"text","text":"hi"},{"type":"tool_use","id":"toolu_1","name":"ScheduleWakeup","input":{"delaySeconds":60}}]}}"#);
        let Out::Message {
            assistant,
            uuid,
            main,
            tool_uses,
            tool_results,
            task_notification,
            images: _,
        } = a
        else {
            panic!("{a:?}")
        };
        assert!(assistant && main && task_notification.is_none() && tool_results.is_empty());
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
        assert!(
            matches!(parse(big.as_bytes()), Out::Message { tool_uses, tool_results, images: 1, .. } if tool_uses.is_empty() && tool_results == ["t"])
        );
        // images are counted wherever a message carries them: its own blocks and each tool result's, not text that
        // merely looks like one, not an assistant's tool input
        let many = br#"{"type":"user","uuid":"u5","message":{"content":[{"type":"image","source":{"data":"QQ=="}},{"type":"text","text":"{\"type\":\"image\"}"},{"type":"tool_result","tool_use_id":"a","content":[{"type":"text","text":"x"},{"type":"image","source":{}},{"type":"image","source":{}},"odd",7]},{"type":"tool_result","tool_use_id":"b","content":"plain"},{"type":"tool_result","tool_use_id":"c"}]}}"#;
        assert!(matches!(parse(many), Out::Message { images: 3, tool_results, .. } if tool_results == ["a", "b", "c"]));
        let none = br#"{"type":"assistant","uuid":"u6","message":{"content":[{"type":"tool_use","id":"t","name":"X","input":{"content":[{"type":"image"}]}}]}}"#;
        assert!(matches!(parse(none), Out::Message { images: 0, .. }));
        assert_eq!(
            parse(br#"{"type":"system","subtype":"compact_boundary","compact_metadata":{"trigger":"manual"}}"#),
            Out::Compacted
        );
    }

    #[test]
    fn an_initialize_answer_names_the_slash_commands() {
        // an initialize answer names Claude Code's slash commands: a skill's paragraph is cut to its first line
        let Out::ControlResponse { commands, .. } = parse(
            br#"{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"commands":[{"name":"compact","description":"Clear conversation history but keep a summary in context","argumentHint":"<optional instructions>","builtin":true},{"name":"dataviz","description":"Use this skill for any chart.\nRead it first."},{"name":"","description":"no name"},{"description":"no name either"}],"models":[]}}}"#,
        ) else {
            panic!()
        };
        let commands = commands.unwrap();
        assert_eq!(
            commands
                .iter()
                .map(|c| (c.name.as_str(), c.description.as_str(), c.argument_hint.as_str()))
                .collect::<Vec<_>>(),
            [
                (
                    "compact",
                    "Clear conversation history but keep a summary in context",
                    "<optional instructions>"
                ),
                ("dataviz", "Use this skill for any chart.", ""),
            ]
        );
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
                error: None,
                commands: None,
            }
        );
        assert_eq!(parse(br#"{"type":"keep_alive"}"#), Out::KeepAlive);
        // the model's answer as it comes (Claude Code 2.1.322 with --include-partial-messages)
        let partial = |line: &str| match parse(line.as_bytes()) {
            Out::StreamEvent(p) => p,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            partial(
                r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"msg_1","content":[]}},"parent_tool_use_id":null,"session_id":"s","uuid":"u"}"#
            ),
            Partial {
                parent_tool_use_id: None,
                step: Step::Message { id: "msg_1".into() }
            }
        );
        assert_eq!(
            partial(r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#).step,
            Step::Start { index: 0, block: magpie_api::live::BlockType::Text, name: None }
        );
        assert_eq!(
            partial(
                r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"Bash","input":{}}},"parent_tool_use_id":"toolu_agent"}"#
            ),
            Partial {
                parent_tool_use_id: Some("toolu_agent".into()),
                step: Step::Start {
                    index: 1,
                    block: magpie_api::live::BlockType::ToolUse,
                    name: Some("Bash".into())
                }
            }
        );
        assert_eq!(
            partial(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Done"}}}"#).step,
            Step::Text { index: 0, text: "Done".into() }
        );
        assert_eq!(
            partial(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":2,"delta":{"type":"thinking_delta","thinking":"hm"}}}"#).step,
            Step::Text { index: 2, text: "hm".into() }
        );
        assert_eq!(
            partial(r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#).step,
            Step::Stop { index: 0 }
        );
        // a tool input's JSON, the message's end, an event with no body: nothing to show
        for other in [
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"a"}}}"#,
            r#"{"type":"stream_event","event":{"type":"message_stop"}}"#,
            r#"{"type":"stream_event"}"#,
        ] {
            assert_eq!(partial(other).step, Step::Other, "{other}");
        }
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
