//! Admission (M7 `POST /v1/conversations/{id}/turns`, and M3's first message), magpie 8.2.
//!
//! One transaction: the conversation row lock, the idempotency check on `clientMessageId`, the single admission
//! point (while a turn is admitted, preparing, running or stopping, two sockets sending at once get exactly one
//! turn: the second message joins that turn as a mid-turn message, [`super::midturn`], or is refused 409
//! `turn_running` when it has attachments or comes from the deployment), the attachments, `user.sent` and `turn.started`, the turn, its `start_turn` (and `title`)
//! command, the conversation's run state, and the changes. The user's text is stored before anything can fail, so a
//! turn that fails later still shows as sent and unanswered.
//!
//! Attachments name the person's unattached uploads: `images[].uploadId`, and the upload id inside each
//! `files[].path` (`uploads/<uploadId>/<name>`, as M13 returned it). Old asset paths (the switch window's legacy
//! paths, magpie 8.2) are resolved to fresh uploads before the transaction (`crate::legacy::paths`).

use std::collections::HashSet;

use aviary_pg::{Pool, Tx};
use magpie_api::Admitted;
use magpie_api::Timestamp;
use magpie_api::errors::ErrorCode;
use magpie_api::events::{EventBody, TurnStartedData, UserSent};
use magpie_api::sandbox::{Resume, StartFile, StartImage, StartTurn, TitleCommand, WORKSPACE_NAME_MAX_BYTES};
use magpie_api::turns::{SendRequest, SendResponse, TurnOrigin, TurnPhase, TurnStarted};
use magpie_api::uploads::{AttachedImage, Attachment, BlobKind, BlobPointer, BlobRef};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{command, hex, secs, titles, turn_change};
use crate::changes;
use crate::conversations::{Person, ensure_person, lock};
use crate::error::{Error, Result};
use crate::events::{self, NewEvent};
use crate::limits;

/// M7's checks that need no database.
///
/// # Errors
/// 400 `bad_request` / `too_many_attachments`, 413 `too_large`.
pub fn validate(m: &SendRequest) -> Result<()> {
    if m.client_message_id.trim().is_empty() || m.client_message_id.len() > limits::IDEMPOTENCY_KEY_MAX {
        return Err(Error::bad_request("clientMessageId must be 1-200 bytes"));
    }
    if m.text.len() > limits::TURN_TEXT_MAX_BYTES {
        return Err(Error::code(ErrorCode::TooLarge, "the message is over 1 MiB"));
    }
    if m.images.len() > limits::ATTACHMENTS_MAX || m.files.len() > limits::ATTACHMENTS_MAX {
        return Err(Error::code(
            ErrorCode::TooManyAttachments,
            "at most 10 images and 10 files per message",
        ));
    }
    if !super::has_content(m) {
        return Err(Error::bad_request("the message has no text and no attachments"));
    }
    Ok(())
}

/// The outcome of M7: the answer, and whether the turn is new (its driver is then started by the caller).
#[derive(Debug, Clone)]
pub struct Sent {
    /// 202 or 200.
    pub answer: Admitted<SendResponse>,
}

impl Sent {
    /// The new turn, when this call admitted it (a message that joined a running turn started none).
    #[must_use]
    pub fn new_turn(&self) -> Option<Uuid> {
        match &self.answer {
            Admitted::New(r) if !r.mid_turn => Some(r.turn.id),
            _ => None,
        }
    }
}

/// What a send does while a turn runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhileRunning {
    /// The message joins the running turn ([`super::midturn`]): a person's send.
    Join,
    /// 409 `turn_running`: the deployment's send (hermit's route).
    Refuse,
}

