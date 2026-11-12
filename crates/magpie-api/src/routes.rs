//! magpie's route table (magpie 16): path templates as constants, and one [`Route`] per method and path with its
//! callers and success statuses. The client, the server router and the fixture tests are all checked against it.
//!
//! Path templates use axum's `{param}` syntax; the client fills them with percent-encoded values.

use std::fmt::Write as _;

use crate::Caller;

/// An HTTP method of a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// GET.
    Get,
    /// POST.
    Post,
    /// PUT.
    Put,
    /// PATCH.
    Patch,
    /// DELETE.
    Delete,
}

impl Method {
    /// The method's name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        }
    }
}

/// Which `Aviary-Protocol` surface a route belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Surface {
    /// `magpie-person` (pigeon, hermit, ops, admin).
    Person,
    /// `magpie-sandbox` (the supervisor through the gateway).
    Sandbox,
}

/// One route: a method on a path template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    /// Stable name, also the fixture file `fixtures/routes/<id>.json` and the client/server method name.
    pub id: &'static str,
    /// pigeon 14.1's number (`M1` … `M17`), if any.
    pub m: Option<&'static str>,
    /// Method.
    pub method: Method,
    /// Path template.
    pub path: &'static str,
    /// Callers allowed (anyone else: 403 `wrong_caller`).
    pub callers: &'static [Caller],
    /// Success statuses (the first is the usual one).
    pub ok: &'static [u16],
    /// `Aviary-Person` is required.
    pub person: bool,
    /// `Aviary-Lease` is required.
    pub lease: bool,
    /// `Aviary-Admin` is required.
    pub admin: bool,
    /// The protocol surface.
    pub surface: Surface,
}

// ---- path templates ----

/// `GET` list (M1), `POST` create (M3).
pub const CONVERSATIONS: &str = "/v1/conversations";
/// `GET` details (M2), `PATCH` rename (M5), `DELETE` permanent delete (M6).
pub const CONVERSATION: &str = "/v1/conversations/{id}";
/// `POST` soft delete (M6).
pub const CONVERSATION_DELETE: &str = "/v1/conversations/{id}/delete";
/// `POST` restore (M6).
pub const CONVERSATION_RESTORE: &str = "/v1/conversations/{id}/restore";
/// `GET` creation lookup (M4).
pub const CREATION: &str = "/v1/creations/{idempotencyKey}";
/// `POST` send (M7).
pub const TURNS: &str = "/v1/conversations/{id}/turns";
/// `POST` cancel (M8).
pub const CANCEL: &str = "/v1/conversations/{id}/cancel";
/// `GET` timeline (M9).
pub const TIMELINE: &str = "/v1/conversations/{id}/timeline";
/// `GET` live state.
pub const LIVE: &str = "/v1/conversations/{id}/live";
/// `GET` prompts (M10).
pub const PROMPTS: &str = "/v1/conversations/{id}/prompts";
/// `GET` search.
pub const SEARCH: &str = "/v1/conversations/{id}/search";
/// The raw view's transcript lines (view-as only).
pub const TRANSCRIPT: &str = "/v1/conversations/{id}/transcript";
/// `POST` ask answer (M11).
pub const ASK_ANSWER: &str = "/v1/conversations/{id}/asks/{askId}/answer";
/// `GET` change feed (M12).
pub const CHANGES: &str = "/v1/changes";
/// `POST` upload (M13).
pub const UPLOADS: &str = "/v1/uploads";
/// `GET` download (M14).
pub const FILES_RAW: &str = "/v1/conversations/{id}/files/raw";
/// `GET` files panel.
pub const FILES: &str = "/v1/conversations/{id}/files";
/// `GET` blob (M15).
pub const BLOB: &str = "/v1/blobs/{blobId}";
/// `GET` list, `POST` create (M16).
pub const CONVERSATION_SHARES: &str = "/v1/conversations/{id}/shares";
/// `GET` view, `DELETE` revoke (M16).
pub const SHARE: &str = "/v1/shares/{shareId}";
/// `GET` share download (M16).
pub const SHARE_FILES_RAW: &str = "/v1/shares/{shareId}/files/raw";
/// `POST` continue (M16).
pub const SHARE_CONTINUE: &str = "/v1/shares/{shareId}/continue";
/// `POST` record view-as (M17).
pub const VIEW_AS: &str = "/v1/view-as";
/// `PUT` a teed upload's old asset path (the legacy-path set, until A7: [`crate::legacy`]).
pub const UPLOAD_LEGACY_PATH: &str = "/v1/uploads/{uploadId}/legacy-path";

