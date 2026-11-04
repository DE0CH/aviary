//! hermit's routes (magpie 16.3: a leaver's purge, the workspaces magpie still references) and the admin reads
//! (magpie 13: a conversation and its events and timeline for debugging without SQL, each first read per admin,
//! conversation and day audited).

use aviary_pg::Pool;
use magpie_api::Timestamp;
use magpie_api::conversations::ListState;
use magpie_api::errors::ErrorCode;
use magpie_api::events::{Event, EventsPage, EventsQuery};
use magpie_api::ops::{
    AdminConversation, AdminConversationList, AdminHeartbeat, AdminLease, AdminListQuery, LegacyImportStatus,
    PersonPurgeResponse, WORKSPACES_EXISTS_MAX, WorkspacesExistsRequest, WorkspacesExistsResponse,
};
use magpie_api::rows::{TimelinePage, TimelineQuery};
use magpie_api::server::Ctx;
use uuid::Uuid;

use crate::app::Magpie;
use crate::conversations::delete::{ShareRevoke, purge_locked};
use crate::conversations::{self, Locked};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::{rows, viewas};

/// `POST /v1/hermit/persons/{person}/purge` (caller hermit; decision 7: an operator's purge of a leaver): refused
/// with 409 `person_allowed` while hermit still allows the person (asked again here), otherwise every conversation
/// of the person purged as magpie 9.6 (shares revoked `offboarded`) and their unattached uploads deleted.
/// Idempotent; a conversation the importer holds is left for a later call (409 `conversation_migrating`). Each
/// conversation is purged in its own short transaction; a leaver with many conversations takes about 10-50 ms per
/// conversation (a few hundred take seconds), well inside hermit's call timeout, and a retry after a timeout carries
/// on where the last call stopped.
///
/// # Errors
/// 409 `person_allowed` / `conversation_migrating`; 503 when hermit cannot be reached; a statement error.
pub async fn purge_person(app: &Magpie, person: &str) -> Result<PersonPurgeResponse> {
    let person = person.trim().to_lowercase();
    match app.hermit.person(&person).await {
        Ok(p) if p.allowed => {
            return Err(Error::code(ErrorCode::PersonAllowed, "hermit still allows this person"));
        }
        Ok(_) => {}
        // hermit answers 200 for every well-formed address (H2): any other answer, a 404 included (a route missing
        // during version skew), is not a "no" and never leads to a purge
        Err(e) => {
            tracing::warn!(error = %e, "person purge: hermit did not answer");
            return Err(Error::code(ErrorCode::Unavailable, "hermit cannot be reached"));
        }
    }
    let ids: Vec<Uuid> = app
        .pool
        .get()
        .await?
        .query(
            "select id from conversations where person = $1 order by created_at",
            &[&person],
        )
        .await?
        .into_iter()
        .map(|r| r.get(0))
        .collect();
    let mut n = 0u64;
    let mut held = false;
    for id in ids {
        let p = &person;
        let done = aviary_pg::tx!(&app.pool, |t| -> Result<bool> {
            // gone meanwhile (a purge of its own): skipped; any other error fails the purge
            let l = match conversations::lock(t, p, id).await {
                Ok(l) => l,
                Err(e) if e.is(&ErrorCode::NotFound) => return Ok(false),
                Err(e) => return Err(e),
            };
            let Locked { migrating, fenced, .. } = l;
            if migrating || fenced {
                return Err(migrating_refusal());
            }
            purge_locked(t, p, id, ShareRevoke::Offboarded).await?;
            Ok(true)
        })
        .await;
        match done {
            Ok(true) => n += 1,
            Ok(false) => {}
            Err(e) if e.is(&ErrorCode::ConversationMigrating) => held = true,
            Err(e) => return Err(e),
        }
    }
    let p = &person;
    aviary_pg::tx!(&app.pool, |t| -> Result<()> {
        t.execute(
            "delete from blob_refs where owner_kind = 'upload'
               and owner in (select id from uploads where person = $1 and conversation_id is null)",
            &[p],
        )
        .await?;
        t.execute(
            "delete from uploads where person = $1 and conversation_id is null",
            &[p],
        )
        .await?;
        Ok(())
    })
    .await?;
    if held {
        return Err(migrating_refusal());
    }
    tracing::info!(person_key = %aviary_core::log::person_key(&person), conversations = n, "person purged");
    Ok(PersonPurgeResponse { conversations: n })
}

fn migrating_refusal() -> Error {
    let mut r = magpie_api::errors::Refusal::legacy_route();
    r.retry_after_ms = Some(crate::limits::MIGRATING_RETRY_AFTER_MS);
    r.into()
}

