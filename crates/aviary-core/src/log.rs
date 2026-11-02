//! Logs: one JSON object per line (platform 15.1), written by a `tracing` layer.
//!
//! Every line starts with the fixed fields `t` (RFC 3339 UTC, milliseconds), `level`, `svc`, `ver` and (in a Pod)
//! `pod`, then `msg` (the event's message), then the fields of the enclosing spans (outermost first) and of the
//! event. Every string value passes through [`redact`] inside the layer, so no call site can forget it, except the
//! typed fields of [`TYPED_FIELDS`] (ids, the route template, status, durations), which are written as they are.
//!
//! Levels: `error`, `warn` and `info`; there is no level switch (verbosity is not deployment configuration).
//! Identity in logs is [`person_key`], never an email (platform 3.8).

use std::fmt::{self, Write as _};
use std::io::Write as _;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use ring::digest;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Context, SubscriberExt as _};
use tracing_subscriber::registry::LookupSpan;

use crate::version::{self, Build};

/// Fields written without [`redact`]: typed ids (`request`, `conversation`, `turn`, `sandbox`), `person_key`, the
/// route template, the HTTP status and durations. Callers put only such values in them.
pub const TYPED_FIELDS: &[&str] = &[
    "request",
    "conversation",
    "turn",
    "sandbox",
    "person_key",
    "route",
    "status",
    "ms",
];

/// Fixed fields a call site cannot override.
const FIXED_FIELDS: &[&str] = &["t", "level", "svc", "ver", "pod", "msg"];

/// Where log lines go: stdout for services, stderr for the supervisor (its stderr is the sandbox console).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// A destination for whole log lines.
pub trait Sink: Send + Sync + 'static {
    /// Writes one line (with its newline) in one piece.
    fn write_line(&self, line: &[u8]);
}

impl Sink for Output {
    fn write_line(&self, line: &[u8]) {
        // one write per line under the stream's lock; a failed write has nowhere to be reported
        match self {
            Self::Stdout => {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(line);
                let _ = out.flush();
            }
            Self::Stderr => {
                let _ = std::io::stderr().lock().write_all(line);
            }
        }
    }
}

/// An in-memory sink for tests: collects the lines.
#[derive(Debug, Clone, Default)]
pub struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    /// An empty capture.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The lines written so far, each parsed as JSON.
    ///
    /// # Panics
    /// When a line is not JSON (which the layer never writes).
    #[must_use]
    pub fn lines(&self) -> Vec<serde_json::Value> {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
        String::from_utf8_lossy(&bytes)
            .lines()
            .map(|l| {
                #[allow(clippy::expect_used)] // the layer writes only JSON lines
                serde_json::from_str(l).expect("a log line is JSON")
            })
            .collect()
    }

    /// The raw text written so far.
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
    }
}

impl Sink for Capture {
    fn write_line(&self, line: &[u8]) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(line);
    }
}

/// The service name and Pod of this process once [`init`] (or [`init_in_pod`]) has named them, for lines written
/// before or without the layer (the start failure line of [`crate::env`]).
static SVC: OnceLock<String> = OnceLock::new();
static POD: OnceLock<String> = OnceLock::new();

/// The service name set by [`init`], if any.
pub(crate) fn svc_name() -> Option<&'static str> {
    SVC.get().map(String::as_str)
}

/// The Pod name set by [`init`], if any.
pub(crate) fn pod_name() -> Option<&'static str> {
    POD.get().map(String::as_str)
}

/// Installs the JSON layer (level `info` and above) as the global subscriber for service `build`.
/// A second call is ignored.
pub fn init(build: &Build, pod: Option<String>, output: Output) {
    let _ = SVC.set(build.name().to_owned());
    if let Some(p) = &pod {
        let _ = POD.set(p.clone());
    }
    let layer = JsonLayer::new(build.name(), version::sha12(), pod, output);
    let subscriber = tracing_subscriber::registry().with(layer.with_filter(LevelFilter::INFO));
    let _ = tracing::subscriber::set_global_default(subscriber);
}

/// [`init`] for a service in a Pod: names the service first, then reads `POD_NAME` (the downward API) with
/// [`crate::env::field`], so a missing `POD_NAME` aborts with a line that already carries `svc` and `ver`.
pub fn init_in_pod(build: &Build, output: Output) {
    let _ = SVC.set(build.name().to_owned());
    let pod = crate::env::field("POD_NAME");
    init(build, Some(pod), output);
}