/// `POST` lease.
pub const SANDBOX_LEASE: &str = "/v1/sandbox/lease";
/// `GET` tree.
pub const SANDBOX_CC_TREE: &str = "/v1/sandbox/cc/tree";
/// `GET` file bytes.
pub const SANDBOX_CC_FILE: &str = "/v1/sandbox/cc/file";
/// `POST` write.
pub const SANDBOX_CC_WRITE: &str = "/v1/sandbox/cc/write";
/// `POST` truncate.
pub const SANDBOX_CC_TRUNCATE: &str = "/v1/sandbox/cc/truncate";
/// `POST` rename.
pub const SANDBOX_CC_RENAME: &str = "/v1/sandbox/cc/rename";
/// `POST` unlink.
pub const SANDBOX_CC_UNLINK: &str = "/v1/sandbox/cc/unlink";
/// `POST` mkdir.
pub const SANDBOX_CC_MKDIR: &str = "/v1/sandbox/cc/mkdir";
/// `POST` rmdir.
pub const SANDBOX_CC_RMDIR: &str = "/v1/sandbox/cc/rmdir";
/// `POST` setattr.
pub const SANDBOX_CC_SETATTR: &str = "/v1/sandbox/cc/setattr";
/// `GET` command poll.
pub const SANDBOX_COMMANDS: &str = "/v1/sandbox/commands";
/// `POST` command ack.
pub const SANDBOX_COMMAND_ACK: &str = "/v1/sandbox/commands/{cmdSeq}/ack";
/// `POST` agent-started turn.
pub const SANDBOX_TURNS: &str = "/v1/sandbox/turns";
/// `POST` turn state.
pub const SANDBOX_TURN_STATE: &str = "/v1/sandbox/turns/{turnId}/state";
/// `POST` live frames.
pub const SANDBOX_TURN_LIVE: &str = "/v1/sandbox/turns/{turnId}/live";
/// `POST` task events.
pub const SANDBOX_TASKS: &str = "/v1/sandbox/tasks";
/// `POST` open ask.
pub const SANDBOX_ASKS: &str = "/v1/sandbox/asks";
/// `POST` ask resolved.
pub const SANDBOX_ASK_RESOLVED: &str = "/v1/sandbox/asks/{askId}/resolved";
/// `POST` heartbeat.
pub const SANDBOX_HEARTBEAT: &str = "/v1/sandbox/heartbeat";
/// `GET` and `PUT` a blob.
pub const SANDBOX_BLOB: &str = "/v1/sandbox/blobs/{sha}";
/// `POST` sent-file staging.
pub const SANDBOX_SENT_FILES: &str = "/v1/sandbox/sent-files";
/// `POST` sent-file commit.
pub const SANDBOX_SENT_FILES_COMMIT: &str = "/v1/sandbox/sent-files/{toolUseId}/commit";
/// `GET` the person's schedules, `POST` arm one (the AI, [`crate::schedules`]).
pub const SANDBOX_SCHEDULES: &str = "/v1/sandbox/schedules";
/// `GET` one with its last runs, `PATCH` change, `DELETE` remove (the AI).
pub const SANDBOX_SCHEDULE: &str = "/v1/sandbox/schedules/{scheduleId}";
/// `GET` a conversation's schedules with their last runs (the app's panel).
pub const CONVERSATION_SCHEDULES: &str = "/v1/conversations/{id}/schedules";
/// `GET` everyone's schedules and the pause-all switch (admin).
pub const ADMIN_SCHEDULES: &str = "/v1/admin/schedules";
/// `PUT` the pause-all switch (admin).
pub const ADMIN_SCHEDULES_PAUSE: &str = "/v1/admin/schedules/pause";

