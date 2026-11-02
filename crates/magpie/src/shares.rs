//! Shares (M16, magpie 12): a frozen copy of a conversation up to its last finished turn — its own timeline rows
//! (`share_rows`), its own Claude Code scope (`snapshot`), blob references of its own, and its own workspace, a
//! copy-on-write clone in hermit (`workspace_id = uuidv5(SHARE_NS, "share:" + id)`). A later change, revoke or purge
//! of the source never reaches the copy, and a continue copies the share, not the source.
//!
//! hermit's clone is made before the transaction (a transaction never calls out, platform 4.3), with its workspace id
//! recorded first in `pending_workspaces` so hermit's cleanup sees it as referenced; the transaction deletes that row,
//! and one that fails queues the clone's deletion in the outbox.
//!
//! A frozen row is one whose source events are all within the share (`upto_seq`); a row of a finished turn that an
//! event of the running turn has changed since is left out. A continue copies the share's rows and snapshot, the
//! model recorded at the freeze, and from the source the bookkeeping of the share's own events (their seqs are at
//! most `upto_seq`, and retractions made by later events are left out); the source must still be live.

use aviary_pg::Tx;
use base64::Engine as _;
use magpie_api::Timestamp;
use magpie_api::errors::ErrorCode;
use magpie_api::rows::{Row, RowItem};
use magpie_api::shares::{ContinueResponse, ShareRef, ShareView, SharesList};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::Magpie;
use crate::changes;
use crate::conversations::{Person, ensure_person, lock};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::jobs::outbox::{self, Item};

/// The namespace of share workspace ids (magpie 12; the same as hermit's and the cutover's).
pub const SHARE_NS: Uuid = Uuid::from_u128(0x6f1c_9d2e_0b7a_4f53_9a43_5b2f_3f1e_8c61);

/// A share's frozen workspace.
#[must_use]
pub fn workspace_of(share_id: &str) -> Uuid {
    Uuid::new_v5(&SHARE_NS, format!("share:{share_id}").as_bytes())
}

/// A new share id: 16 random bytes, base64url, 22 characters.
///
/// # Errors
/// [`Error::Internal`] when the system has no randomness.
pub fn new_id() -> Result<String> {
    use ring::rand::SecureRandom as _;
    let mut b = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut b)
        .map_err(|_| Error::Internal("no randomness".into()))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b))
}