/// `POST /v1/hermit/workspaces/exists`: the ids among `ids` that magpie still references — every conversation not
/// purged (Recently deleted included), every live share's frozen workspace, and every clone made for a share or a
/// continue whose row has not committed yet (`pending_workspaces`).
///
/// # Errors
/// 400 for more than 1,000 ids; a statement error.
pub async fn workspaces_exist(db: &impl Db, req: &WorkspacesExistsRequest) -> Result<WorkspacesExistsResponse> {
    if req.ids.len() > WORKSPACES_EXISTS_MAX {
        return Err(Error::bad_request("at most 1,000 ids"));
    }
    let rows = db
        .query(
            "select id from conversations where id = any($1)
             union select workspace_id from shares where revoked_at is null and workspace_id = any($1)
             union select workspace_id from pending_workspaces where workspace_id = any($1)",
            &[&req.ids],
        )
        .await?;
    let found: std::collections::HashSet<Uuid> = rows.into_iter().map(|r| r.get(0)).collect();
    let mut exists: Vec<Uuid> = req.ids.iter().copied().filter(|i| found.contains(i)).collect();
    exists.dedup();
    Ok(WorkspacesExistsResponse { exists })
}

/// `GET /v1/hermit/legacy-import`: the record of the last clean `magpie import verify --all` with every conversation
/// owned (`legacy_import_verified`, written by the importer), and, while the importer's tables still exist (until A6),
/// whether the import changed since: a conversation imported or changed after it, or one not released any more (a
/// reverse hand-over), makes it stale.
///
/// # Errors
/// A statement error.
pub async fn legacy_import(db: &impl Db) -> Result<LegacyImportStatus> {
    let Some(rec) = db
        .query_opt("select verified_at, conversations from legacy_import_verified", &[])
        .await?
    else {
        return Ok(LegacyImportStatus {
            complete: false,
            verified_at: None,
            conversations: 0,
            why: Some("no clean verify --all with every conversation owned is recorded".to_owned()),
        });
    };
    let verified_at: time::OffsetDateTime = rec.get(0);
    let conversations = u64::try_from(rec.get::<_, i64>(1)).unwrap_or(0);
    let has_sources: bool = db
        .query_one("select to_regclass('import_sources') is not null", &[])
        .await?
        .get(0);
    let mut why = None;
    if has_sources {
        let r = db
            .query_one(
                "select count(*), count(*) filter (where released_at is null),
                        count(*) filter (where greatest(last_change_at, first_imported_at) > $1)
                   from import_sources",
                &[&verified_at],
            )
            .await?;
        let (n, open, changed) = (r.get::<_, i64>(0), r.get::<_, i64>(1), r.get::<_, i64>(2));
        if open > 0 {
            why = Some(format!("{open} imported conversations are not released to magpie"));
        } else if changed > 0 {
            why = Some(format!("{changed} imported conversations changed after the verify"));
        } else if u64::try_from(n).unwrap_or(0) != conversations {
            why = Some(format!("{n} conversations are imported, {conversations} were verified"));
        }
    }
    Ok(LegacyImportStatus {
        complete: why.is_none(),
        verified_at: Some(Timestamp(verified_at)),
        conversations,
        why,
    })
}

/// The admin of an admin read (`Aviary-Admin`, required).
///
/// # Errors
/// 400 without it.
pub fn admin_of(ctx: &Ctx) -> Result<String> {
    ctx.admin
        .clone()
        .filter(|a| a.contains('@'))
        .ok_or_else(|| Error::bad_request("Aviary-Admin is required"))
}

async fn admin_view(db: &impl Db, id: Uuid) -> Result<Option<AdminConversation>> {
    let Some((person, conversation)) = conversations::load(db, id).await? else {
        return Ok(None);
    };
    let r = db
        .query_one(
            "select lease_id, lease_epoch, lease_sandbox, lease_granted_at, heartbeat_at, last_heartbeat,
                    hermit_sandbox
             from conversations where id = $1",
            &[&id],
        )
        .await?;
    let lease = match (r.get::<_, Option<Uuid>>(0), r.get::<_, Option<Uuid>>(2)) {
        (Some(lease_id), Some(sandbox)) => Some(AdminLease {
            lease_id,
            epoch: u64::try_from(r.get::<_, i64>(1)).unwrap_or(0),
            sandbox_id: sandbox.to_string(),
            granted_at: Timestamp(
                r.get::<_, Option<time::OffsetDateTime>>(3)
                    .unwrap_or(time::OffsetDateTime::UNIX_EPOCH),
            ),
        }),
        _ => None,
    };
    let heartbeat = match (
        r.get::<_, Option<time::OffsetDateTime>>(4),
        r.get::<_, Option<serde_json::Value>>(5),
    ) {
        (Some(at), Some(mut v)) => {
            if let Some(o) = v.as_object_mut() {
                o.insert("at".into(), serde_json::to_value(Timestamp(at))?);
            }
            serde_json::from_value::<AdminHeartbeat>(v).ok()
        }
        _ => None,
    };
    Ok(Some(AdminConversation {
        conversation,
        person,
        lease,
        heartbeat,
        hermit_sandbox: r.get::<_, Option<Uuid>>(6).map(|s| s.to_string()),
    }))
}

