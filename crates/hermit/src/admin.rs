//! The admin routes (hermit 3.12, 16): the operator's, never a service's. Every call needs the admin token and
//! `Aviary-Admin`, and writes an `admin_events` row (the audit log, kept for good): in the same transaction as the
//! change of hermit's own rows where there is one (hosts, switches, release, recycle). A call that acts outside
//! hermit's database (exec in a sandbox, magpie's purge) writes its row as an intent first, in a transaction of its
//! own before anything happens (no row, no action), and records the outcome in that row afterwards; a failure to
//! record the outcome is logged and never turns an action that happened into an error. Reads are audited first.

use std::sync::Arc;

use aviary_pg::Tx;
use hermit_api::admin::{
    AdminEventsQuery, AdminRelease, AdminSandboxesQuery, FreshHost, HostPatch, PurgeReply, PurgeRequest, Recycle,
    SandboxesEnabled,
};
use hermit_api::error::{self, ApiError, codes};
use hermit_api::exec::{ExecRequest, ExecResult};
use hermit_api::people::normalise_email;
use hermit_api::sandbox::ReleaseReason;
use hermit_api::server::Ctx;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::Core;
use crate::sandboxes::{self, OrUnavailable};
use crate::snapshot::{iso, opt_iso};

/// The operator behind the call (`Aviary-Admin`, normalised).
///
/// # Errors
/// 400 `bad_request` without it.
pub fn actor(ctx: &Ctx) -> Result<String, ApiError> {
    ctx.admin().and_then(normalise_email).ok_or_else(|| {
        error::api(
            400,
            codes::BAD_REQUEST,
            "Aviary-Admin (the operator's email) is required",
        )
    })
}

/// Writes one audit row inside `t`.
///
/// # Errors
/// A database error.
pub async fn audit_in(
    t: &Tx<'_>,
    actor: &str,
    action: &str,
    target: Option<&str>,
    data: &Value,
) -> Result<(), aviary_pg::Error> {
    t.execute(
        "insert into admin_events (actor, action, target, data) values ($1, $2, $3, $4)",
        &[&actor, &action, &target, data],
    )
    .await?;
    Ok(())
}

/// Writes an intent row (`data.outcome` = `pending`) before an action outside hermit's database; its id for
/// [`outcome`].
///
/// # Errors
/// 503 `unavailable`: the action does not happen.
async fn intent(core: &Core, actor: &str, action: &str, target: Option<&str>, data: &Value) -> Result<i64, ApiError> {
    let mut data = data.clone();
    data["outcome"] = json!("pending");
    let r = core
        .pool
        .get()
        .await
        .or_503()?
        .query_one(
            "insert into admin_events (actor, action, target, data) values ($1, $2, $3, $4) returning id",
            &[&actor, &action, &target, &data],
        )
        .await
        .or_503()?;
    r.try_get(0).or_503()
}

/// Records what came of an intent row's action (merged into its `data`, `outcome` = `done` or `failed`). Logged
/// when it cannot be written: the action has happened either way.
async fn outcome(core: &Core, id: i64, ok: bool, data: &Value) {
    let mut data = data.clone();
    data["outcome"] = json!(if ok { "done" } else { "failed" });
    let res = async {
        core.pool
            .get()
            .await?
            .execute("update admin_events set data = data || $2 where id = $1", &[&id, &data])
            .await
    }
    .await;
    if let Err(e) = res {
        tracing::error!(admin_event = id, error = %e, "admin action done, its outcome not recorded");
    }
}

/// Writes one audit row on its own (a read).
///
/// # Errors
/// 503 `unavailable`: a read that cannot be audited does not happen.
pub async fn audit(core: &Core, actor: &str, action: &str, target: Option<&str>, data: &Value) -> Result<(), ApiError> {
    core.pool
        .get()
        .await
        .or_503()?
        .execute(
            "insert into admin_events (actor, action, target, data) values ($1, $2, $3, $4)",
            &[&actor, &action, &target, data],
        )
        .await
        .or_503()?;
    Ok(())
}

