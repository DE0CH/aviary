//! The ops reads (magpie 16.4, platform 15.3): per-person figures, the view-as log, and store totals for hermit's
//! ops page.

use aviary_pg::Pool;
use magpie_api::Timestamp;
use magpie_api::ops::{OpsStats, PeopleResponse, PersonStats, ViewAsEntry, ViewAsEvent, ViewAsLog};
use time::OffsetDateTime;

use crate::db::count;
use crate::error::Result;
use crate::limits;

/// The page size of the view-as log: default and most.
pub const VIEW_AS_DEFAULT: u32 = 100;
/// See [`VIEW_AS_DEFAULT`].
pub const VIEW_AS_MAX: u32 = 1_000;

/// `GET /v1/ops/people?tz=`: every known person. `total` counts every conversation not purged (Recently deleted
/// included), `today` and `last7d` those active since the zone's midnight and in the last 7 days, `live` those
/// with a run state other than idle, `deleted` those in Recently deleted, `purged` the person's purge counter. An
/// unknown zone falls back to UTC.
///
/// # Errors
/// When a statement fails.
pub async fn people(pool: &Pool, tz: Option<&str>) -> Result<PeopleResponse> {
    let tz = tz
        .map(str::trim)
        .filter(|z| z.parse::<chrono_tz::Tz>().is_ok())
        .unwrap_or("UTC")
        .to_owned();
    let rows = pool
        .get()
        .await?
        .query(
            "with midnight as (select (date_trunc('day', now() at time zone $1) at time zone $1) as m)
             select p.person, p.purged_conversations,
                    count(c.id) as total,
                    count(c.id) filter (where c.activity_at >= (select m from midnight)) as today,
                    count(c.id) filter (where c.activity_at >= now() - interval '7 days') as last7d,
                    count(c.id) filter (where c.run_state <> 'idle') as live,
                    count(c.id) filter (where c.deleted_at is not null) as deleted,
                    max(c.activity_at) as last_active
             from persons p left join conversations c on c.person = p.person
             group by p.person, p.purged_conversations
             order by p.person",
            &[&tz],
        )
        .await?;
    let people = rows
        .iter()
        .map(|r| PersonStats {
            owner: r.get(0),
            purged: u64::try_from(r.get::<_, i64>(1)).unwrap_or(0),
            total: count(r, "total"),
            today: count(r, "today"),
            last7d: count(r, "last7d"),
            live: count(r, "live"),
            deleted: count(r, "deleted"),
            last_active_at: r.get::<_, Option<OffsetDateTime>>("last_active").map(Timestamp),
        })
        .collect();
    Ok(PeopleResponse { tz, people })
}

fn view_as_event(s: &str) -> ViewAsEvent {
    match s {
        "create" => ViewAsEvent::Create,
        "send" => ViewAsEvent::Send,
        "cancel" => ViewAsEvent::Cancel,
        "upload" => ViewAsEvent::Upload,
        "admin_read" => ViewAsEvent::AdminRead,
        _ => ViewAsEvent::Start,
    }
}

/// `GET /v1/ops/view-as?limit=&before=<id>`: the view-as audit, newest first by `(at, id)`; `before` is the last
/// entry's id of the previous page, and the page holds the entries older than that entry (entries that share its
/// time included, ordered by id). An id that names no entry gives an empty page.
///
/// # Errors
/// When a statement fails.
pub async fn view_as(pool: &Pool, limit: Option<u32>, before: Option<u64>) -> Result<ViewAsLog> {
    let limit = i64::from(limit.unwrap_or(VIEW_AS_DEFAULT).clamp(1, VIEW_AS_MAX));
    let before = before.map(|b| i64::try_from(b).unwrap_or(i64::MAX));
    let rows = pool
        .get()
        .await?
        .query(
            "select id, at, conversation_id, event, admin, person, session, chars from view_as_log
             where $1::bigint is null or (at, id) < (select at, id from view_as_log where id = $1)
             order by at desc, id desc limit $2",
            &[&before, &limit],
        )
        .await?;
    let view_as = rows
        .iter()
        .map(|r| ViewAsEntry {
            id: u64::try_from(r.get::<_, i64>(0)).unwrap_or(0),
            at: Timestamp(r.get(1)),
            conversation_id: r.get(2),
            event: view_as_event(r.get(3)),
            admin: r.get(4),
            user: r.get(5),
            session: Some(r.get::<_, String>(6)).filter(|s| !s.is_empty()),
            chars: r.get::<_, Option<i32>>(7).and_then(|c| u64::try_from(c).ok()),
        })
        .collect();
    Ok(ViewAsLog { view_as })
}

/// `GET /v1/ops/stats`. `events` and `rows`, the largest tables, are Postgres's estimates (`pg_class.reltuples`,
/// kept by autovacuum and analyze), so the call stays cheap however much is stored; the other figures are exact.
///
/// # Errors
/// When a statement fails.
pub async fn stats(pool: &Pool) -> Result<OpsStats> {
    let r = pool
        .get()
        .await?
        .query_one(
            "select
               (select count(*) from conversations where deleted_at is null) as conversations,
               (select count(*) from conversations where deleted_at is not null) as deleted,
               (select greatest(reltuples, 0)::bigint from pg_class where oid = 'events'::regclass) as events,
               (select greatest(reltuples, 0)::bigint from pg_class where oid = 'rows'::regclass) as rows,
               (select coalesce(sum(cc_bytes), 0)::bigint from conversations) as cc_bytes,
               (select count(*) from blobs where stored) as blobs,
               (select coalesce(sum(size), 0)::bigint from blobs where stored) as blob_bytes,
               (select count(*) from blob_staging) as staged_blobs,
               (select coalesce(sum(length(bytes)), 0)::bigint from blob_staging) as staged_bytes,
               (select count(*) from outbox) as outbox_pending,
               (select min(created_at) from outbox) as oldest_outbox,
               (select min(deleted_at) + make_interval(secs => $1) from conversations
                  where deleted_at <= now() - make_interval(secs => $1)) as oldest_due_purge,
               (select extract(epoch from now() - updated_at)::float8 from hermit_feed) as feed_lag",
            &[&limits::RECENTLY_DELETED.as_secs_f64()],
        )
        .await?;
    Ok(OpsStats {
        conversations: count(&r, "conversations"),
        deleted: count(&r, "deleted"),
        events: count(&r, "events"),
        rows: count(&r, "rows"),
        cc_bytes: count(&r, "cc_bytes"),
        blobs: count(&r, "blobs"),
        blob_bytes: count(&r, "blob_bytes"),
        staged_blobs: count(&r, "staged_blobs"),
        staged_bytes: count(&r, "staged_bytes"),
        outbox_pending: count(&r, "outbox_pending"),
        oldest_outbox_at: r.get::<_, Option<OffsetDateTime>>("oldest_outbox").map(Timestamp),
        oldest_due_purge_at: r.get::<_, Option<OffsetDateTime>>("oldest_due_purge").map(Timestamp),
        hermit_feed_lag_seconds: r.get("feed_lag"),
    })
}