fn valid_id(id: &str) -> bool {
    id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The local part of an email (a share's `ownerName`).
#[must_use]
pub fn local_part(email: &str) -> &str {
    email.split('@').next().unwrap_or(email)
}

/// Copies cc scope `from` into `to` (files, lines, pages; a scope is a conversation or a share snapshot). With
/// `cut = Some((path, ord))` the lines of `path` from `ord` on are left out and that file ends where they began.
async fn copy_scope(t: &Tx<'_>, from: Uuid, to: Uuid, cut: Option<(&str, i64)>) -> Result<()> {
    t.execute(
        "insert into cc_files (scope, path, kind, mode, mtime_ns, size, version, tail, next_ord, last_op_at)
         select $2, path, kind, mode, mtime_ns, size, version, tail, next_ord, last_op_at from cc_files where scope = $1",
        &[&from, &to],
    )
    .await?;
    let (cut_path, cut_ord) = cut.map_or((None, i64::MAX), |(p, o)| (Some(p), o));
    t.execute(
        "insert into cc_lines (file_id, ord, start_off, len, sha256, stub, spans, event_seq, turn_id, classified)
         select n.id, l.ord, l.start_off, l.len, l.sha256, l.stub, l.spans, l.event_seq, l.turn_id, l.classified
         from cc_lines l
         join cc_files o on o.id = l.file_id and o.scope = $1
         join cc_files n on n.scope = $2 and n.path = o.path
         where $3::text is null or o.path <> $3 or l.ord < $4",
        &[&from, &to, &cut_path, &cut_ord],
    )
    .await?;
    t.execute(
        "insert into cc_pages (file_id, page_no, bytes)
         select n.id, p.page_no, p.bytes from cc_pages p
         join cc_files o on o.id = p.file_id and o.scope = $1
         join cc_files n on n.scope = $2 and n.path = o.path",
        &[&from, &to],
    )
    .await?;
    if let Some(path) = cut_path {
        // the cut file ends where its first left-out line began
        t.execute(
            "update cc_files f set tail = '', size = coalesce(
                 (select l.start_off from cc_lines l join cc_files o on o.id = l.file_id
                  where o.scope = $1 and o.path = $3 and l.ord = $4), f.size)
             where f.scope = $2 and f.path = $3",
            &[&from, &to, &path, &cut_ord],
        )
        .await?;
    }
    Ok(())
}

/// Copies `from`'s blob references to `to` (`owner_kind` of `to`), bumping each blob's `last_ref_at`.
async fn copy_refs(t: &Tx<'_>, from: Uuid, to: Uuid, owner_kind: &str) -> Result<()> {
    t.execute(
        "update blobs set last_ref_at = now() where sha256 in (select blob from blob_refs where owner = $1)",
        &[&from],
    )
    .await?;
    t.execute(
        "insert into blob_refs (owner, owner_kind, blob) select $2, $3, blob from blob_refs where owner = $1
         on conflict do nothing",
        &[&from, &to, &owner_kind],
    )
    .await?;
    Ok(())
}

/// Removes a share's frozen data (its snapshot scope, rows and blob references) and queues its workspace's
/// deletion; the share row stays (audit).
async fn drop_frozen(t: &Tx<'_>, share_id: &str, snapshot: Uuid, workspace: Uuid) -> Result<()> {
    t.execute("delete from cc_files where scope = $1", &[&snapshot]).await?;
    t.execute("delete from cc_limbo where scope = $1", &[&snapshot]).await?;
    t.execute("delete from cc_ops where scope = $1", &[&snapshot]).await?;
    t.execute("delete from blob_refs where owner = $1", &[&snapshot])
        .await?;
    t.execute("delete from share_rows where share_id = $1", &[&share_id])
        .await?;
    outbox::enqueue(
        t,
        &Item::DeleteWorkspace {
            workspace_id: workspace,
        },
    )
    .await?;
    Ok(())
}

/// The rows whose source events are all at most `$N` (over `rows` or `share_rows`).
fn within(n: u8) -> String {
    format!("not exists (select 1 from unnest(source_seqs) x where x > ${n})")
}

/// Records `workspace` as a clone about to be made (before calling hermit).
async fn pending(app: &Magpie, workspace: Uuid) -> Result<()> {
    app.pool
        .get()
        .await?
        .execute(
            "insert into pending_workspaces (workspace_id) values ($1) on conflict do nothing",
            &[&workspace],
        )
        .await?;
    Ok(())
}

async fn not_pending(t: &Tx<'_>, workspace: Uuid) -> Result<()> {
    t.execute("delete from pending_workspaces where workspace_id = $1", &[&workspace])
        .await?;
    Ok(())
}

/// Retention (magpie 15): pending clones older than `age` whose creating replica went away; their clone's deletion
/// is queued unless a conversation or a share references it after all. How many.
///
/// # Errors
/// When a statement fails.
pub async fn expire_pending(pool: &aviary_pg::Pool, age: std::time::Duration) -> Result<u64> {
    let secs = age.as_secs_f64();
    aviary_pg::tx!(pool, |t| -> Result<u64> {
        let rows = t
            .query(
                "delete from pending_workspaces where created_at < now() - make_interval(secs => $1)
                 returning workspace_id",
                &[&secs],
            )
            .await?;
        let mut n = 0;
        for r in rows {
            let w: Uuid = r.get(0);
            let referenced = t
                .query_opt(
                    "select 1 from conversations where id = $1 union all
                     select 1 from shares where workspace_id = $1 and revoked_at is null limit 1",
                    &[&w],
                )
                .await?
                .is_some();
            if !referenced {
                outbox::enqueue(t, &Item::DeleteWorkspace { workspace_id: w }).await?;
            }
            n += 1;
        }
        Ok(n)
    })
    .await
}

/// Queues the deletion of a clone whose transaction failed.
async fn drop_clone(app: &Magpie, workspace: Uuid) {
    let r = aviary_pg::tx!(&app.pool, |t| -> Result<()> {
        not_pending(t, workspace).await?;
        outbox::enqueue(
            t,
            &Item::DeleteWorkspace {
                workspace_id: workspace,
            },
        )
        .await
    })
    .await;
    if let Err(e) = r {
        tracing::error!(error = %e, workspace = %workspace, "an orphan clone's deletion was not queued");
    }
}

/// What a freeze takes: the last event seq it includes, and where the main transcript is cut.
struct Freeze {
    upto: i64,
    cut: Option<(String, i64)>,
}

async fn freeze_point(db: &impl Db, conversation: Uuid) -> Result<Freeze> {
    let c = db
        .query_one(
            "select c.head_seq, c.active_turn, c.main_transcript, t.user_seq, t.start_seq
             from conversations c left join turns t on t.id = c.active_turn where c.id = $1",
            &[&conversation],
        )
        .await?;
    let head: i64 = c.get(0);
    let active: Option<Uuid> = c.get(1);
    let main: Option<String> = c.get(2);
    let Some(turn) = active else {
        return Ok(Freeze { upto: head, cut: None });
    };
    // only finished turns are shared: up to the event just before the running turn's user.sent
    let first: Option<i64> = c.get::<_, Option<i64>>(3).or(c.get(4));
    let upto = first.map_or(head, |s| s - 1);
    let cut = match main {
        Some(path) => db
            .query_opt(
                "select min(l.ord) from cc_lines l join cc_files f on f.id = l.file_id
                 where f.scope = $1 and f.path = $2 and l.turn_id = $3",
                &[&conversation, &path, &turn],
            )
            .await?
            .and_then(|r| r.get::<_, Option<i64>>(0))
            .map(|ord| (path, ord)),
        None => None,
    };
    Ok(Freeze { upto, cut })
}

/// M16 `POST /v1/conversations/{id}/shares`: freezes the conversation up to its last finished turn.
///
/// # Errors
/// 403 `view_as_read_only`, 404, 409 `nothing_to_share` / `conversation_migrating`, 500 `share_failed` when hermit
/// cannot clone the workspace; a statement error.
pub async fn create(app: &Magpie, person: &Person, conversation: Uuid) -> Result<ShareRef> {
    person.refuse_view_as()?;
    let conn = app.pool.get().await?;
    let c = crate::conversations::load_owned(&conn, &person.email, conversation).await?;
    if c.migrating {
        let mut r = magpie_api::errors::Refusal::legacy_route();
        r.retry_after_ms = Some(crate::limits::MIGRATING_RETRY_AFTER_MS);
        return Err(r.into());
    }
    let f = freeze_point(&conn, conversation).await?;
    let any = conn
        .query_opt(
            &format!(
                "select 1 from rows where conversation_id = $1 and kind = 'assistant' and {} limit 1",
                within(2)
            ),
            &[&conversation, &f.upto],
        )
        .await?
        .is_some();
    drop(conn);
    if !any {
        return Err(Error::code(ErrorCode::NothingToShare, "no finished reply to share"));
    }
    let id = new_id()?;
    let workspace = workspace_of(&id);
    let clone = hermit_api::workspaces::CloneRequest { from: conversation };
    pending(app, workspace).await?;
    if let Err(e) = app.hermit.clone_workspace(workspace, &clone).await {
        tracing::warn!(error = %e, conversation = %conversation, "share: hermit could not clone the workspace");
        drop_clone(app, workspace).await;
        return Err(Error::code(ErrorCode::ShareFailed, "the workspace could not be copied"));
    }
    let snapshot = Uuid::new_v4();
    let owner_name = local_part(&person.email).to_owned();
    let idr = &id;
    let r = aviary_pg::tx!(&app.pool, |t| -> Result<OffsetDateTime> {
        let l = lock(t, &person.email, conversation).await?;
        l.refuse_migrating()?;
        // the freeze point again, under the lock (a turn may have started meanwhile)
        let f = freeze_point(t, conversation).await?;
        let row = t
            .query_one(
                "insert into shares (id, conversation_id, owner, owner_name, title, upto_seq, snapshot,
                                     provider_session_id, workspace_id, model)
                 select $1, c.id, c.person, $3, c.title, $4, $5, c.provider_session_id, $6, c.model
                 from conversations c where c.id = $2
                 returning created_at",
                &[idr, &conversation, &owner_name, &f.upto, &snapshot, &workspace],
            )
            .await?;
        let frozen = format!(
            "insert into share_rows (share_id, row_id, seq_end, kind, at, turn_id, item, seqs, source_seqs,
                                     tool_use_id, message_id, block_index, is_prompt)
             select $1, row_id, seq_end, kind, at, turn_id, item, seqs, source_seqs, tool_use_id, message_id,
                    block_index, is_prompt
             from rows where conversation_id = $2 and {}",
            within(3)
        );
        t.execute(&frozen, &[idr, &conversation, &f.upto]).await?;
        let cut = f.cut.as_ref().map(|(p, o)| (p.as_str(), *o));
        copy_scope(t, conversation, snapshot, cut).await?;
        copy_refs(t, conversation, snapshot, "share").await?;
        not_pending(t, workspace).await?;
        Ok(row.get(0))
    })
    .await;
    match r {
        Ok(at) => {
            tracing::info!(conversation = %conversation, "share created");
            Ok(ShareRef {
                id,
                created_at: Timestamp(at),
            })
        }
        Err(e) => {
            drop_clone(app, workspace).await;
            Err(e)
        }
    }
}