fn not_found(what: &str) -> ApiError {
    error::not_found(codes::NOT_FOUND, format!("no such {what}"))
}

/// `GET /v1/admin/hosts`: live hosts with slots, sandboxes, `retire_by`, cap.
///
/// # Errors
/// 503 `unavailable`.
pub async fn hosts(core: &Arc<Core>, ctx: &Ctx) -> Result<Value, ApiError> {
    let who = actor(ctx)?;
    audit(core, &who, "hosts.read", None, &json!({})).await?;
    let list = crate::snapshot::live_hosts(core).await.or_503()?;
    Ok(json!({ "hosts": list }))
}

/// `PATCH /v1/admin/hosts/{name}`: a cap, retire, or reactivate.
///
/// # Errors
/// 400 `bad_request` (retire with reactivate), 404 `not_found`, 503 `unavailable`.
pub async fn patch_host(core: &Arc<Core>, ctx: &Ctx, name: &str, body: &HostPatch) -> Result<Value, ApiError> {
    let who = actor(ctx)?;
    let retire = body.retire == Some(true);
    let reactivate = body.reactivate == Some(true);
    if retire && reactivate {
        return Err(error::api(400, codes::BAD_REQUEST, "retire and reactivate together"));
    }
    let data = serde_json::to_value(body).unwrap_or_default();
    let (name_s, who_s) = (name.to_owned(), who.clone());
    let cap = body.max_sandboxes;
    let found = aviary_pg::tx!(&core.pool, |t| -> Result<bool, aviary_pg::Error> {
        let Some(row) = t
            .query_opt(
                "select status from hosts where name = $1 and status <> 'gone' for update",
                &[&name_s],
            )
            .await?
        else {
            return Ok(false);
        };
        let status: String = row.try_get(0)?;
        if let Some(cap) = cap {
            let cap = cap.map(|c| i32::try_from(c).unwrap_or(i32::MAX));
            t.execute("update hosts set max_sandboxes = $2 where name = $1", &[&name_s, &cap])
                .await?;
        }
        if retire && status != "retiring" {
            t.execute(
                "update hosts set status = 'retiring', retire_by = 'operator' where name = $1",
                &[&name_s],
            )
            .await?;
        }
        if reactivate && status == "retiring" {
            t.execute(
                "update hosts set status = 'active', retire_by = null where name = $1",
                &[&name_s],
            )
            .await?;
        }
        audit_in(t, &who_s, "host.patch", Some(&name_s), &data).await?;
        Ok(true)
    })
    .await
    .or_503()?;
    if !found {
        return Err(not_found("host"));
    }
    tracing::info!(host = %name, admin_key = %aviary_core::log::person_key(&who), "host changed by the operator");
    let list = crate::snapshot::live_hosts(core).await.or_503()?;
    Ok(list.into_iter().find(|h| h["name"] == name).unwrap_or(Value::Null))
}

async fn controls(core: &Core) -> Result<Value, aviary_pg::Error> {
    let r = core
        .pool
        .get()
        .await?
        .query_one(
            "select sandboxes_enabled, disabled_reason, fresh_host_until, updated_at, updated_by from controls where id = 1",
            &[],
        )
        .await?;
    Ok(json!({
        "sandboxesEnabled": r.try_get::<_, bool>(0)?,
        "disabledReason": r.try_get::<_, Option<String>>(1)?,
        "freshHostUntil": opt_iso(r.try_get(2)?),
        "updatedAt": iso(r.try_get(3)?),
        "updatedBy": r.try_get::<_, Option<String>>(4)?,
    }))
}