/// A subscriber writing to `sink` at level `info` and above, for tests (`tracing::subscriber::with_default`).
#[must_use]
pub fn subscriber(svc: &str, pod: Option<String>, sink: impl Sink) -> impl Subscriber + Send + Sync + 'static {
    tracing_subscriber::registry().with(JsonLayer::new(svc, version::sha12(), pod, sink).with_filter(LevelFilter::INFO))
}

/// The `tracing` layer writing the lines of 15.1.
pub struct JsonLayer<W> {
    svc: String,
    ver: String,
    pod: Option<String>,
    sink: W,
}

impl<W> fmt::Debug for JsonLayer<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JsonLayer")
            .field("svc", &self.svc)
            .field("ver", &self.ver)
            .finish_non_exhaustive()
    }
}

impl<W: Sink> JsonLayer<W> {
    /// A layer for service `svc` at version `ver`, in Pod `pod`, writing to `sink`.
    pub fn new(svc: &str, ver: &str, pod: Option<String>, sink: W) -> Self {
        Self {
            svc: svc.to_owned(),
            ver: ver.to_owned(),
            pod,
            sink,
        }
    }
}

/// Field values collected from a span or an event, in order, already redacted.
#[derive(Debug, Default, Clone)]
struct Fields(Vec<(&'static str, serde_json::Value)>);

impl Fields {
    fn put(&mut self, name: &'static str, value: serde_json::Value) {
        if let Some(slot) = self.0.iter_mut().find(|(n, _)| *n == name) {
            slot.1 = value;
        } else {
            self.0.push((name, value));
        }
    }

    fn put_str(&mut self, name: &'static str, s: &str) {
        let v = if TYPED_FIELDS.contains(&name) {
            s.to_owned()
        } else {
            redact(s)
        };
        self.put(name, serde_json::Value::String(v));
    }
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put_str(field.name(), value);
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let mut s = String::new();
        let _ = write!(s, "{value:?}");
        self.put_str(field.name(), &s);
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        // the error and its causes, as one string: library errors put the useful part in a source
        let mut s = value.to_string();
        let mut source = value.source();
        while let Some(cause) = source {
            let _ = write!(s, ": {cause}");
            source = cause.source();
        }
        self.put_str(field.name(), &s);
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field.name(), value.into());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field.name(), value.into());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(field.name(), value.into());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field.name(), value.into());
    }
}

impl<S, W> Layer<S> for JsonLayer<W>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: Sink,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(fields);
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let mut ext = span.extensions_mut();
            if let Some(fields) = ext.get_mut::<Fields>() {
                values.record(fields);
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut all = Fields::default();
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                if let Some(f) = span.extensions().get::<Fields>() {
                    for (k, v) in &f.0 {
                        all.put(k, v.clone());
                    }
                }
            }
        }
        let mut msg = serde_json::Value::String(String::new());
        for (k, v) in fields.0 {
            if k == "message" {
                msg = v;
            } else {
                all.put(k, v);
            }
        }
        let mut line = String::with_capacity(256);
        line.push('{');
        push_pair(&mut line, "t", &serde_json::Value::String(now()));
        push_pair(
            &mut line,
            "level",
            &serde_json::Value::String(level_name(*event.metadata().level()).to_owned()),
        );
        push_pair(&mut line, "svc", &serde_json::Value::String(self.svc.clone()));
        push_pair(&mut line, "ver", &serde_json::Value::String(self.ver.clone()));
        if let Some(pod) = &self.pod {
            push_pair(&mut line, "pod", &serde_json::Value::String(pod.clone()));
        }
        push_pair(&mut line, "msg", &msg);
        for (k, v) in &all.0 {
            if !FIXED_FIELDS.contains(k) {
                push_pair(&mut line, k, v);
            }
        }
        line.push_str("}\n");
        self.sink.write_line(line.as_bytes());
    }
}

fn push_pair(line: &mut String, key: &str, value: &serde_json::Value) {
    if !line.ends_with('{') {
        line.push(',');
    }
    // serialising a str or a Value cannot fail
    line.push_str(&serde_json::to_string(key).unwrap_or_default());
    line.push(':');
    line.push_str(&serde_json::to_string(value).unwrap_or_default());
}

fn level_name(level: Level) -> &'static str {
    match level {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}

/// `t` for a time: RFC 3339 UTC with milliseconds, `2026-11-02T04:05:06.789Z`.
#[must_use]
pub fn rfc3339(t: time::OffsetDateTime) -> String {
    let t = t.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.millisecond()
    )
}