/// M16 `GET /v1/conversations/{id}/shares`: the live links, newest first.
///
/// # Errors
/// 404; a statement error.
pub async fn list(db: &impl Db, person: &Person, conversation: Uuid) -> Result<SharesList> {
    crate::conversations::load_owned(db, &person.email, conversation).await?;
    let rows = db
        .query(
            "select id, created_at from shares where conversation_id = $1 and owner = $2 and revoked_at is null
             order by created_at desc, id",
            &[&conversation, &person.email],
        )
        .await?;
    Ok(SharesList {
        shares: rows
            .into_iter()
            .map(|r| ShareRef {
                id: r.get(0),
                created_at: Timestamp(r.get(1)),
            })
            .collect(),
    })
}

/// M16 `DELETE /v1/shares/{shareId}`: the owner revokes a live link.
///
/// # Errors
/// 403 `view_as_read_only`; 404 for anyone else's, an unknown or a revoked link; a statement error.
pub async fn revoke(app: &Magpie, person: &Person, share_id: &str) -> Result<()> {
    person.refuse_view_as()?;
    if !valid_id(share_id) {
        return Err(Error::not_found());
    }
    aviary_pg::tx!(&app.pool, |t| -> Result<()> {
        let row = t
            .query_opt(
                "update shares set revoked_at = now(), revoke_reason = 'owner'
                 where id = $1 and owner = $2 and revoked_at is null
                 returning snapshot, workspace_id",
                &[&share_id, &person.email],
            )
            .await?
            .ok_or_else(Error::not_found)?;
        drop_frozen(t, share_id, row.get(0), row.get(1)).await?;
        Ok(())
    })
    .await?;
    tracing::info!("share revoked");
    Ok(())
}