/// `PUT /v1/admin/sandboxes-enabled`: while off, `PUT …/sandbox` answers 503 `sandbox_busy {reason: "disabled"}` and
/// the scaler launches nothing; running sandboxes are untouched.
///
/// # Errors
/// 503 `unavailable`.
pub async fn sandboxes_enabled(core: &Arc<Core>, ctx: &Ctx, body: &SandboxesEnabled) -> Result<Value, ApiError> {
    let who = actor(ctx)?;
    let data = serde_json::to_value(body).unwrap_or_default();
    let (enabled, reason) = (body.enabled, body.reason.clone().map(|r| crate::db::clip(&r, 200)));
    let who_s = who.clone();
    aviary_pg::tx!(&core.pool, |t| -> Result<(), aviary_pg::Error> {
        t.execute(
            "update controls set sandboxes_enabled = $1, disabled_reason = case when $1 then null else $2 end,
               updated_at = now(), updated_by = $3 where id = 1",
            &[&enabled, &reason, &who_s],
        )
        .await?;
        audit_in(t, &who_s, "sandboxes.enabled", None, &data).await
    })
    .await
    .or_503()?;
    tracing::info!(enabled, "sandboxes switched by the operator");
    controls(core).await.or_503()
}

/// `POST /v1/admin/fresh-host`: until the TTL passes only hosts that become active after now are placeable.
///
/// # Errors
/// 400 `bad_request` outside 1..=60, 503 `unavailable`.
pub async fn fresh_host(core: &Arc<Core>, ctx: &Ctx, body: FreshHost) -> Result<Value, ApiError> {
    let who = actor(ctx)?;
    if !(1..=60).contains(&body.ttl_minutes) {
        return Err(error::api(400, codes::BAD_REQUEST, "ttlMinutes is 1..60"));
    }
    let ttl = i64::from(body.ttl_minutes);
    let who_s = who.clone();
    aviary_pg::tx!(&core.pool, |t| -> Result<(), aviary_pg::Error> {
        t.execute(
            "update controls set fresh_host_until = now() + $1::bigint * interval '1 minute', fresh_host_set_at = now(),
               updated_at = now(), updated_by = $2 where id = 1",
            &[&ttl, &who_s],
        )
        .await?;
        audit_in(t, &who_s, "fresh_host", None, &json!({ "ttlMinutes": ttl })).await
    })
    .await
    .or_503()?;
    controls(core).await.or_503()
}

/// `GET /v1/admin/sandboxes?conversation=&person=&since=`: sandboxes with their starts, phases, releases and
/// migrations (the trace tool, platform 15.2). At most 200, newest first.
///
/// # Errors
/// 503 `unavailable`.
pub async fn sandboxes(core: &Arc<Core>, ctx: &Ctx, q: &AdminSandboxesQuery) -> Result<Value, ApiError> {
    let who = actor(ctx)?;
    audit(
        core,
        &who,
        "sandboxes.read",
        None,
        &serde_json::to_value(q).unwrap_or_default(),
    )
    .await?;
    let person = q.person.as_deref().and_then(normalise_email);
    let since = q.since.map(hermit_api::Timestamp::datetime);
    let c = core.pool.get().await.or_503()?;
    let rows = c
        .query(
            "select s.id, s.conversation_id, s.person, s.status, s.phase, s.host, s.image, s.created_at, s.ready_at,
                    s.last_active_at, s.released_at, s.release_reason, s.release_note, s.release_code,
                    s.release_message, s.release_pending, s.busy, s.imported,
                    coalesce((select jsonb_agg(jsonb_build_object('at', to_char(st.at at time zone 'UTC',
                                'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'), 'ok', st.ok, 'ms', st.ms, 'startMs', st.start_ms,
                                'host', st.host, 'reason', st.reason, 'error', st.error) order by st.at)
                              from sandbox_starts st where st.sandbox_id = s.id), '[]'::jsonb),
                    coalesce((select jsonb_agg(jsonb_build_object('at', to_char(e.at at time zone 'UTC',
                                'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'), 'kind', e.kind, 'host', e.host, 'data', e.data)
                                order by e.at)
                              from host_events e where e.sandbox_id = s.id), '[]'::jsonb)
               from sandboxes s
              where ($1::uuid is null or s.conversation_id = $1)
                and ($2::text is null or s.person = $2)
                and ($3::timestamptz is null or s.created_at >= $3)
              order by s.created_at desc limit 200",
            &[&q.conversation, &person, &since],
        )
        .await
        .or_503()?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let get = || -> Result<Value, aviary_pg::reexports::tokio_postgres::Error> {
            Ok(json!({
                "id": r.try_get::<_, Uuid>(0)?,
                "conversationId": r.try_get::<_, Uuid>(1)?,
                "owner": r.try_get::<_, String>(2)?,
                "status": r.try_get::<_, String>(3)?,
                "phase": r.try_get::<_, Option<String>>(4)?,
                "host": r.try_get::<_, Option<String>>(5)?,
                "image": r.try_get::<_, Option<String>>(6)?,
                "createdAt": iso(r.try_get(7)?),
                "readyAt": opt_iso(r.try_get(8)?),
                "lastActiveAt": iso(r.try_get(9)?),
                "releasedAt": opt_iso(r.try_get(10)?),
                "releaseReason": r.try_get::<_, Option<String>>(11)?,
                "releaseNote": r.try_get::<_, Option<String>>(12)?,
                "releaseCode": r.try_get::<_, Option<String>>(13)?,
                "releaseMessage": r.try_get::<_, Option<String>>(14)?,
                "releasePending": r.try_get::<_, Option<Value>>(15)?,
                "busy": r.try_get::<_, bool>(16)?,
                "imported": r.try_get::<_, bool>(17)?,
                "starts": r.try_get::<_, Value>(18)?,
                "events": r.try_get::<_, Value>(19)?,
            }))
        };
        out.push(get().or_503()?);
    }
    Ok(json!({ "sandboxes": out }))
}