/// The current time as [`rfc3339`].
#[must_use]
pub fn now() -> String {
    rfc3339(time::OffsetDateTime::now_utc())
}

/// An email as compared everywhere: trimmed and lowercased (platform 3.6).
#[must_use]
pub fn normalise_email(email: &str) -> String {
    email.trim().to_lowercase()
}

/// The log identity of a person (platform 3.8): `u` + the first 24 hex digits of SHA-256 of the normalised email.
#[must_use]
pub fn person_key(email: &str) -> String {
    let d = digest::digest(&digest::SHA256, normalise_email(email).as_bytes());
    let mut key = String::with_capacity(25);
    key.push('u');
    for b in &d.as_ref()[..12] {
        let _ = write!(key, "{b:02x}");
    }
    key
}

/// Removes what could be a credential or an email from a string (platform 15.1; the port of jarvis3
/// `common/log.ts` `redact()` with its rules, plus emails, which 3.8 and 15.1 keep out of logs). Rules, in order:
/// 1. `Bearer <x>`, `Basic <x>` (any case) → `Bearer [redacted]`, `Basic [redacted]`, when `<x>` is token-shaped
///    (token68 characters, and a digit, a symbol, mixed case or 20+ characters: `Basic info` is prose and stays);
/// 2. userinfo in a URL, `scheme://user:pass@host` or `scheme://token@host` → `scheme://[redacted]@host`, up to the
///    last `@` before the host (a raw `@` in a password does not leak the rest);
/// 3. an email, `local@domain.tld` or its percent-encoded form `local%40domain.tld` → its [`person_key`] (the log
///    identity of a person, so the line stays searchable);
/// 4. a parameter, field or header whose name contains `token`, `key`, `secret`, `password`, `passwd`,
///    `authorization`, `signature` or `sig` (any case), followed by `=` or `:` with optional quotes and whitespace
///    (`api_key=…`, `{"password":"…"}`, `Config { token: "…" }`, `authorization: …`) → `<name><sep>[redacted]`;
///    `person_key` is exempt (it is the log identity, not a credential);
/// 5. hermit's tokens, `hst1.…` and `hct_…` → `[redacted]` (whole, however short);
/// 6. any other run of 24 or more characters from `[A-Za-z0-9_\-+/=.]` and `%XX` escapes → `[redacted]`, except
///    UUIDs, person keys and RFC 3339 times, which are kept (inside a longer run they are kept and the pieces around
///    them judged on their own length).
///
/// (15.1 lists the hermit-token rule after the long-run rule, as caught by it anyway; it runs before it here so a
/// short token is caught too.)
#[must_use]
pub fn redact(s: &str) -> String {
    let s = auth_schemes(s);
    let s = userinfo(&s);
    let s = emails(&s);
    let s = secret_params(&s);
    let s = hermit_tokens(&s);
    long_runs(&s)
}

const REDACTED: &str = "[redacted]";

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// A token68 character (RFC 9110 11.2).
fn is_token68(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'+' | b'/' | b'=')
}

/// Whether a word after `Bearer`/`Basic` looks like a credential rather than prose.
fn token_shaped(t: &[u8]) -> bool {
    if t.is_empty() || !t.iter().all(|b| is_token68(*b)) {
        return false;
    }
    let digit = t.iter().any(u8::is_ascii_digit);
    let symbol = t.iter().any(|b| !b.is_ascii_alphanumeric());
    // mixed case past the first letter (a capitalised word is prose)
    let mixed = t[1..].iter().any(u8::is_ascii_uppercase) && t.iter().any(u8::is_ascii_lowercase);
    digit || symbol || mixed || t.len() >= 20
}

/// Rule 1.
fn auth_schemes(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut i = 0;
    while i < b.len() {
        let at_boundary = i == 0 || !is_word(b[i - 1]);
        let word_len = if at_boundary && starts_with_ignore_case(&b[i..], b"bearer") {
            6
        } else if at_boundary && starts_with_ignore_case(&b[i..], b"basic") {
            5
        } else {
            0
        };
        if word_len > 0 {
            let mut j = i + word_len;
            let ws_start = j;
            while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            let tok_start = j;
            while j < b.len() && !b[j].is_ascii_whitespace() && !matches!(b[j], b'"' | b'\'' | b',' | b';') {
                j += 1;
            }
            if tok_start > ws_start && token_shaped(&b[tok_start..j]) {
                out.push_str(&s[copied..tok_start]);
                out.push_str(REDACTED);
                copied = j;
                i = j;
                continue;
            }
            i += word_len;
            continue;
        }
        i += 1;
    }
    out.push_str(&s[copied..]);
    out
}