/// A live share as a reader sees it: 404 for a revoked share, one whose source is in Recently deleted or was purged.
#[derive(Debug, Clone)]
struct Live {
    id: String,
    conversation: Uuid,
    owner: String,
    owner_name: String,
    title: Option<String>,
    upto: i64,
    snapshot: Uuid,
    provider_session_id: Option<String>,
    workspace: Uuid,
    created_at: OffsetDateTime,
}

async fn live(db: &impl Db, share_id: &str) -> Result<Live> {
    if !valid_id(share_id) {
        return Err(Error::not_found());
    }
    let r = db
        .query_opt(
            "select s.id, s.conversation_id, s.owner, s.owner_name, s.title, s.upto_seq, s.snapshot,
                    s.provider_session_id, s.workspace_id, s.created_at
             from shares s join conversations c on c.id = s.conversation_id
             where s.id = $1 and s.revoked_at is null and c.deleted_at is null",
            &[&share_id],
        )
        .await?
        .ok_or_else(Error::not_found)?;
    Ok(Live {
        id: r.get(0),
        conversation: r.get(1),
        owner: r.get(2),
        owner_name: r.get(3),
        title: r.get(4),
        upto: r.get(5),
        snapshot: r.get(6),
        provider_session_id: r.get(7),
        workspace: r.get(8),
        created_at: r.get(9),
    })
}