/// `GET /v1/admin/sandboxes/{id}/log`: the stored console tail.
///
/// # Errors
/// 404 `not_found`, 503 `unavailable`.
pub async fn sandbox_log(core: &Arc<Core>, ctx: &Ctx, id: Uuid) -> Result<Value, ApiError> {
    let who = actor(ctx)?;
    audit(core, &who, "sandbox.log", Some(&id.to_string()), &json!({})).await?;
    let tail = crate::console::read(core, id)
        .await
        .or_503()?
        .ok_or_else(|| not_found("console tail"))?;
    Ok(serde_json::to_value(tail).unwrap_or_default())
}

/// `POST /v1/admin/sandboxes/{id}/release`: forced release `operator` with the note.
///
/// # Errors
/// 404 `not_found` (no live sandbox), 503 `unavailable`.
pub async fn release(core: &Arc<Core>, ctx: &Ctx, id: Uuid, body: &AdminRelease) -> Result<Value, ApiError> {
    let who = actor(ctx)?;
    let live = crate::db::load(&core.pool, id)
        .await
        .or_503()?
        .is_some_and(|r| r.is_live());
    if !live {
        return Err(not_found("live sandbox"));
    }
    let (c, note) = (core.clone(), body.note.clone());
    // the audit row and the pending release commit together
    let took = aviary_pg::tx!(&core.pool, |t| -> Result<bool, aviary_pg::Error> {
        audit_in(
            t,
            &who,
            "sandbox.release",
            Some(&id.to_string()),
            &json!({ "note": note }),
        )
        .await?;
        sandboxes::request_in(t, &c, id, ReleaseReason::Operator, true, note.clone()).await
    })
    .await
    .or_503()?;
    if took {
        sandboxes::run_requested(core, id);
    }
    Ok(json!({ "released": true }))
}