/// M7.
///
/// # Errors
/// 404, 409 `conversation_deleted` / `turn_running`, 400, 413; a statement error.
pub async fn send(pool: &Pool, person: &Person, id: Uuid, req: SendRequest, running: WhileRunning) -> Result<Sent> {
    validate(&req)?;
    let req = &req; // borrowed: the transaction may run more than once (aviary_pg::tx!)
    let answer = aviary_pg::tx!(pool, |t| -> Result<Admitted<SendResponse>> {
        let c = lock(t, &person.email, id).await?;
        if c.deleted_at.is_some() {
            return Err(Error::code(
                ErrorCode::ConversationDeleted,
                "the conversation is in Recently deleted",
            ));
        }
        if let Some(r) = t
            .query_opt(
                "select id, created_at from turns where conversation_id = $1 and client_message_id = $2",
                &[&id, &req.client_message_id],
            )
            .await?
        {
            return Ok(Admitted::Repeat(SendResponse {
                turn: TurnStarted {
                    id: r.get(0),
                    started_at: Timestamp(r.get(1)),
                },
                mid_turn: false,
            }));
        }
        if let Some(r) = super::midturn::repeat(t, id, &req.client_message_id).await? {
            return Ok(Admitted::Repeat(r));
        }
        let state: String = t
            .query_one("select run_state from conversations where id = $1", &[&id])
            .await?
            .get(0);
        if matches!(state.as_str(), "admitted" | "preparing" | "running" | "stopping") {
            if running == WhileRunning::Join
                && let Some(r) = super::midturn::join(t, id, person, req).await?
            {
                return Ok(Admitted::New(r));
            }
            return Err(Error::code(ErrorCode::TurnRunning, "a turn is already running"));
        }
        ensure_person(t, &person.email).await?;
        let turn = admit(t, id, person, req).await?;
        Ok(Admitted::New(SendResponse { turn, mid_turn: false }))
    })
    .await?;
    Ok(Sent { answer })
}

/// Whether the conversation already has the turn of `client_message_id` (a repeat admits nothing).
///
/// # Errors
/// When the statement fails.
pub async fn is_repeat(pool: &Pool, id: Uuid, client_message_id: &str) -> Result<bool> {
    Ok(pool
        .get()
        .await?
        .query_opt(
            "select 1 from turns where conversation_id = $1 and client_message_id = $2",
            &[&id, &client_message_id],
        )
        .await?
        .is_some())
}

/// An upload being attached.
struct Upload {
    id: Uuid,
    role: String,
    name: String,
    mime: String,
    size: i64,
    blob: Vec<u8>,
    model_blob: Option<Vec<u8>>,
    width: Option<i32>,
    height: Option<i32>,
}

/// The upload id inside an M13 path (`uploads/<uploadId>/<name>`).
fn upload_id_of(path: &str) -> Option<Uuid> {
    let rest = path.strip_prefix("uploads/")?;
    let (id, name) = rest.split_once('/')?;
    if name.is_empty() {
        return None;
    }
    Uuid::parse_str(id).ok()
}

/// The longest prefix of `s` within `chars` characters and `bytes` UTF-8 bytes (whole characters only).
fn fit(s: &str, chars: usize, bytes: usize) -> &str {
    let mut end = 0;
    for (n, (i, c)) in s.char_indices().enumerate() {
        if n >= chars || i + c.len_utf8() > bytes {
            break;
        }
        end = i + c.len_utf8();
    }
    &s[..end]
}

/// A file name made safe for `/workspace/uploads/` (magpie 8.2): `/` and NUL replaced, leading dots replaced, at most
/// 200 characters and 255 bytes (one directory entry: a Traditional Chinese title of 200 characters is 600 bytes).
#[must_use]
pub fn safe_name(name: &str) -> String {
    // trimmed first, so no leading space hides a leading dot (and `.` / `..` become `_` / `__`)
    let s: String = name
        .trim()
        .chars()
        .map(|c| if c == '/' || c == '\0' { '_' } else { c })
        .collect();
    let dots = s.chars().take_while(|c| *c == '.').count();
    let s = format!("{}{}", "_".repeat(dots), &s[dots..]);
    let s = if s.is_empty() { "file".to_owned() } else { s };
    // the extension is kept when the name is cut
    let (base, ext) = match s.rfind('.') {
        Some(i) if i > 0 && s.len() - i <= 16 => (&s[..i], &s[i..]),
        _ => (s.as_str(), ""),
    };
    let room_chars = limits::WORKSPACE_NAME_MAX_CHARS.saturating_sub(ext.chars().count());
    let room_bytes = WORKSPACE_NAME_MAX_BYTES.saturating_sub(ext.len());
    let cut = fit(base, room_chars, room_bytes).trim_end();
    if cut.len() == base.len() {
        return s.trim_end().to_owned();
    }
    let out = format!("{cut}{ext}");
    if out.is_empty() { "file".to_owned() } else { out }
}