fn starts_with_ignore_case(hay: &[u8], needle: &[u8]) -> bool {
    hay.len() >= needle.len() && hay[..needle.len()].eq_ignore_ascii_case(needle)
}

/// Rule 2.
fn userinfo(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut i = 0;
    while i + 3 <= b.len() {
        if &b[i..i + 3] == b"://" {
            let start = i + 3;
            let mut j = start;
            let mut last_at = None;
            // the authority ends at the path, query or fragment; userinfo ends at its last `@`
            while j < b.len()
                && !b[j].is_ascii_whitespace()
                && !matches!(b[j], b'/' | b'?' | b'#' | b'"' | b'\'' | b'<' | b'>' | b'[' | b']')
            {
                if b[j] == b'@' {
                    last_at = Some(j);
                }
                j += 1;
            }
            if let Some(at) = last_at.filter(|at| *at > start && &s[start..*at] != REDACTED) {
                out.push_str(&s[copied..start]);
                out.push_str(REDACTED);
                copied = at;
            }
            i = j.max(i + 3);
            continue;
        }
        i += 1;
    }
    out.push_str(&s[copied..]);
    out
}

fn is_local(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-')
}

fn is_domain(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')
}

/// The end of a domain `label(.label)+` starting at `i` whose last label is two or more letters, if any.
fn domain_end(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i;
    while j < b.len() && is_domain(b[j]) {
        j += 1;
    }
    // trailing dots and hyphens are punctuation, not the domain
    while j > i && matches!(b[j - 1], b'.' | b'-') {
        j -= 1;
    }
    let domain = &b[i..j];
    let last_dot = domain.iter().rposition(|c| *c == b'.')?;
    let tld = &domain[last_dot + 1..];
    let labels_ok = domain.split(|c| *c == b'.').all(|l| !l.is_empty());
    (labels_ok && last_dot > 0 && tld.len() >= 2 && tld.iter().all(u8::is_ascii_alphabetic)).then_some(j)
}