/// `POST /v1/admin/sandboxes/{id}/exec`: forwards to that sandbox's supervisor; the audit records the argv and the
/// exit code, never stdin or output.
///
/// # Errors
/// 400 `bad_request` (an empty argv), 404 `not_found`, 503 `sandbox_unreachable`, the supervisor's refusal.
pub async fn exec(core: &Arc<Core>, ctx: &Ctx, id: Uuid, body: &ExecRequest) -> Result<ExecResult, ApiError> {
    let who = actor(ctx)?;
    if body.argv.is_empty() {
        return Err(error::api(400, codes::BAD_REQUEST, "argv is empty"));
    }
    let row = crate::db::load(&core.pool, id)
        .await
        .or_503()?
        .filter(crate::db::SandboxRow::is_live)
        .ok_or_else(|| not_found("live sandbox"))?;
    let client = crate::supervisor_client::client(core, &row)
        .ok_or_else(|| error::api(503, codes::SANDBOX_UNREACHABLE, "the sandbox has no address yet"))?;
    // no audit row, no exec: the intent is written before the call
    let ev = intent(
        core,
        &who,
        "sandbox.exec",
        Some(&id.to_string()),
        &json!({ "argv": body.argv, "cwd": body.cwd }),
    )
    .await?;
    let res = client.exec(body).await;
    let (ok, done) = match &res {
        Ok(r) => (true, json!({ "exitCode": r.exit_code, "signal": r.signal })),
        Err(e) => (false, json!({ "error": crate::db::clip(&e.to_string(), 300) })),
    };
    outcome(core, ev, ok, &done).await;
    res.map_err(|e| match e {
        hermit_api::ClientError::Api(r) => {
            let mut a = error::api(r.status, &r.body.code, r.body.message.clone());
            if let Some(s) = r.retry_after {
                a = a.retry_after(s);
            }
            a
        }
        other => error::api(503, codes::SANDBOX_UNREACHABLE, other.to_string()),
    })
}

/// `POST /v1/admin/recycle`: releases every live sandbox on another image (soft with `onlyIdle`, forced without).
///
/// # Errors
/// 400 `bad_request` (not a digest), 503 `unavailable`.
pub async fn recycle(core: &Arc<Core>, ctx: &Ctx, body: &Recycle) -> Result<Value, ApiError> {
    let who = actor(ctx)?;
    let digest = body.except_image.strip_prefix("sha256:").unwrap_or("");
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(error::api(400, codes::BAD_REQUEST, "exceptImage is sha256:<64 hex>"));
    }
    let (c, image, force) = (core.clone(), body.except_image.clone(), !body.only_idle);
    // the audit row and every pending release commit together
    let (ids, took) = aviary_pg::tx!(&core.pool, |t| -> Result<(Vec<Uuid>, Vec<Uuid>), aviary_pg::Error> {
        let ids: Vec<Uuid> = t
            .query(
                "select id from sandboxes where status in ('starting', 'ready') and image is distinct from $1
                  order by id",
                &[&image],
            )
            .await?
            .iter()
            .filter_map(|r| r.try_get(0).ok())
            .collect();
        audit_in(
            t,
            &who,
            "recycle",
            None,
            &json!({ "exceptImage": image, "onlyIdle": !force, "sandboxes": ids.len() }),
        )
        .await?;
        let mut took = Vec::new();
        for id in &ids {
            if sandboxes::request_in(t, &c, *id, ReleaseReason::Recycle, force, None).await? {
                took.push(*id);
            }
        }
        Ok((ids, took))
    })
    .await
    .or_503()?;
    for id in took {
        sandboxes::run_requested(core, id);
    }
    Ok(json!({ "released": ids.len(), "force": force }))
}