/// `POST` person purge (hermit).
pub const HERMIT_PERSON_PURGE: &str = "/v1/hermit/persons/{person}/purge";
/// A conversation the deployment starts for a person (hermit's, where `conversations.peopleCreate` is false).
pub const HERMIT_CONVERSATIONS: &str = "/v1/hermit/conversations";
/// `GET` a conversation as the deployment reads it (hermit: the title and the picked model of a session's conversation).
pub const HERMIT_CONVERSATION: &str = "/v1/hermit/conversations/{id}";
/// A turn the deployment sends into such a conversation as its person.
pub const HERMIT_CONVERSATION_TURNS: &str = "/v1/hermit/conversations/{id}/turns";
/// `POST` workspace references (hermit).
pub const HERMIT_WORKSPACES_EXISTS: &str = "/v1/hermit/workspaces/exists";
/// `GET` whether the switch's import is complete (hermit's `legacy-cleanup`).
pub const HERMIT_LEGACY_IMPORT: &str = "/v1/hermit/legacy-import";
/// `GET` per-person counts.
pub const OPS_PEOPLE: &str = "/v1/ops/people";
/// `GET` view-as log.
pub const OPS_VIEW_AS: &str = "/v1/ops/view-as";
/// `GET` store stats.
pub const OPS_STATS: &str = "/v1/ops/stats";
/// `GET` admin list.
pub const ADMIN_CONVERSATIONS: &str = "/v1/admin/conversations";
/// `GET` admin details.
pub const ADMIN_CONVERSATION: &str = "/v1/admin/conversations/{id}";
/// `GET` admin events page.
pub const ADMIN_EVENTS: &str = "/v1/admin/conversations/{id}/events";
/// `GET` admin timeline page.
pub const ADMIN_TIMELINE: &str = "/v1/admin/conversations/{id}/timeline";
/// `GET` the raw view's transcript lines for an admin (pigeon's raw page, or the ops tools), any conversation.
pub const ADMIN_TRANSCRIPT: &str = "/v1/admin/conversations/{id}/transcript";

const PIGEON: &[Caller] = &[Caller::Pigeon];
const GATEWAY: &[Caller] = &[Caller::Gateway];
const HERMIT: &[Caller] = &[Caller::Hermit];
const OPS: &[Caller] = &[Caller::Hermit, Caller::Admin];
const ADMIN: &[Caller] = &[Caller::Admin];
/// pigeon on behalf of a signed-in admin (it sets `Aviary-Admin` to the admin's verified email), or the ops tools.
const PIGEON_ADMIN: &[Caller] = &[Caller::Pigeon, Caller::Admin];

const fn person(id: &'static str, m: &'static str, method: Method, path: &'static str, ok: &'static [u16]) -> Route {
    Route {
        id,
        m: Some(m),
        method,
        path,
        callers: PIGEON,
        ok,
        person: true,
        lease: false,
        admin: false,
        surface: Surface::Person,
    }
}

const fn pigeon_extra(id: &'static str, method: Method, path: &'static str, ok: &'static [u16]) -> Route {
    Route {
        id,
        m: None,
        method,
        path,
        callers: PIGEON,
        ok,
        person: true,
        lease: false,
        admin: false,
        surface: Surface::Person,
    }
}

const fn sandbox(id: &'static str, method: Method, path: &'static str, ok: &'static [u16], lease: bool) -> Route {
    Route {
        id,
        m: None,
        method,
        path,
        callers: GATEWAY,
        ok,
        person: false,
        lease,
        admin: false,
        surface: Surface::Sandbox,
    }
}