/// `GET /v1/admin/conversations?person=&state=`: audited as a read of every conversation it lists (their titles come
/// from their content: a provisional title is the start of the first message).
///
/// # Errors
/// 400 without `Aviary-Admin`; a statement error.
pub async fn list(pool: &Pool, ctx: &Ctx, q: &AdminListQuery) -> Result<AdminConversationList> {
    let admin = admin_of(ctx)?;
    let db = &pool.get().await?;
    let deleted = matches!(q.state, Some(ListState::Deleted));
    let person = q.person.as_deref().map(|p| p.trim().to_lowercase());
    let ids: Vec<Uuid> = db
        .query(
            "select id from conversations
             where ($1::text is null or person = $1) and (deleted_at is not null) = $2
             order by activity_at desc, id limit 500",
            &[&person, &deleted],
        )
        .await?
        .into_iter()
        .map(|r| r.get(0))
        .collect();
    let mut conversations = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(c) = admin_view(db, id).await? {
            conversations.push(c);
        }
    }
    let read: Vec<(String, Uuid)> = conversations
        .iter()
        .map(|c| (c.person.clone(), c.conversation.id))
        .collect();
    viewas::admin_reads(pool, &admin, &read).await?;
    Ok(AdminConversationList { conversations })
}

/// The conversation an admin reads, its first read today by this admin recorded (`admin_read`).
async fn audited(pool: &Pool, ctx: &Ctx, id: Uuid) -> Result<String> {
    let admin = admin_of(ctx)?;
    let (person, _) = conversations::load(&pool.get().await?, id)
        .await?
        .ok_or_else(Error::not_found)?;
    viewas::admin_reads(pool, &admin, &[(person.clone(), id)]).await?;
    Ok(person)
}

/// `GET /v1/admin/conversations/{id}`.
///
/// # Errors
/// 400 without `Aviary-Admin`, 404; a statement error.
pub async fn get(pool: &Pool, ctx: &Ctx, id: Uuid) -> Result<AdminConversation> {
    audited(pool, ctx, id).await?;
    admin_view(&pool.get().await?, id).await?.ok_or_else(Error::not_found)
}

/// `GET /v1/admin/conversations/{id}/events?before|after|tail&limit` (magpie 7.2's page shape).
///
/// # Errors
/// 400 without `Aviary-Admin` or for a query that is not exactly one window, 404; a statement error.
pub async fn events(pool: &Pool, ctx: &Ctx, id: Uuid, q: &EventsQuery) -> Result<EventsPage> {
    audited(pool, ctx, id).await?;
    let limit = i64::from(q.limit.unwrap_or(40).clamp(1, 500));
    let s = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
    let conn = pool.get().await?;
    let head: i64 = conn
        .query_one("select head_seq from conversations where id = $1", &[&id])
        .await?
        .get(0);
    let cols = "seq, kind, at, turn_id, data, meta, viewer";
    let mut rows = match (q.tail, q.before, q.after) {
        (true, None, None) => {
            conn.query(
                &format!("select {cols} from events where conversation_id = $1 order by seq desc limit $2"),
                &[&id, &limit],
            )
            .await?
        }
        (false, Some(b), None) => {
            conn.query(
                &format!(
                    "select {cols} from events where conversation_id = $1 and seq < $2 order by seq desc limit $3"
                ),
                &[&id, &s(b), &limit],
            )
            .await?
        }
        (false, None, Some(a)) => {
            conn.query(
                &format!("select {cols} from events where conversation_id = $1 and seq > $2 order by seq limit $3"),
                &[&id, &s(a), &limit],
            )
            .await?
        }
        _ => return Err(Error::bad_request("exactly one of tail=1, before, after")),
    };
    if q.after.is_none() {
        rows.reverse();
    }
    let mut events = Vec::with_capacity(rows.len());
    for r in rows {
        let kind: String = r.get(1);
        let data: serde_json::Value = r.get(4);
        let meta: Option<serde_json::Value> = r.get(5);
        events.push(Event {
            seq: u64::try_from(r.get::<_, i64>(0)).unwrap_or(0),
            body: rows::body_of(&kind, &data)?,
            at: Timestamp(r.get(2)),
            turn_id: r.get(3),
            meta: rows::meta_of(meta.as_ref()),
            viewer: r.get(6),
        });
    }
    let first = events.first().map_or(head + 1, |e| s(e.seq));
    let last = events.last().map_or(0, |e| s(e.seq));
    let (has_older, has_newer) = match (events.is_empty(), q.after) {
        (true, Some(a)) => (s(a) >= 1, s(a) < head),
        (true, None) => (q.before.is_some_and(|b| s(b) > 1 && head > 0), false),
        (false, _) => (first > 1, last < head),
    };
    Ok(EventsPage {
        events,
        head_seq: u64::try_from(head).unwrap_or(0),
        has_older,
        has_newer,
    })
}

/// `GET /v1/admin/conversations/{id}/timeline` (M9's shape, without the long-poll).
///
/// # Errors
/// 400 without `Aviary-Admin` or for a bad query, 404; a statement error.
pub async fn timeline(pool: &Pool, ctx: &Ctx, id: Uuid, q: &TimelineQuery) -> Result<TimelinePage> {
    audited(pool, ctx, id).await?;
    let pq = rows::read::PageQuery::check(q)?;
    rows::read::page(pool, id, None, &pq).await
}