/// `POST /v1/admin/people/{email}/purge`: the only deletion of a person's data. 409 `still_allowed` while the
/// catalogue allows the person; otherwise their live sandboxes are released (forced, `offboarded`) and magpie
/// purges their conversations (magpie 9.6). Idempotent.
///
/// # Errors
/// 400 `bad_email` / `bad_request` (confirm differs), 409 `still_allowed`, 503 (magpie or the database).
pub async fn purge(core: &Arc<Core>, ctx: &Ctx, email: &str, body: &PurgeRequest) -> Result<PurgeReply, ApiError> {
    let who = actor(ctx)?;
    let person = normalise_email(email).ok_or_else(|| error::api(400, codes::BAD_EMAIL, "not an email address"))?;
    if normalise_email(&body.confirm).as_deref() != Some(person.as_str()) {
        return Err(error::api(400, codes::BAD_REQUEST, "confirm must repeat the email"));
    }
    if core.catalogue.allowed(&person) {
        return Err(error::api(
            409,
            codes::STILL_ALLOWED,
            "the catalogue still allows this person",
        ));
    }
    let person_key = aviary_core::log::person_key(&person);
    // the intent row and the person's forced releases commit together, before magpie is asked
    let (c, who_s, person_s) = (core.clone(), who.clone(), person.clone());
    let (ev, took) = aviary_pg::tx!(&core.pool, |t| -> Result<(i64, Vec<Uuid>), aviary_pg::Error> {
        let live: Vec<Uuid> = t
            .query(
                "select id from sandboxes where person = $1 and status <> 'gone' order by id",
                &[&person_s],
            )
            .await?
            .iter()
            .filter_map(|r| r.try_get(0).ok())
            .collect();
        let data = json!({ "personKey": aviary_core::log::person_key(&person_s), "sandboxes": live.len(),
                           "outcome": "pending" });
        let ev: i64 = t
            .query_one(
                "insert into admin_events (actor, action, target, data) values ($1, 'person.purge', $2, $3)
                 returning id",
                &[&who_s, &person_s, &data],
            )
            .await?
            .try_get(0)?;
        let mut took = Vec::new();
        for id in &live {
            if sandboxes::request_in(t, &c, *id, ReleaseReason::Offboarded, true, Some("purge".into())).await? {
                took.push(*id);
            }
        }
        Ok((ev, took))
    })
    .await
    .or_503()?;
    for id in took {
        sandboxes::run_requested(core, id);
    }
    let res = core
        .ops
        .up
        .magpie
        .service(Some(&who))
        .person_purge(
            &person,
            &magpie_api::ops::PersonPurgeRequest {
                reason: magpie_api::ops::PurgeReason::Offboarded,
            },
        )
        .await;
    let (ok, done) = match &res {
        Ok(r) => (true, json!({ "conversations": r.conversations })),
        Err(e) => (false, json!({ "error": crate::db::clip(&e.to_string(), 300) })),
    };
    outcome(core, ev, ok, &done).await;
    match res {
        Ok(r) => {
            tracing::info!(person_key = %person_key, conversations = r.conversations, "person purged by the operator");
            Ok(PurgeReply {
                conversations: r.conversations,
            })
        }
        Err(e) => {
            tracing::warn!(person_key = %person_key, error = %e, "person purge: magpie refused or did not answer");
            Err(error::api(503, codes::UNAVAILABLE, format!("magpie: {e}")).retry_after(5))
        }
    }
}

/// `GET /v1/admin/events?limit=` (default 100, at most 1 000), newest first.
///
/// # Errors
/// 503 `unavailable`.
pub async fn events(core: &Arc<Core>, ctx: &Ctx, q: AdminEventsQuery) -> Result<Value, ApiError> {
    let who = actor(ctx)?;
    audit(core, &who, "events.read", None, &json!({ "limit": q.limit })).await?;
    let limit = i64::from(q.limit.unwrap_or(100).clamp(1, 1000));
    let rows = core
        .pool
        .get()
        .await
        .or_503()?
        .query(
            "select id, at, actor, action, target, data from admin_events order by id desc limit $1",
            &[&limit],
        )
        .await
        .or_503()?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(json!({
            "id": r.try_get::<_, i64>(0).or_503()?,
            "at": iso(r.try_get(1).or_503()?),
            "actor": r.try_get::<_, String>(2).or_503()?,
            "action": r.try_get::<_, String>(3).or_503()?,
            "target": r.try_get::<_, Option<String>>(4).or_503()?,
            "data": r.try_get::<_, Value>(5).or_503()?,
        }));
    }
    Ok(json!({ "events": out }))
}