const fn service(
    id: &'static str,
    method: Method,
    path: &'static str,
    callers: &'static [Caller],
    ok: &'static [u16],
    admin: bool,
) -> Route {
    Route {
        id,
        m: None,
        method,
        path,
        callers,
        ok,
        person: false,
        lease: false,
        admin,
        surface: Surface::Person,
    }
}

use Method::{Delete, Get, Patch, Post, Put};

/// Every route magpie serves (magpie 16.1-16.4). Health is on :9090 and not part of this contract.
pub const ROUTES: &[Route] = &[
    // 16.1 pigeon
    person("list_conversations", "M1", Get, CONVERSATIONS, &[200]),
    person("get_conversation", "M2", Get, CONVERSATION, &[200]),
    person("create_conversation", "M3", Post, CONVERSATIONS, &[201, 200]),
    person("get_creation", "M4", Get, CREATION, &[200]),
    person("rename_conversation", "M5", Patch, CONVERSATION, &[200]),
    person("delete_conversation", "M6", Post, CONVERSATION_DELETE, &[200]),
    person("restore_conversation", "M6", Post, CONVERSATION_RESTORE, &[200]),
    person("purge_conversation", "M6", Delete, CONVERSATION, &[204]),
    person("send_turn", "M7", Post, TURNS, &[202, 200]),
    person("cancel_turn", "M8", Post, CANCEL, &[202]),
    person("timeline", "M9", Get, TIMELINE, &[200]),
    pigeon_extra("live", Get, LIVE, &[200, 204]),
    person("prompts", "M10", Get, PROMPTS, &[200]),
    pigeon_extra("search", Get, SEARCH, &[200]),
    pigeon_extra("transcript", Get, TRANSCRIPT, &[200]),
    person("answer_ask", "M11", Post, ASK_ANSWER, &[202]),
    person("changes", "M12", Get, CHANGES, &[200]),
    person("upload", "M13", Post, UPLOADS, &[201]),
    person("files_raw", "M14", Get, FILES_RAW, &[200, 206]),
    pigeon_extra("files_panel", Get, FILES, &[200]),
    pigeon_extra("conversation_schedules", Get, CONVERSATION_SCHEDULES, &[200]),
    Route {
        id: "blob",
        m: Some("M15"),
        method: Get,
        path: BLOB,
        callers: PIGEON,
        ok: &[200],
        person: false,
        lease: false,
        admin: false,
        surface: Surface::Person,
    },
    person("list_shares", "M16", Get, CONVERSATION_SHARES, &[200]),
    person("create_share", "M16", Post, CONVERSATION_SHARES, &[201]),
    person("revoke_share", "M16", Delete, SHARE, &[204]),
    person("get_share", "M16", Get, SHARE, &[200]),
    person("share_files_raw", "M16", Get, SHARE_FILES_RAW, &[200, 206]),
    person("continue_share", "M16", Post, SHARE_CONTINUE, &[201]),
    Route {
        id: "view_as",
        m: Some("M17"),
        method: Post,
        path: VIEW_AS,
        callers: PIGEON,
        ok: &[201],
        person: false,
        lease: false,
        admin: false,
        surface: Surface::Person,
    },
    // the legacy-path record (DESIGN 9.5), until A7
    pigeon_extra("upload_legacy_path", Put, UPLOAD_LEGACY_PATH, &[204]),
    // 16.2 the supervisor through the gateway
    sandbox("lease", Post, SANDBOX_LEASE, &[200], false),
    sandbox("cc_tree", Get, SANDBOX_CC_TREE, &[200], false),
    sandbox("cc_file", Get, SANDBOX_CC_FILE, &[200, 206], false),
    sandbox("cc_write", Post, SANDBOX_CC_WRITE, &[200], true),
    sandbox("cc_truncate", Post, SANDBOX_CC_TRUNCATE, &[200], true),
    sandbox("cc_rename", Post, SANDBOX_CC_RENAME, &[200], true),
    sandbox("cc_unlink", Post, SANDBOX_CC_UNLINK, &[204], true),
    sandbox("cc_mkdir", Post, SANDBOX_CC_MKDIR, &[204], true),
    sandbox("cc_rmdir", Post, SANDBOX_CC_RMDIR, &[204], true),
    sandbox("cc_setattr", Post, SANDBOX_CC_SETATTR, &[200], true),
    sandbox("commands", Get, SANDBOX_COMMANDS, &[200], true),
    sandbox("ack_command", Post, SANDBOX_COMMAND_ACK, &[204], true),
    sandbox("agent_turn", Post, SANDBOX_TURNS, &[201, 200], true),
    sandbox("turn_state", Post, SANDBOX_TURN_STATE, &[204], true),
    sandbox("live_frames", Post, SANDBOX_TURN_LIVE, &[204], true),
    sandbox("tasks", Post, SANDBOX_TASKS, &[204], true),
    sandbox("ask_open", Post, SANDBOX_ASKS, &[201], true),
    sandbox("ask_resolved", Post, SANDBOX_ASK_RESOLVED, &[204], true),
    sandbox("heartbeat", Post, SANDBOX_HEARTBEAT, &[204], true),
    sandbox("sandbox_blob_get", Get, SANDBOX_BLOB, &[200], false),
    sandbox("sandbox_blob_put", Put, SANDBOX_BLOB, &[201, 200], true),
    sandbox("sent_files", Post, SANDBOX_SENT_FILES, &[200], true),
    sandbox("sent_files_commit", Post, SANDBOX_SENT_FILES_COMMIT, &[200], true),
    // schedules: Claude Code itself calls these with curl (no lease: it is not the supervisor)
    sandbox("list_schedules", Get, SANDBOX_SCHEDULES, &[200], false),
    sandbox("create_schedule", Post, SANDBOX_SCHEDULES, &[201], false),
    sandbox("get_schedule", Get, SANDBOX_SCHEDULE, &[200], false),
    sandbox("update_schedule", Patch, SANDBOX_SCHEDULE, &[200], false),
    sandbox("delete_schedule", Delete, SANDBOX_SCHEDULE, &[204], false),
    // 16.3 hermit
    service("person_purge", Post, HERMIT_PERSON_PURGE, HERMIT, &[202], false),
    service(
        "hermit_create_conversation",
        Post,
        HERMIT_CONVERSATIONS,
        HERMIT,
        &[201, 200],
        false,
    ),
    service("hermit_conversation", Get, HERMIT_CONVERSATION, HERMIT, &[200], false),
    service(
        "hermit_send_turn",
        Post,
        HERMIT_CONVERSATION_TURNS,
        HERMIT,
        &[202, 200],
        false,
    ),
    service(
        "workspaces_exists",
        Post,
        HERMIT_WORKSPACES_EXISTS,
        HERMIT,
        &[200],
        false,
    ),
    service("legacy_import", Get, HERMIT_LEGACY_IMPORT, HERMIT, &[200], false),
    // 16.4 ops and admin
    service("ops_people", Get, OPS_PEOPLE, OPS, &[200], false),
    service("ops_view_as", Get, OPS_VIEW_AS, OPS, &[200], false),
    service("ops_stats", Get, OPS_STATS, OPS, &[200], false),
    service("admin_conversations", Get, ADMIN_CONVERSATIONS, ADMIN, &[200], true),
    service("admin_conversation", Get, ADMIN_CONVERSATION, ADMIN, &[200], true),
    service("admin_events", Get, ADMIN_EVENTS, ADMIN, &[200], true),
    service("admin_timeline", Get, ADMIN_TIMELINE, ADMIN, &[200], true),
    service("admin_transcript", Get, ADMIN_TRANSCRIPT, PIGEON_ADMIN, &[200], true),
    service("admin_schedules", Get, ADMIN_SCHEDULES, PIGEON_ADMIN, &[200], true),
    service(
        "admin_schedules_pause",
        Put,
        ADMIN_SCHEDULES_PAUSE,
        PIGEON_ADMIN,
        &[200],
        true,
    ),
];