fn u(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

/// M16 `GET /v1/shares/{shareId}`: every frozen row, ascending by `seqStart`, for any signed-in person.
///
/// # Errors
/// 404; a statement error.
pub async fn view(db: &impl Db, person: &Person, share_id: &str) -> Result<ShareView> {
    let s = live(db, share_id).await?;
    let rows = db
        .query(
            "select row_id, seq_end, seqs, at, turn_id, item from share_rows where share_id = $1 order by row_id",
            &[&s.id],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let row_id: i64 = r.get(0);
        let seq_end: i64 = r.get(1);
        let seqs: Vec<i64> = r.get(2);
        let item: RowItem = serde_json::from_value(r.get(5))?;
        out.push(Row {
            seq_start: u(row_id),
            seq_end: u(seq_end),
            seqs: seqs.into_iter().filter(|x| *x <= seq_end).map(u).collect(),
            updated: seq_end != row_id,
            timestamp: Timestamp(r.get(3)),
            turn_id: r.get(4),
            item,
        });
    }
    Ok(ShareView {
        id: s.id,
        title: s.title,
        owner_name: s.owner_name,
        is_owner: s.owner == person.email,
        created_at: Timestamp(s.created_at),
        epoch: "1".into(),
        rows: out,
    })
}

/// M16 `GET /v1/shares/{shareId}/files/raw`: with `sent`, a sent file of a `files.sent` event within the share;
/// without, an attachment of a `user.sent` within it by its `uploads/<name>` path, else the file from the share's
/// frozen workspace. Never the live workspace.
///
/// # Errors
/// 400 `bad_path`, 404, 503; a statement error.
pub async fn files_raw(
    app: &Magpie,
    ctx: &magpie_api::server::Ctx,
    share_id: &str,
    q: &magpie_api::files::RawFileQuery,
) -> Result<axum::response::Response> {
    let path = crate::files::relative_path(&q.path)?;
    let conn = app.pool.get().await?;
    let s = live(&conn, share_id).await?;
    let blob = match &q.sent {
        Some(sent) => Some(
            conn.query_opt(
                "select blob from sent_files
                 where conversation_id = $1 and tool_use_id = $2 and path = $3 and seq <= $4",
                &[&s.conversation, sent, &path, &s.upto],
            )
            .await?
            .ok_or_else(Error::not_found)?,
        ),
        None => {
            conn.query_opt(
                "select blob from uploads
                 where conversation_id = $1 and workspace_path = $2 and attached_seq <= $3",
                &[&s.conversation, &path, &s.upto],
            )
            .await?
        }
    };
    drop(conn);
    match blob {
        Some(r) => crate::files::snapshot_download(app, ctx, &path, q.inline, &r.get::<_, Vec<u8>>(0)).await,
        None => crate::files::workspace_download(app, ctx, s.workspace, &path, q.inline).await,
    }
}

/// M16 `POST /v1/shares/{shareId}/continue` (「繼續對話」): a new conversation for the viewer from the share — its
/// events, rows, Claude Code state, blob references, sent files and tool links, with their seqs — and a clone of
/// the share's frozen workspace. The sandbox is not carried over.
///
/// # Errors
/// 403 `view_as_read_only`, 404, 500 `continue_failed` when hermit cannot clone; a statement error.
#[allow(clippy::too_many_lines)] // one transaction, in the order magpie 12.4 lists
pub async fn continue_share(app: &Magpie, person: &Person, share_id: &str) -> Result<ContinueResponse> {
    person.refuse_view_as()?;
    let s = live(&app.pool.get().await?, share_id).await?;
    let id = Uuid::new_v4();
    let clone = hermit_api::workspaces::CloneRequest { from: s.workspace };
    pending(app, id).await?;
    if let Err(e) = app.hermit.clone_workspace(id, &clone).await {
        tracing::warn!(error = %e, "continue: hermit could not clone the share's workspace");
        drop_clone(app, id).await;
        return Err(Error::code(
            ErrorCode::ContinueFailed,
            "the workspace could not be copied",
        ));
    }
    let main = s
        .provider_session_id
        .as_ref()
        .map(|sid| format!("projects/-workspace/{sid}.jsonl"));
    let origin = serde_json::json!({"kind": "continue", "shareId": s.id});
    let sr = &s;
    let (main_r, origin_r) = (&main, &origin);
    let r = aviary_pg::tx!(&app.pool, |t| -> Result<()> {
        // the share as it is now, under its row lock and its source's (a revoke, a delete or a purge may have come
        // first; the source's lock keeps it live until this commits)
        // the source first, as a delete or a purge locks it before its shares
        t.query_opt(
            "select 1 from conversations where id = $1 and deleted_at is null for share",
            &[&sr.conversation],
        )
        .await?
        .ok_or_else(Error::not_found)?;
        let model = t
            .query_opt(
                "select model from shares where id = $1 and revoked_at is null for update",
                &[&sr.id],
            )
            .await?
            .ok_or_else(Error::not_found)?;
        let model: Option<String> = model.get(0);
        ensure_person(t, &person.email).await?;
        let row_head: i64 = t
            .query_one(
                "select coalesce(max(seq_end), 0) from share_rows where share_id = $1",
                &[&sr.id],
            )
            .await?
            .get(0);
        t.execute(
            "insert into conversations (id, person, title, name_source, created_at, activity_at, head_seq, row_head,
                                        provider_session_id, main_transcript, model, origin)
             values ($1, $2, $3, 'title', now(), now(), $4, $5, $6, $7, $8, $9)",
            &[
                &id,
                &person.email,
                &sr.title,
                &sr.upto,
                &row_head,
                &sr.provider_session_id,
                main_r,
                &model,
                origin_r,
            ],
        )
        .await?;
        t.execute(
            "insert into events (conversation_id, seq, kind, at, turn_id, data, meta, text, is_prompt, viewer)
             select $1, seq, kind, at, turn_id, data, meta, text, is_prompt, viewer from events
             where conversation_id = $2 and seq <= $3",
            &[&id, &sr.conversation, &sr.upto],
        )
        .await?;
        t.execute(
            "insert into rows (conversation_id, row_id, seq_end, kind, at, turn_id, item, source_seqs, tool_use_id,
                               message_id, block_index, is_prompt, seqs)
             select $1, row_id, seq_end, kind, at, turn_id, item, source_seqs, tool_use_id, message_id, block_index,
                    is_prompt, seqs
             from share_rows where share_id = $2",
            &[&id, &sr.id],
        )
        .await?;
        t.execute(
            "insert into row_seqs (conversation_id, seq, row_id)
             select $1, x, r.row_id from share_rows r cross join lateral unnest(r.seqs) x
             where r.share_id = $2 and x <= r.seq_end",
            &[&id, &sr.id],
        )
        .await?;
        t.execute(
            "insert into retracted (conversation_id, seq, by_seq) select $1, seq, by_seq from retracted
             where conversation_id = $2 and seq <= $3 and coalesce(by_seq, seq) <= $3",
            &[&id, &sr.conversation, &sr.upto],
        )
        .await?;
        t.execute(
            "insert into line_pos (conversation_id, seq, pos) select $1, seq, pos from line_pos
             where conversation_id = $2 and seq <= $3",
            &[&id, &sr.conversation, &sr.upto],
        )
        .await?;
        t.execute(
            "insert into cc_retracted (conversation_id, uuid, seq, by_seq) select $1, uuid, seq, by_seq
             from cc_retracted where conversation_id = $2 and seq <= $3 and coalesce(by_seq, seq) <= $3",
            &[&id, &sr.conversation, &sr.upto],
        )
        .await?;
        t.execute(
            "insert into tool_links (conversation_id, tool_use_id, use_seq, result_seq, name)
             select $1, tool_use_id, use_seq, case when result_seq <= $3 then result_seq end, name from tool_links
             where conversation_id = $2 and use_seq <= $3",
            &[&id, &sr.conversation, &sr.upto],
        )
        .await?;
        t.execute(
            "insert into sent_files (conversation_id, tool_use_id, path, name, size, mime, is_image, blob, caption,
                                     sent_at, seq)
             select $1, tool_use_id, path, name, size, mime, is_image, blob, caption, sent_at, seq from sent_files
             where conversation_id = $2 and seq <= $3",
            &[&id, &sr.conversation, &sr.upto],
        )
        .await?;
        copy_scope(t, sr.snapshot, id, None).await?;
        t.execute(
            "update conversations set cc_bytes =
                 (select coalesce(sum(size), 0)::bigint from cc_files where scope = $1 and kind <> 'dir')
             where id = $1",
            &[&id],
        )
        .await?;
        copy_refs(t, sr.snapshot, id, "conversation").await?;
        changes::upserted(t, &person.email, id).await?;
        not_pending(t, id).await?;
        Ok(())
    })
    .await;
    match r {
        Ok(()) => {
            tracing::info!(conversation = %id, "share continued");
            Ok(ContinueResponse { conversation_id: id })
        }
        Err(e) => {
            drop_clone(app, id).await;
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        let id = new_id().unwrap();
        assert!(valid_id(&id), "{id}");
        assert_ne!(id, new_id().unwrap());
        assert_eq!(local_part("chan.tai.man@example.com"), "chan.tai.man");
        // the convention hermit and the cutover use
        assert_eq!(
            workspace_of("AAAAAAAAAAAAAAAAAAAAAA"),
            Uuid::new_v5(&SHARE_NS, b"share:AAAAAAAAAAAAAAAAAAAAAA")
        );
    }
}