/// Rule 3.
fn emails(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut i = 0;
    while i < b.len() {
        let sep = if b[i] == b'@' {
            1
        } else if b[i] == b'%' && i + 3 <= b.len() && b[i + 1..i + 3].eq_ignore_ascii_case(b"40") {
            3
        } else {
            0
        };
        if sep > 0 && i > copied {
            let mut start = i;
            while start > copied && is_local(b[start - 1]) {
                start -= 1;
            }
            // a local part does not start with a dot
            while start < i && b[start] == b'.' {
                start += 1;
            }
            if start < i
                && let Some(end) = domain_end(b, i + sep)
            {
                let email = format!("{}@{}", &s[start..i], &s[i + sep..end]);
                out.push_str(&s[copied..start]);
                out.push_str(&person_key(&email));
                copied = end;
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&s[copied..]);
    out
}

fn is_param_name(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')
}

/// jarvis3's names; `sig` also covers `signature`.
const SECRET_NAMES: &[&str] = &["token", "key", "secret", "password", "passwd", "authorization", "sig"];

/// Names that contain a secret word but are not secrets.
const NOT_SECRET_NAMES: &[&str] = &["person_key"];

/// The end of a parameter separator at `i`: `["']?\s*[=:]\s*["']?` (jarvis3's), if one is there.
fn separator_end(b: &[u8], mut i: usize) -> Option<usize> {
    if i < b.len() && matches!(b[i], b'"' | b'\'') {
        i += 1;
    }
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= b.len() || !matches!(b[i], b'=' | b':') {
        return None;
    }
    i += 1;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    if i < b.len() && matches!(b[i], b'"' | b'\'') {
        i += 1;
    }
    Some(i)
}

/// Rule 4.
fn secret_params(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut i = 0;
    while i < b.len() {
        // a name starts at a word boundary
        if !is_param_name(b[i]) || (i > 0 && is_param_name(b[i - 1])) {
            i += 1;
            continue;
        }
        let name_start = i;
        let mut name_end = i;
        while name_end < b.len() && is_param_name(b[name_end]) {
            name_end += 1;
        }
        let name = s[name_start..name_end].to_ascii_lowercase();
        let secret = SECRET_NAMES.iter().any(|w| name.contains(w)) && !NOT_SECRET_NAMES.contains(&name.as_str());
        if let Some(value_start) = secret.then(|| separator_end(b, name_end)).flatten() {
            let mut j = value_start;
            while j < b.len()
                && !b[j].is_ascii_whitespace()
                && !matches!(b[j], b'"' | b'\'' | b'&' | b',' | b';' | b'}')
            {
                j += 1;
            }
            let value = &s[value_start..j];
            // already redacted, or an auth scheme rule 1 has dealt with (`authorization: Bearer [redacted]`)
            let scheme_follows = (value.eq_ignore_ascii_case("bearer") || value.eq_ignore_ascii_case("basic"))
                && b.get(j).is_some_and(u8::is_ascii_whitespace);
            if j > value_start && !value.starts_with(REDACTED) && !scheme_follows {
                out.push_str(&s[copied..value_start]);
                out.push_str(REDACTED);
                copied = j;
                i = j;
                continue;
            }
        }
        i = name_end;
    }
    out.push_str(&s[copied..]);
    out
}

fn is_run(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+' | b'/' | b'=' | b'.')
}

/// The length of the run element at `i`: 1 for a run character, 3 for a `%XX` escape, 0 otherwise.
fn run_step(b: &[u8], i: usize) -> usize {
    if is_run(b[i]) {
        1
    } else if b[i] == b'%' && i + 3 <= b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
        3
    } else {
        0
    }
}

/// Rule 5.
fn hermit_tokens(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut i = 0;
    while i < b.len() {
        let boundary = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        if boundary && (b[i..].starts_with(b"hst1.") || b[i..].starts_with(b"hct_")) {
            let mut j = i;
            while j < b.len() && is_run(b[j]) {
                j += 1;
            }
            out.push_str(&s[copied..i]);
            out.push_str(REDACTED);
            copied = j;
            i = j;
            continue;
        }
        i += 1;
    }
    out.push_str(&s[copied..]);
    out
}

/// The length of a protected token (UUID, person key) starting at `i` inside `run`, when it is bounded by
/// non-alphanumeric characters (or the run's ends).
fn protected_at(run: &[u8], i: usize) -> Option<usize> {
    let bounded = |len: usize| {
        (i == 0 || !run[i - 1].is_ascii_alphanumeric())
            && (i + len == run.len() || !run[i + len].is_ascii_alphanumeric())
    };
    if i + 36 <= run.len() && is_uuid(&run[i..i + 36]) && bounded(36) {
        return Some(36);
    }
    if i + 25 <= run.len()
        && run[i] == b'u'
        && run[i + 1..i + 25]
            .iter()
            .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
        && bounded(25)
    {
        return Some(25);
    }
    None
}

fn is_uuid(t: &[u8]) -> bool {
    t.len() == 36
        && t.iter().enumerate().all(|(k, c)| {
            if matches!(k, 8 | 13 | 18 | 23) {
                *c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// Rule 6. (An RFC 3339 time holds `:`, which ends a run, so no piece of one reaches 24 characters.)
fn long_runs(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if run_step(b, i) == 0 {
            // copy one whole character (non-ASCII ones are never part of a run)
            let ch_len = s[i..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&s[i..i + ch_len]);
            i += ch_len;
            continue;
        }
        let start = i;
        loop {
            let step = if i < b.len() { run_step(b, i) } else { 0 };
            if step == 0 {
                break;
            }
            i += step;
        }
        let run = &s[start..i];
        if run.len() < 24 {
            out.push_str(run);
            continue;
        }
        // keep protected tokens; judge the pieces between them on their own length
        let rb = run.as_bytes();
        let mut piece = 0;
        let mut k = 0;
        while k < rb.len() {
            if let Some(len) = protected_at(rb, k) {
                push_piece(&mut out, &run[piece..k]);
                out.push_str(&run[k..k + len]);
                k += len;
                piece = k;
            } else {
                k += 1;
            }
        }
        push_piece(&mut out, &run[piece..]);
    }
    out
}

fn push_piece(out: &mut String, piece: &str) {
    if piece.len() >= 24 {
        out.push_str(REDACTED);
    } else {
        out.push_str(piece);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- redact, one test per rule (15.1) ----

    #[test]
    fn redact_bearer_and_basic() {
        assert_eq!(
            redact("Authorization: Bearer abc.def"),
            "Authorization: Bearer [redacted]"
        );
        assert_eq!(
            redact("authorization: bearer abc123"),
            "authorization: bearer [redacted]"
        );
        assert_eq!(redact("BASIC c2J4OnRvaw=="), "BASIC [redacted]");
        assert_eq!(redact("Basic dXNlcjpwYXNz"), "Basic [redacted]");
        assert_eq!(redact(r#"{"h":"Bearer xyz9"}"#), r#"{"h":"Bearer [redacted]"}"#);
        assert_eq!(redact("Bearer abcdefghijklmnopqrst"), "Bearer [redacted]");
        // not a scheme: part of a longer word
        assert_eq!(redact("unbearer x1"), "unbearer x1");
        // prose after the word stays: stable msg strings are not altered
        assert_eq!(redact("Basic info about the person"), "Basic info about the person");
        assert_eq!(redact("bearer Token refused"), "bearer Token refused");
    }

    #[test]
    fn redact_url_userinfo() {
        assert_eq!(
            redact("error connecting to postgresql://magpie:s3cr3t@pg-rw.db:5432/magpie"),
            "error connecting to postgresql://[redacted]@pg-rw.db:5432/magpie"
        );
        assert_eq!(redact("https://tok@example.com/x"), "https://[redacted]@example.com/x");
        assert_eq!(
            redact("proxy http://sbx:hst1.abc.def@192.0.2.1:3128"),
            "proxy http://[redacted]@192.0.2.1:3128"
        );
        // a raw `@` in the password: everything up to the last `@` of the authority goes
        assert_eq!(redact("postgresql://u:p@ss@host/db"), "postgresql://[redacted]@host/db");
        assert_eq!(
            redact("postgresql://u:p@ss@[2001:db8::1]:5432/db"),
            "postgresql://[redacted]@[2001:db8::1]:5432/db"
        );
        // an `@` in the path is not userinfo
        assert_eq!(redact("http://magpie/v1/x@y"), "http://magpie/v1/x@y");
        // no userinfo: unchanged
        assert_eq!(redact("http://magpie/v1/x"), "http://magpie/v1/x");
    }

    #[test]
    fn redact_emails() {
        let pk = person_key("alice@example.hk");
        assert_eq!(redact("failed for alice@example.hk"), format!("failed for {pk}"));
        assert_eq!(redact("failed for Alice@Example.HK."), format!("failed for {pk}."));
        assert_eq!(
            redact("GET /v1/people/alice%40example.hk"),
            format!("GET /v1/people/{pk}")
        );
        assert_eq!(
            redact("GET /v1/people/alice%40example.hk/x"),
            format!("GET /v1/people/{pk}/x")
        );
        assert_eq!(
            redact(r#"Request { person: "first.last+tag@mail.example.com" }"#),
            format!(
                r#"Request {{ person: "{}" }}"#,
                person_key("first.last+tag@mail.example.com")
            )
        );
        // not emails: no domain, no letters in the last label, nothing before the @
        assert_eq!(redact("user@localhost"), "user@localhost");
        assert_eq!(redact("v1@1.2"), "v1@1.2");
        assert_eq!(redact("@everyone"), "@everyone");
        assert!(!redact("to bob.smith@corp.example and carol@x.org").contains('@'));
    }

    #[test]
    fn redact_secret_parameters() {
        assert_eq!(
            redact("GET /x?access_token=abc&n=1"),
            "GET /x?access_token=[redacted]&n=1"
        );
        assert_eq!(
            redact("u?X-Amz-Signature=deadbeef&X-Amz-Date=1"),
            "u?X-Amz-Signature=[redacted]&X-Amz-Date=1"
        );
        assert_eq!(
            redact("api_key=zzz sig=1 password=p SECRET=q passwd=abc"),
            "api_key=[redacted] sig=[redacted] password=[redacted] SECRET=[redacted] passwd=[redacted]"
        );
        // other parameters stay
        assert_eq!(redact("a=1&limit=20"), "a=1&limit=20");
    }

    #[test]
    fn redact_secret_fields_in_json_debug_and_headers() {
        assert_eq!(
            redact(r#"{"password":"hunter2","api_key":"short1","n":1}"#),
            r#"{"password":"[redacted]","api_key":"[redacted]","n":1}"#
        );
        assert_eq!(redact(r#"{"password": "hunter2"}"#), r#"{"password": "[redacted]"}"#);
        assert_eq!(
            redact(r#"Config { password: "hunter2", token: "abc123", port: 5432 }"#),
            r#"Config { password: "[redacted]", token: "[redacted]", port: 5432 }"#
        );
        assert_eq!(redact("authorization: rawtok123"), "authorization: [redacted]");
        assert_eq!(
            redact("x-api-key: k1\nsecret='s'"),
            "x-api-key: [redacted]\nsecret='[redacted]'"
        );
        // a scheme after the name is rule 1's
        assert_eq!(
            redact("Authorization: Bearer abc.def"),
            "Authorization: Bearer [redacted]"
        );
        // person_key is the log identity, not a credential
        let pk = person_key("a@b.example");
        assert_eq!(redact(&format!("person_key={pk}")), format!("person_key={pk}"));
        assert_eq!(
            redact(&format!(r#"{{"person_key":"{pk}"}}"#)),
            format!(r#"{{"person_key":"{pk}"}}"#)
        );
    }

    #[test]
    fn redact_hermit_tokens() {
        assert_eq!(redact("got hst1.short"), "got [redacted]");
        assert_eq!(redact("control hct_abc,next"), "control [redacted],next");
        assert_eq!(redact("x=hst1.aaaa.bbbb"), "x=[redacted]");
    }

    #[test]
    fn redact_long_runs() {
        let tok = "Zm9vYmFyYmF6cXV4cXV1eHh4eXl5enp6"; // 32 base64 characters
        assert_eq!(redact(&format!("got {tok} back")), "got [redacted] back");
        assert_eq!(redact("short_run_of_23_chars_x"), "short_run_of_23_chars_x");
        assert_eq!(redact("abcdefghijklmnopqrstuvwx"), "[redacted]");
        // percent escapes do not split a run
        assert_eq!(redact("got Zm9vYmFy%2BYmF6cXV4%2FcXV1eHh4 back"), "got [redacted] back");
        assert_eq!(redact("a%2Bb"), "a%2Bb");
    }

    #[test]
    fn redact_keeps_uuids_person_keys_and_times() {
        let id = "0190f3b2-7c1a-7d2e-9f00-0123456789ab";
        let pk = person_key("Someone@Example.com ");
        let t = "2026-11-02T04:05:06.789Z";
        let line = format!("conversation {id} person {pk} at {t}");
        assert_eq!(redact(&line), line);
        // inside a longer run, the id survives and the long pieces around it do not
        assert_eq!(
            redact(&format!("http://magpie/v1/admin/conversations/{id}/events")),
            format!("http://magpie/v1/admin/conversations/{id}/events")
                .replace("//magpie/v1/admin/conversations/", "[redacted]")
        );
    }

    #[test]
    fn redact_leaves_plain_text_and_unicode() {
        assert_eq!(redact("turn ended"), "turn ended");
        assert_eq!(redact("檔案已上載 ok"), "檔案已上載 ok");
        assert_eq!(redact(""), "");
    }

    #[test]
    fn redact_is_idempotent() {
        let once =
            redact("Bearer abc1 a@b.example postgresql://u:p@h/x?token=t hst1.x Zm9vYmFyYmF6cXV4cXV1eHh4eXl5enp6");
        assert_eq!(redact(&once), once);
    }

    // ---- line shape ----

    fn capture() -> (Capture, impl Subscriber + Send + Sync + 'static) {
        let c = Capture::new();
        let s = subscriber("magpie", Some("magpie-7c9".into()), c.clone());
        (c, s)
    }

    #[test]
    fn line_shape_and_fixed_fields() {
        let (c, s) = capture();
        let conv = uuid::Uuid::now_v7();
        tracing::subscriber::with_default(s, || {
            tracing::info!(conversation = %conv, status = 202_u64, ms = 41_u64, route = "POST /v1/conversations/{id}/turns", "turn ended");
        });
        let text = c.text();
        assert_eq!(text.lines().count(), 1);
        assert!(text.starts_with(r#"{"t":""#), "{text}");
        let l = &c.lines()[0];
        let t = l["t"].as_str().unwrap();
        assert_eq!(t.len(), "2026-11-02T04:05:06.789Z".len());
        assert!(t.ends_with('Z'));
        assert_eq!(l["level"], "info");
        assert_eq!(l["svc"], "magpie");
        assert_eq!(l["ver"], version::sha12());
        assert_eq!(l["pod"], "magpie-7c9");
        assert_eq!(l["msg"], "turn ended");
        assert_eq!(l["conversation"], conv.to_string());
        assert_eq!(l["status"], 202);
        assert_eq!(l["ms"], 41);
        assert_eq!(l["route"], "POST /v1/conversations/{id}/turns");
        // the fixed fields come first, in order
        let keys: Vec<&str> = text
            .split("\":")
            .map(|p| p.rsplit('"').next().unwrap())
            .take(6)
            .collect();
        assert_eq!(keys, ["t", "level", "svc", "ver", "pod", "msg"]);
    }

    #[test]
    fn every_string_field_is_redacted() {
        let (c, s) = capture();
        tracing::subscriber::with_default(s, || {
            let err = std::io::Error::other("connect postgresql://magpie:pw@pg:5432/magpie failed");
            tracing::warn!(
                error = &err as &(dyn std::error::Error + 'static),
                detail = "Bearer abc1",
                url = %"https://x.test/?token=t1",
                dbg = ?"hst1.secret",
                "call failed with Basic dXNlcjpwYXNz"
            );
        });
        let l = &c.lines()[0];
        assert_eq!(l["level"], "warn");
        assert_eq!(l["error"], "connect postgresql://[redacted]@pg:5432/magpie failed");
        assert_eq!(l["detail"], "Bearer [redacted]");
        assert_eq!(l["url"], "https://x.test/?token=[redacted]");
        assert_eq!(l["dbg"], "\"[redacted]\"");
        assert_eq!(l["msg"], "call failed with Basic [redacted]");
        assert!(!c.text().contains("pw@"));
        assert!(!c.text().contains("dXNlcjpwYXNz"));
    }

    #[test]
    fn emails_never_reach_a_line() {
        let (c, s) = capture();
        tracing::subscriber::with_default(s, || {
            let err = std::io::Error::other("GET http://hermit/v1/people/alice%40example.hk: 404");
            tracing::warn!(
                error = &err as &(dyn std::error::Error + 'static),
                who = "alice@example.hk",
                "lookup failed for alice@example.hk"
            );
        });
        let text = c.text();
        assert!(!text.contains("alice"), "{text}");
        let l = &c.lines()[0];
        let pk = person_key("alice@example.hk");
        assert_eq!(l["who"], pk);
        assert_eq!(l["msg"], format!("lookup failed for {pk}"));
    }

    #[test]
    fn typed_fields_are_written_as_they_are() {
        let (c, s) = capture();
        let pk = person_key("a@b.c");
        tracing::subscriber::with_default(s, || {
            tracing::info!(route = "GET /v1/admin/conversations/{id}/events", person_key = %pk, turn = "t_0123456789abcdefghijklmn", "x");
        });
        let l = &c.lines()[0];
        assert_eq!(l["route"], "GET /v1/admin/conversations/{id}/events");
        assert_eq!(l["person_key"], pk);
        assert_eq!(l["turn"], "t_0123456789abcdefghijklmn");
    }

    #[test]
    fn span_fields_are_included_and_redacted() {
        let (c, s) = capture();
        let req = uuid::Uuid::now_v7();
        tracing::subscriber::with_default(s, || {
            let outer = tracing::info_span!("request", request = %req, peer = "Bearer zzz9");
            let _g = outer.enter();
            let inner = tracing::info_span!("work", sandbox = "s1");
            let _g2 = inner.enter();
            tracing::info!(svc = "spoofed", "inside");
        });
        let l = &c.lines()[0];
        assert_eq!(l["request"], req.to_string());
        assert_eq!(l["peer"], "Bearer [redacted]");
        assert_eq!(l["sandbox"], "s1");
        assert_eq!(l["svc"], "magpie", "a call site cannot override a fixed field");
    }

    #[test]
    fn debug_level_is_not_written() {
        let (c, s) = capture();
        tracing::subscriber::with_default(s, || {
            tracing::debug!("noise");
            tracing::error!("needs an operator");
        });
        let lines = c.lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["level"], "error");
    }

    #[test]
    fn person_key_matches_user_key() {
        // "u" + first 24 hex of sha256("a@b.c")
        let k = person_key("  A@B.C ");
        assert_eq!(k.len(), 25);
        assert!(k.starts_with('u'));
        assert_eq!(k, person_key("a@b.c"));
        let d = digest::digest(&digest::SHA256, b"a@b.c");
        let hex = d.as_ref()[..12].iter().fold(String::new(), |mut h, b| {
            let _ = write!(h, "{b:02x}");
            h
        });
        assert_eq!(k, format!("u{hex}"));
    }

    #[test]
    fn rfc3339_format() {
        let t = time::macros::datetime!(2026-11-02 04:05:06.789 UTC);
        assert_eq!(rfc3339(t), "2026-11-02T04:05:06.789Z");
    }
}