/// The route with this id.
#[must_use]
pub fn by_id(id: &str) -> Option<&'static Route> {
    ROUTES.iter().find(|r| r.id == id)
}

/// The route a request matches (method and concrete path), for servers' route-to-caller checks.
#[must_use]
pub fn matching(method: Method, path: &str) -> Option<&'static Route> {
    ROUTES
        .iter()
        .find(|r| r.method == method && template_matches(r.path, path))
}

/// Whether a concrete path matches a template (`{param}` matches one non-empty segment).
#[must_use]
pub fn template_matches(template: &str, path: &str) -> bool {
    let t: Vec<&str> = template.split('/').collect();
    let p: Vec<&str> = path.split('/').collect();
    t.len() == p.len()
        && t.iter().zip(&p).all(|(t, p)| {
            if t.starts_with('{') && t.ends_with('}') {
                !p.is_empty()
            } else {
                t == p
            }
        })
}

/// The `{param}` names of a template, in order.
#[must_use]
pub fn params(template: &str) -> Vec<&str> {
    template
        .split('/')
        .filter_map(|s| s.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
        .collect()
}

/// Fill a template's `{param}`s, in order, with percent-encoded values.
///
/// # Panics
/// Never: a template with more placeholders than values keeps the extra placeholders (a test catches that).
#[must_use]
pub fn fill(template: &str, values: &[&str]) -> String {
    let mut values = values.iter();
    template
        .split('/')
        .map(|seg| {
            if seg.starts_with('{') && seg.ends_with('}') {
                values.next().map_or_else(|| seg.to_owned(), |v| encode_segment(v))
            } else {
                seg.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Percent-encode one path segment (everything but RFC 3986 unreserved characters and `@`).
#[must_use]
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'@') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Decode percent-encoding (a header value such as `Aviary-Upload-Name`). Invalid escapes are kept as they are;
/// invalid UTF-8 is replaced.
#[must_use]
pub fn decode_percent(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_and_method_paths_are_unique_and_under_v1() {
        let mut ids = HashSet::new();
        let mut keys = HashSet::new();
        for r in ROUTES {
            assert!(ids.insert(r.id), "duplicate id {}", r.id);
            assert!(
                keys.insert((r.method, r.path)),
                "duplicate route {} {}",
                r.method.as_str(),
                r.path
            );
            assert!(r.path.starts_with("/v1/"), "{}", r.path);
            assert!(!r.callers.is_empty() && !r.ok.is_empty(), "{}", r.id);
            for p in params(r.path) {
                assert!(!p.contains('_'), "{}: path params are camelCase", r.id);
            }
            if r.surface == Surface::Sandbox {
                assert!(r.path.starts_with("/v1/sandbox/"), "{}", r.id);
            }
        }
        assert_eq!(ROUTES.len(), 74);
    }

    #[test]
    fn matching_prefers_exact_segments() {
        assert_eq!(
            matching(Method::Get, "/v1/conversations/abc").map(|r| r.id),
            Some("get_conversation")
        );
        assert_eq!(
            matching(Method::Post, "/v1/conversations/abc/delete").map(|r| r.id),
            Some("delete_conversation")
        );
        assert_eq!(matching(Method::Get, "/v1/conversations//timeline"), None);
        assert_eq!(
            matching(Method::Put, "/v1/sandbox/blobs/ab12").map(|r| r.id),
            Some("sandbox_blob_put")
        );
    }

    #[test]
    fn fill_encodes_values() {
        assert_eq!(
            fill(HERMIT_PERSON_PURGE, &["a.b@example.com"]),
            "/v1/hermit/persons/a.b@example.com/purge"
        );
        assert_eq!(fill(CREATION, &["draft 7f3a/x"]), "/v1/creations/draft%207f3a%2Fx");
        assert_eq!(decode_percent("Q3%20report%E6%96%B0.pdf"), "Q3 report新.pdf");
        assert_eq!(decode_percent("100%"), "100%");
    }
}