/// `name`, or `name (2).ext`, `name (3).ext`, … — the first not in `taken`, at most 200 characters and 255 bytes.
#[must_use]
pub fn unique_name<S: std::hash::BuildHasher>(name: &str, taken: &HashSet<String, S>) -> String {
    if !taken.contains(name) {
        return name.to_owned();
    }
    let (base, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 2.. {
        let suffix = format!(" ({n}){ext}");
        let room = limits::WORKSPACE_NAME_MAX_CHARS.saturating_sub(suffix.chars().count());
        let room_bytes = WORKSPACE_NAME_MAX_BYTES.saturating_sub(suffix.len());
        let candidate = format!("{}{suffix}", fit(base, room, room_bytes));
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("the loop above returns")
}

async fn blob_pointer(tx: &Tx<'_>, sha: &[u8]) -> Result<BlobPointer> {
    let r = tx
        .query_opt(
            "select size, kind, mime, width, height from blobs where sha256 = $1",
            &[&sha],
        )
        .await?
        .ok_or_else(|| Error::Internal("an upload's blob row is missing".into()))?;
    let kind = match r.get::<_, String>(1).as_str() {
        "media" => BlobKind::Media,
        "text" => BlobKind::Text,
        _ => BlobKind::File,
    };
    Ok(BlobPointer {
        blob: BlobRef {
            blob_id: hex(sha),
            bytes: u64::try_from(r.get::<_, i64>(0)).unwrap_or(0),
            kind,
            mime: r.get(2),
            width: r.get::<_, Option<i32>>(3).and_then(|v| u32::try_from(v).ok()),
            height: r.get::<_, Option<i32>>(4).and_then(|v| u32::try_from(v).ok()),
            preview: None,
            lines: None,
        },
    })
}

fn unknown_upload() -> Error {
    Error::code(
        ErrorCode::UnknownUpload,
        "an attachment names no unattached upload of the person",
    )
}

/// The person's unattached uploads the message names, locked, in the message's order (images first).
async fn uploads(tx: &Tx<'_>, person: &str, m: &SendRequest) -> Result<(Vec<Upload>, Vec<Upload>)> {
    let image_ids: Vec<Uuid> = m.images.iter().map(|i| i.upload_id).collect();
    let mut file_ids = Vec::new();
    for f in &m.files {
        file_ids.push(upload_id_of(&f.path).ok_or_else(unknown_upload)?);
    }
    let mut seen = HashSet::new();
    let mut all: Vec<Uuid> = Vec::new();
    for id in image_ids.iter().chain(&file_ids) {
        if seen.insert(*id) {
            all.push(*id);
        }
    }
    if all.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let rows = tx
        .query(
            "select id, role, name, mime, size, blob, model_blob, width, height from uploads
             where id = any($1) and person = $2 and conversation_id is null
             order by id for update",
            &[&all, &person],
        )
        .await?;
    if rows.len() != all.len() {
        return Err(unknown_upload());
    }
    let by_id = |id: Uuid| {
        rows.iter().find(|r| r.get::<_, Uuid>(0) == id).map(|r| Upload {
            id,
            role: r.get(1),
            name: r.get(2),
            mime: r.get(3),
            size: r.get(4),
            blob: r.get(5),
            model_blob: r.get(6),
            width: r.get(7),
            height: r.get(8),
        })
    };
    let mut used = HashSet::new();
    let mut images = Vec::new();
    for id in image_ids {
        if !used.insert(id) {
            continue;
        }
        let u = by_id(id).ok_or_else(unknown_upload)?;
        if u.role != "image" || u.model_blob.is_none() {
            return Err(unknown_upload());
        }
        images.push(u);
    }
    let mut files = Vec::new();
    for id in file_ids {
        if !used.insert(id) {
            continue;
        }
        files.push(by_id(id).ok_or_else(unknown_upload)?);
    }
    Ok((images, files))
}

/// Admits `m` as a person turn of `conversation`, whose row the caller has locked and checked (magpie 8.2 steps 4-5).
///
/// # Errors
/// 400 `unknown_upload`; a statement error.
#[allow(clippy::too_many_lines)] // the steps of magpie 8.2, in its order
pub async fn admit(tx: &Tx<'_>, conversation: Uuid, person: &Person, m: &SendRequest) -> Result<TurnStarted> {
    let (images, files) = uploads(tx, &person.email, m).await?;
    let mut taken: HashSet<String> = tx
        .query(
            "select workspace_path from uploads where conversation_id = $1 and workspace_path is not null",
            &[&conversation],
        )
        .await?
        .iter()
        .filter_map(|r| r.get::<_, String>(0).strip_prefix("uploads/").map(str::to_owned))
        .collect();
    let mut placed = Vec::new();
    for u in images.iter().chain(&files) {
        let name = unique_name(&safe_name(&u.name), &taken);
        taken.insert(name.clone());
        placed.push((u, format!("uploads/{name}")));
    }
    let mut ev_images = Vec::new();
    let mut ev_files = Vec::new();
    let mut start_images = Vec::new();
    let mut start_files = Vec::new();
    for (u, path) in &placed {
        let blob = blob_pointer(tx, &u.blob).await?;
        let size = u64::try_from(u.size).unwrap_or(0);
        if let (true, Some(model)) = (u.role == "image" && images.iter().any(|i| i.id == u.id), &u.model_blob) {
            let model_ptr = blob_pointer(tx, model).await?;
            start_images.push(StartImage {
                upload_id: u.id,
                mime_type: u.mime.clone(),
                workspace_path: path.clone(),
                blob: hex(&u.blob),
                model_blob: hex(model),
                model_mime: model_ptr.blob.mime.clone().unwrap_or_else(|| u.mime.clone()),
            });
            ev_images.push(AttachedImage {
                upload_id: u.id,
                mime_type: u.mime.clone(),
                size,
                width: u.width.and_then(|v| u32::try_from(v).ok()),
                height: u.height.and_then(|v| u32::try_from(v).ok()),
                path: Some(path.clone()),
                blob,
                model_blob: model_ptr,
            });
        } else {
            start_files.push(StartFile {
                upload_id: u.id,
                name: u.name.clone(),
                mime_type: u.mime.clone(),
                size,
                workspace_path: path.clone(),
                blob: hex(&u.blob),
            });
            ev_files.push(Attachment {
                upload_id: u.id,
                name: u.name.clone(),
                mime_type: u.mime.clone(),
                size,
                path: path.clone(),
                blob,
            });
        }
    }
    let now = OffsetDateTime::now_utc();
    let sent = EventBody::UserSent(UserSent {
        client_message_id: m.client_message_id.clone(),
        text: m.text.clone(),
        files: ev_files,
        images: ev_images,
    });
    let turn_id = Uuid::now_v7();
    let mut ev = NewEvent::of(&sent)?
        .turn(Some(turn_id))
        .viewer(person.viewer_email())
        .text(Some(m.text.clone()));
    ev.is_prompt = true;
    ev.at = now;
    let user_seq = events::append(tx, conversation, ev).await?;
    for (u, path) in &placed {
        tx.execute(
            "update uploads set conversation_id = $2, workspace_path = $3, attached_seq = $4 where id = $1",
            &[&u.id, &conversation, path, &user_seq],
        )
        .await?;
        for sha in std::iter::once(&u.blob).chain(u.model_blob.as_ref()) {
            tx.execute(
                "insert into blob_refs (owner, owner_kind, blob) values ($1, 'conversation', $2)
                 on conflict do nothing",
                &[&conversation, sha],
            )
            .await?;
        }
    }
    if let Some(v) = &person.viewer {
        let chars = i32::try_from(m.text.chars().count()).unwrap_or(i32::MAX);
        tx.execute(
            "insert into view_as_log (admin, person, session, event, conversation_id, seq, chars)
             values ($1, $2, $3, 'send', $4, $5, $6)",
            &[&v.email, &person.email, &v.session, &conversation, &user_seq, &chars],
        )
        .await?;
    }
    let started = EventBody::TurnStarted(TurnStartedData {
        turn_id,
        origin: TurnOrigin::Person,
    });
    let mut ev = NewEvent::of(&started)?.turn(Some(turn_id));
    ev.at = now;
    let start_seq = events::append(tx, conversation, ev).await?;
    let conv = tx
        .query_one(
            "select provider_session_id, name_source, title_attempts, picked_model from conversations where id = $1",
            &[&conversation],
        )
        .await?;
    tx.execute(
        "insert into turns (id, conversation_id, origin, state, client_message_id, user_seq, start_seq, created_at,
                            deadline)
         values ($1, $2, 'person', 'admitted', $3, $4, $5, $6, $6::timestamptz + make_interval(secs => $7::float8))",
        &[
            &turn_id,
            &conversation,
            &m.client_message_id,
            &user_seq,
            &start_seq,
            &now,
            &secs(limits::ADMIT_TIMEOUT),
        ],
    )
    .await?;
    let payload = StartTurn {
        text: m.text.clone(),
        files: start_files,
        images: start_images,
        resume: Resume {
            provider_session_id: conv.get(0),
        },
        sent_at: Timestamp(now),
        answers: Vec::new(),
        model: conv.get(3),
    };
    let cmd = command(
        tx,
        conversation,
        "start_turn",
        Some(turn_id),
        &serde_json::to_value(&payload)?,
    )
    .await?;
    tx.execute("update turns set start_cmd_seq = $2 where id = $1", &[&turn_id, &cmd])
        .await?;
    let name_source: Option<String> = conv.get(1);
    let attempts: i16 = conv.get(2);
    if name_source.is_none() && attempts < limits::TITLE_ATTEMPTS {
        let pending = tx
            .query_opt(
                "select 1 from commands where conversation_id = $1 and type = 'title'
                   and acked_at is null and canceled_at is null",
                &[&conversation],
            )
            .await?;
        if pending.is_none() {
            let first: Option<String> = tx
                .query_opt(
                    "select data->>'text' from events where conversation_id = $1 and kind = 'user.sent'
                     order by seq limit 1",
                    &[&conversation],
                )
                .await?
                .and_then(|r| r.get(0));
            let title = TitleCommand {
                user_prompt: titles::prompt(first.as_deref().unwrap_or(&m.text)),
                max_tokens: limits::TITLE_MAX_TOKENS,
            };
            command(tx, conversation, "title", None, &serde_json::to_value(&title)?).await?;
        }
    }
    tx.execute(
        "update conversations set run_state = 'admitted', active_turn = $2, activity_at = $3,
                last_user_message_at = $3, sandbox_phase = null
         where id = $1",
        &[&conversation, &turn_id, &now],
    )
    .await?;
    turn_change(
        tx,
        &person.email,
        conversation,
        turn_id,
        TurnPhase::Started,
        TurnOrigin::Person,
        None,
    )
    .await?;
    changes::upserted(tx, &person.email, conversation).await?;
    Ok(TurnStarted {
        id: turn_id,
        started_at: Timestamp(now),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{safe_name, unique_name, upload_id_of};

    #[test]
    fn workspace_names_are_safe_and_unique() {
        assert_eq!(safe_name("Q3 report.pdf"), "Q3 report.pdf");
        assert_eq!(safe_name("../etc/passwd"), "___etc_passwd");
        assert_eq!(safe_name(".bashrc"), "_bashrc");
        assert_eq!(safe_name("a\0b"), "a_b");
        assert_eq!(safe_name("   "), "file");
        assert_eq!(safe_name(" .bashrc"), "_bashrc");
        assert_eq!(safe_name(" .."), "__");
        assert_eq!(safe_name(" . "), "_");
        assert_eq!(safe_name("."), "_");
        assert_eq!(safe_name(".."), "__");
        assert_eq!(safe_name("\t../x"), "___x");
        assert_eq!(safe_name("  Q3 report.pdf  "), "Q3 report.pdf");
        assert_eq!(safe_name(&"x".repeat(300)).chars().count(), 200);
        // a long Traditional Chinese title: cut to one directory entry (255 bytes), its extension kept
        let title = format!("{}.pdf", "香港公司年度業務報告".repeat(10));
        let cut = safe_name(&title);
        assert!(
            cut.len() <= 255 && cut.rsplit_once('.').map(|x| x.1) == Some("pdf"),
            "{cut} {}",
            cut.len()
        );
        assert_eq!(cut.chars().count(), 83 + 4);
        assert!(title.starts_with(cut.trim_end_matches(".pdf")));
        let mut t = HashSet::new();
        t.insert(cut.clone());
        let again = unique_name(&cut, &t);
        assert!(
            again.len() <= 255 && again.rsplit_once(" (2).").map(|x| x.1) == Some("pdf"),
            "{again}"
        );
        let mut taken = HashSet::new();
        assert_eq!(unique_name("a.pdf", &taken), "a.pdf");
        taken.insert("a.pdf".to_owned());
        assert_eq!(unique_name("a.pdf", &taken), "a (2).pdf");
        taken.insert("a (2).pdf".to_owned());
        assert_eq!(unique_name("a.pdf", &taken), "a (3).pdf");
        taken.insert("README".to_owned());
        assert_eq!(unique_name("README", &taken), "README (2)");
        let long = format!("{}.txt", "x".repeat(196));
        taken.insert(long.clone());
        let u = unique_name(&long, &taken);
        assert!(u.ends_with(" (2).txt") && u.chars().count() <= 200, "{u}");
    }

    #[test]
    fn upload_paths_name_their_upload() {
        let id = uuid::Uuid::new_v4();
        assert_eq!(upload_id_of(&format!("uploads/{id}/Q3 report.pdf")), Some(id));
        assert_eq!(upload_id_of(&format!("uploads/{id}/")), None);
        assert_eq!(upload_id_of("cloudcli-assets/x/y.pdf"), None);
        assert_eq!(upload_id_of("uploads/not-a-uuid/y.pdf"), None);
    }
}
