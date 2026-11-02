//! The operator snapshot `GET /ops/snapshot?tz=` (hermit 18.3): one read-only JSON of the deployment, built from
//! hermit's tables, the fabric, magpie's ops reads, egress's ops summary and the Kubernetes API. Sections are built
//! in parallel, each within 8 s; a failed one is `null` with its message in `errors[<section>]`. Cached 4 s per zone
//! per replica. No message contents anywhere: owners (emails), ids, hosts, counts, costs and timings.
//!
//! Two consumers read fields of it by name, so those keep their names, types and units (18.3's two contracts,
//! guarded by `tests/ops.rs`): the news-stream monitor's `ai_slim` (state) and `ai_activity` (event diffs on `at`).

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use hermit_api::Timestamp;
use hermit_api::error::ApiError;
use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::Core;

/// Each section's time limit (18.3).
pub const SECTION_TIMEOUT: Duration = Duration::from_secs(8);
/// "Current" errors: this many minutes back.
pub const RECENT_MINUTES: u32 = 15;
/// A host is reachable while its last heartbeat is younger than this.
pub const HOST_FRESH_S: i64 = 30;
/// The warm hints counted (hermit 3.5, 13.4).
pub const DEMAND_WINDOW_S: i64 = 120;
const GONE_HOSTS_SHOWN: i64 = 30;
const STARTS_SHOWN: i64 = 80;
const EVENTS_SHOWN: i64 = 60;
const RECENT_SHOWN: usize = 20;
const VIEW_AS_SHOWN: u32 = 100;

type Row = aviary_pg::Row;

/// RFC 3339 UTC with milliseconds (the monitor's `at` format).
#[must_use]
pub fn iso(t: OffsetDateTime) -> Value {
    Value::String(Timestamp::from(t).to_string())
}

#[must_use]
pub fn opt_iso(t: Option<OffsetDateTime>) -> Value {
    t.map_or(Value::Null, iso)
}

fn secs_between(a: OffsetDateTime, b: OffsetDateTime) -> i64 {
    (b - a).whole_seconds()
}

async fn section<T, F>(errors: &std::sync::Mutex<Map<String, Value>>, name: &str, f: F) -> Option<T>
where
    F: Future<Output = Result<T, String>>,
{
    let res = match tokio::time::timeout(SECTION_TIMEOUT, f).await {
        Ok(r) => r,
        Err(_) => Err(format!("{name}: no answer in 8000 ms")),
    };
    match res {
        Ok(v) => Some(v),
        Err(e) => {
            let e: String = e.chars().take(300).collect();
            errors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(name.to_owned(), Value::String(e));
            None
        }
    }
}

fn list_of(v: &Value) -> &[Value] {
    v.as_array().map_or(&[], Vec::as_slice)
}

fn s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// The snapshot for zone `tz` (else `ops.timeZone`), from the cache when younger than 4 s.
///
/// # Errors
/// Never in practice: a failed source is a `null` section.
pub async fn get(core: &Arc<Core>, tz: Option<&str>) -> Result<Arc<Value>, ApiError> {
    let zone = crate::ops::safe_zone(tz, &core.cfg.file.ops.time_zone);
    if let Some(v) = core.ops.cached(&zone) {
        return Ok(v);
    }
    let v = Arc::new(Box::pin(build(core, &zone)).await);
    core.ops.store(&zone, v.clone());
    Ok(v)
}

/// Day and month starts in `tz` (an IANA zone [`crate::ops::safe_zone`] accepted), computed here with the zone
/// database compiled in: no source to fail, so the page and the monitor always agree on "today".
#[must_use]
pub fn day_and_month(tz: &str, now: OffsetDateTime) -> (String, OffsetDateTime, OffsetDateTime) {
    use chrono::{Datelike as _, TimeZone as _};
    let Ok(zone) = tz.parse::<chrono_tz::Tz>() else {
        let day = now.to_offset(time::UtcOffset::UTC).replace_time(time::Time::MIDNIGHT);
        return ("UTC".to_owned(), day, day.replace_day(1).unwrap_or(day));
    };
    let Some(utc) = chrono::DateTime::from_timestamp(now.unix_timestamp(), 0) else {
        let day = now.replace_time(time::Time::MIDNIGHT);
        return (tz.to_owned(), day, day);
    };
    let local = utc.with_timezone(&zone).date_naive();
    // the first instant of a local date: midnight, or the end of a gap when a clock change skips midnight
    let start = |d: chrono::NaiveDate| -> OffsetDateTime {
        let mut t = d.and_time(chrono::NaiveTime::MIN);
        for _ in 0..(26 * 4) {
            if let Some(x) = zone.from_local_datetime(&t).earliest() {
                return OffsetDateTime::from_unix_timestamp(x.timestamp()).unwrap_or(now);
            }
            t += chrono::Duration::minutes(15);
        }
        now
    };
    let month = local.with_day(1).unwrap_or(local);
    (tz.to_owned(), start(local), start(month))
}

/// Builds the snapshot (no cache).
#[allow(clippy::too_many_lines)] // the sections in the page's order
pub async fn build(core: &Arc<Core>, tz: &str) -> Value {
    let errors = std::sync::Mutex::new(Map::new());
    let (tz, day, month) = day_and_month(tz, OffsetDateTime::now_utc());
    let cfg = &core.cfg.file;
    let up = &core.ops.up;
    let (
        service,
        hosts,
        sandboxes,
        scaler,
        local_recent,
        hosts_gone,
        events,
        starts,
        summary,
        magpie_people,
        view_as,
        controls,
        replicas,
        fabric_ops,
    ) = tokio::join!(
        section(&errors, "service", service(core)),
        section(&errors, "hosts", hosts_section(core)),
        section(&errors, "sandboxes", sandboxes_section(core)),
        section(&errors, "scaler", scaler_section(core)),
        section(&errors, "recent", recent_local(core)),
        section(&errors, "hostsGone", hosts_gone(core)),
        section(&errors, "events", events_section(core)),
        section(&errors, "starts", starts_section(core)),
        section(&errors, "egress", async {
            up.egress_summary(&tz, RECENT_MINUTES).await
        }),
        section(&errors, "people", async {
            up.magpie
                .service(None)
                .ops_people(&magpie_api::ops::PeopleQuery { tz: Some(tz.clone()) })
                .await
                .map_err(s)
        }),
        section(&errors, "viewAs", async {
            up.magpie
                .service(None)
                .ops_view_as(&magpie_api::ops::ViewAsLogQuery {
                    limit: Some(VIEW_AS_SHOWN),
                    before: None,
                })
                .await
                .map_err(s)
        }),
        section(&errors, "controls", controls_section(core)),
        section(&errors, "replicas", replicas_section(core)),
        section(&errors, "fabricOps", core.providers.fabric.ops()),
    );
    let mut errors = errors.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);

    // budget and egress are egress's, passed through as egress sent them (18.3); a failed summary leaves both null (and its error under both)
    let (budget, egress) = if let Some(sm) = &summary {
        (sm.budget.clone(), sm.egress.clone())
    } else {
        if let Some(e) = errors.get("egress").cloned() {
            errors.insert("budget".into(), e);
        }
        (Value::Null, Value::Null)
    };
    // recent: hermit's own counts and rows, with egress's model errors and blocked requests
    let recent = local_recent.map(|mut r| {
        let (model_errors, blocked, list) = summary.as_ref().map_or((Value::Null, Value::Null, vec![]), |sm| {
            (
                json!(sm.recent.model_errors),
                json!(sm.recent.blocked),
                sm.recent
                    .list
                    .iter()
                    .map(|e| json!({"at": e.at, "what": e.what, "detail": e.detail, "owner": e.owner}))
                    .collect(),
            )
        });
        r["modelErrors"] = model_errors;
        r["blocked"] = blocked;
        let mut all: Vec<Value> = r["list"].as_array().cloned().unwrap_or_default();
        all.extend(list);
        all.sort_by(|a, b| b["at"].as_str().cmp(&a["at"].as_str()));
        all.truncate(RECENT_SHOWN);
        r["list"] = Value::Array(all);
        r
    });
    if summary.is_none()
        && recent.is_some()
        && let Some(e) = errors.get("egress").cloned()
    {
        errors.insert("recent".into(), e);
    }

    // people: every explicit catalogue user (zeros when magpie has nothing), then everyone magpie reports; `live` is
    // hermit's count of the person's live sandboxes
    let live_by: HashMap<String, u64> = sandboxes
        .as_ref()
        .map(|s: &Value| {
            let mut m = HashMap::new();
            for x in s["list"].as_array().map_or(&[][..], Vec::as_slice) {
                if let Some(o) = x["owner"].as_str() {
                    *m.entry(o.to_owned()).or_insert(0) += 1;
                }
            }
            m
        })
        .unwrap_or_default();
    let people = magpie_people.map(|mp| people_section(core, &mp, &live_by));
    let view_as = view_as.map(|v| {
        Value::Array(
            v.view_as
                .iter()
                .map(|e| {
                    json!({"at": e.at, "conversationId": e.conversation_id, "event": e.event, "admin": e.admin,
                           "user": e.user, "session": e.session, "chars": e.chars})
                })
                .collect(),
        )
    });
    let gateway_missing = hosts.as_ref().is_some_and(|h: &Value| {
        !h["list"]
            .as_array()
            .is_some_and(|l| l.iter().any(|x| x["gateway"] == true))
    });
    let hosts = hosts.map(|mut h| {
        for x in h["list"].as_array_mut().into_iter().flatten() {
            if let Some(name) = x["name"].as_str()
                && x["gateway"] != true
            {
                x["slots"] = fabric_ops
                    .as_ref()
                    .and_then(|f| f.slots.get(name))
                    .map_or(Value::Null, |n| json!(n));
            }
        }
        h
    });
    let scaler = scaler.map(|mut sc| {
        sc["plan"] = fabric_ops
            .as_ref()
            .and_then(|f| f.plan.as_ref())
            .map_or(Value::Null, |p| serde_json::to_value(p).unwrap_or(Value::Null));
        sc
    });
    // a silent replica whose pod still runs is a hung one; the silent rows of pods a rollout replaced are history.
    // Without the pod list, a replica silent for under 2 minutes is still shown.
    let running: Option<Vec<String>> = service.as_ref().filter(|s| s["available"] == true).map(|s| {
        list_of(&s["deployments"])
            .iter()
            .flat_map(|d| list_of(&d["pods"]).iter())
            .filter(|p| p["phase"] != "Terminating")
            .filter_map(|p| p["name"].as_str().map(str::to_owned))
            .collect()
    });
    let now = OffsetDateTime::now_utc();
    let replicas = replicas.map(|r| {
        Value::Array(
            list_of(&r)
                .iter()
                .filter(|x| {
                    x["reporting"] == true
                        || running.as_ref().map_or_else(
                            || {
                                x["seenAt"]
                                    .as_str()
                                    .and_then(|t| Timestamp::parse(t).ok())
                                    .is_some_and(|t| secs_between(t.datetime(), now) < 120)
                            },
                            |pods| x["pod"].as_str().is_some_and(|p| pods.iter().any(|q| q == p)),
                        )
                })
                .cloned()
                .collect(),
        )
    });
    let catalogue = core.catalogue.get();
    let mut d = json!({
        "at": Timestamp::now(),
        "tz": tz,
        "today": iso(day),
        "monthStart": iso(month),
        "product": cfg.product.name,
        "fabric": "gvisor-aws",
        "sandboxImage": cfg.sandbox.image,
        "recentMinutes": RECENT_MINUTES,
        "controls": controls,
        "catalogue": {
            "generation": catalogue.as_ref().map(|c| c.generation.clone()),
            "loadedAt": catalogue.as_ref().map(|c| c.loaded_at),
            "refused": core.catalogue.refused(),
        },
        "fabricOps": fabric_ops.as_ref().map(|f| json!({
            "lastPassAt": f.last_pass_at,
            "egressTargets": f.egress_targets,
        })),
        "replicas": replicas,
        // now
        "service": service,
        "hosts": hosts,
        "gatewayMissing": gateway_missing,
        "sandboxes": sandboxes,
        "scaler": scaler,
        "recent": recent,
        "budget": budget,
        // history
        "hostsGone": hosts_gone,
        "events": events,
        "starts": starts,
        "egress": egress,
        "people": people,
        "viewAs": view_as,
        "errors": errors,
    });
    let health = crate::health_verdict::health_of(&d, cfg.ops.budget_watch, RECENT_MINUTES);
    d["health"] = serde_json::to_value(health).unwrap_or(Value::Null);
    d
}

/// The service's Deployments (in `ops.deployments` order) and their pods, from the Kubernetes API (A3.F40).
async fn service(core: &Core) -> Result<Value, String> {
    let names = &core.cfg.file.ops.deployments;
    let Some((deps, pods)) = core.ops.up.cluster.service(names).await? else {
        return Ok(json!({"available": false, "deployments": []}));
    };
    let prefix = &core.cfg.file.ops.pod_node_prefix;
    Ok(service_of(names, &deps, &pods, prefix, OffsetDateTime::now_utc()))
}

/// The `service` section from raw API objects (pure; tested on API-shaped fixtures).
#[must_use]
pub fn service_of(names: &[String], deps: &[Value], pods: &[Value], node_prefix: &str, now: OffsetDateTime) -> Value {
    let int = |v: &Value| v.as_i64().unwrap_or(0);
    let pod = |p: &Value| {
        let st = &p["status"];
        let ready = st["conditions"]
            .as_array()
            .is_some_and(|c| c.iter().any(|x| x["type"] == "Ready" && x["status"] == "True"));
        let cs = st["containerStatuses"].as_array().map_or(&[][..], Vec::as_slice);
        let waiting = cs.iter().find_map(|c| c["state"]["waiting"]["reason"].as_str());
        let phase = if p["metadata"]["deletionTimestamp"].is_string() {
            "Terminating".to_owned()
        } else {
            waiting.or_else(|| st["phase"].as_str()).unwrap_or("?").to_owned()
        };
        let age = p["metadata"]["creationTimestamp"]
            .as_str()
            .and_then(|t| Timestamp::parse(t).ok())
            .map(|t| secs_between(t.datetime(), now));
        let node = p["spec"]["nodeName"]
            .as_str()
            .map(|n| n.strip_prefix(node_prefix).unwrap_or(n).to_owned());
        json!({
            "name": p["metadata"]["name"],
            "ready": ready,
            "phase": phase,
            "restarts": cs.iter().map(|c| int(&c["restartCount"])).sum::<i64>(),
            "ageS": age,
            "node": node,
        })
    };
    let list: Vec<Value> = names
        .iter()
        .map(|name| {
            let mut mine: Vec<Value> = pods
                .iter()
                .filter(|p| p["metadata"]["labels"]["app"].as_str() == Some(name))
                .map(pod)
                .collect();
            mine.sort_by_key(|p| p["ageS"].as_i64().unwrap_or(0));
            let Some(d) = deps.iter().find(|d| d["metadata"]["name"].as_str() == Some(name)) else {
                return json!({"name": name, "missing": true, "state": "problem", "desired": 0, "ready": 0,
                              "updated": 0, "total": 0, "rolling": false, "pods": mine});
            };
            let st = &d["status"];
            let desired = int(&d["spec"]["replicas"]);
            let ready = int(&st["readyReplicas"]);
            let updated = int(&st["updatedReplicas"]);
            let total = int(&st["replicas"]);
            let rolling = updated < desired
                || total > desired
                || int(&st["observedGeneration"]) < int(&d["metadata"]["generation"]);
            // a Deployment deliberately scaled to 0 (and settled there) is not an outage
            let state = if desired == 0 && ready == 0 && !rolling {
                "ok"
            } else if ready >= desired && desired > 0 {
                if rolling { "warming" } else { "ok" }
            } else if rolling && ready > 0 {
                "warming"
            } else {
                "problem"
            };
            json!({"name": name, "desired": desired, "ready": ready, "updated": updated, "total": total,
                   "rolling": rolling, "state": state, "pods": mine})
        })
        .collect();
    json!({"available": true, "deployments": list})
}

/// Live hosts (the gateway and AWS hosts; not gone) with their sandbox counts.
///
/// # Errors
/// A database error.
pub async fn live_hosts(core: &Core) -> Result<Vec<Value>, aviary_pg::Error> {
    let rows = core
        .pool
        .get()
        .await?
        .query(
            "select h.name, h.kind, h.status, h.instance_id, h.created_at, h.last_seen_at, h.empty_since, h.retire_by,
                    h.max_sandboxes, h.cpus, h.mem_total, h.info,
                    h.last_seen_at > now() - $1::bigint * interval '1 second' as reachable,
                    (select count(*) from sandboxes s where s.host = h.name and s.status <> 'gone') as n
               from hosts h where h.status <> 'gone' order by h.kind = 'gateway' desc, h.created_at",
            &[&HOST_FRESH_S],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let info: Option<Value> = r.try_get(11)?;
        let info = info.unwrap_or(Value::Null);
        let kind: String = r.try_get(1)?;
        let status: String = r.try_get(2)?;
        out.push(json!({
            "name": r.try_get::<_, String>(0)?,
            "kind": kind,
            "gateway": kind == "gateway",
            "status": status,
            "instanceId": r.try_get::<_, Option<String>>(3)?,
            "launchedAt": iso(r.try_get(4)?),
            "lastSeenAt": opt_iso(r.try_get(5)?),
            "reachable": r.try_get::<_, Option<bool>>(12)?.unwrap_or(false),
            "emptySince": opt_iso(r.try_get(6)?),
            "retiring": status == "retiring",
            "retireBy": r.try_get::<_, Option<String>>(7)?,
            "sandboxes": r.try_get::<_, i64>(13)?,
            "slots": Value::Null,
            "maxSandboxes": r.try_get::<_, Option<i32>>(8)?,
            "cpus": r.try_get::<_, Option<i32>>(9)?,
            "memTotal": r.try_get::<_, Option<i64>>(10)?,
            "memAvailable": info["memAvailable"].as_i64(),
            "load1": info["loadAvg"][0].as_f64(),
            "gvisor": info["gvisorRelease"].clone(),
        }));
    }
    Ok(out)
}

async fn hosts_section(core: &Core) -> Result<Value, String> {
    Ok(json!({
        "maxHosts": core.cfg.file.fabric.aws.max_hosts,
        "list": live_hosts(core).await.map_err(s)?,
    }))
}

async fn rows(
    core: &Core,
    sql: &str,
    params: &[&(dyn aviary_pg::reexports::tokio_postgres::types::ToSql + Sync)],
) -> Result<Vec<Row>, String> {
    core.pool.get().await.map_err(s)?.query(sql, params).await.map_err(s)
}

macro_rules! col {
    ($r:expr, $i:expr, $t:ty) => {
        $r.try_get::<_, $t>($i).map_err(s)?
    };
}

/// Live sandboxes (not gone) with their people, phases, busy flags, migrations and pending releases (A3.F41).
async fn sandboxes_section(core: &Core) -> Result<Value, String> {
    let sb = &core.cfg.file.sandbox;
    let reporting_s = i64::from(sb.activity_every_seconds) * 3;
    let list = rows(
        core,
        "select s.id, s.conversation_id, s.person, s.host, s.status, s.phase, s.busy, s.created_at, s.last_active_at,
                s.expires_at, s.release_pending,
                s.last_heartbeat_at > now() - $1::bigint * interval '1 second',
                (select count(*) from host_events e where e.sandbox_id = s.id and e.kind = 'migration'),
                now()
           from sandboxes s where s.status <> 'gone' order by s.created_at",
        &[&reporting_s],
    )
    .await?;
    let mut out = Vec::with_capacity(list.len());
    for r in &list {
        let created: OffsetDateTime = col!(r, 7, OffsetDateTime);
        let active: OffsetDateTime = col!(r, 8, OffsetDateTime);
        let now: OffsetDateTime = col!(r, 13, OffsetDateTime);
        let pending: Option<Value> = col!(r, 10, Option<Value>);
        out.push(json!({
            "id": col!(r, 0, Uuid),
            "conversationId": col!(r, 1, Uuid),
            "owner": col!(r, 2, String),
            "host": col!(r, 3, Option<String>),
            "status": col!(r, 4, String),
            "phase": col!(r, 5, Option<String>),
            "busy": col!(r, 6, bool),
            "createdAt": iso(created),
            "lastActiveAt": iso(active),
            "expiresAt": iso(col!(r, 9, OffsetDateTime)),
            "ageS": secs_between(created, now),
            "idleS": secs_between(active, now).max(0),
            "migrations": col!(r, 12, i64),
            "releasePending": pending.map(|p| json!({
                "reason": p["reason"], "force": p["force"], "refusals": p["refusals"], "askedAt": p["askedAt"],
            })),
            "reporting": col!(r, 11, Option<bool>).unwrap_or(false),
        }));
    }
    Ok(json!({
        "maxActive": sb.max_active,
        "idleMinutes": sb.idle_minutes,
        "maxLifetimeHours": sb.max_lifetime_hours,
        "list": out,
    }))
}

/// The warm hints counted now (people typing in the last 120 s); the plan is the fabric's.
async fn scaler_section(core: &Core) -> Result<Value, String> {
    let list = rows(
        core,
        "select person, at, at > now() - $1::bigint * interval '1 second' from demand_hints order by at desc limit 30",
        &[&DEMAND_WINDOW_S],
    )
    .await?;
    let mut hints = Vec::new();
    for r in &list {
        if col!(r, 2, bool) {
            hints.push(json!({"who": col!(r, 0, String), "at": iso(col!(r, 1, OffsetDateTime)), "counted": true}));
        }
    }
    Ok(json!({"windowS": DEMAND_WINDOW_S, "hints": hints, "plan": null}))
}

const HOST_ERROR: &str = "(e.kind like '%fail%' or e.kind like '%lost%' or e.data ? 'error')";

/// hermit's errors of the last 15 minutes: failed starts and host errors (egress's are added by [`build`]).
async fn recent_local(core: &Core) -> Result<Value, String> {
    let win = i64::from(RECENT_MINUTES);
    let counts = rows(
        core,
        &format!(
            "select (select count(*) from sandbox_starts where not ok and at > now() - $1::bigint * interval '1 minute'),
                    (select count(*) from host_events e where e.at > now() - $1::bigint * interval '1 minute'
                       and {HOST_ERROR})"
        ),
        &[&win],
    )
    .await?;
    let list = rows(
        core,
        &format!(
            "(select st.at, 'sandbox start failed' as what,
                     coalesce(st.reason, '') || coalesce(': ' || st.error, '') as detail, st.person as owner
                from sandbox_starts st where not st.ok and st.at > now() - $1::bigint * interval '1 minute')
             union all
             (select e.at, e.kind, coalesce(e.host, '') || coalesce(' ' || (e.data->>'error'), ''), s.person
                from host_events e left join sandboxes s on s.id = e.sandbox_id
               where e.at > now() - $1::bigint * interval '1 minute' and {HOST_ERROR})
             order by at desc limit 20"
        ),
        &[&win],
    )
    .await?;
    let c = counts.first().ok_or("no counts")?;
    let mut out = Vec::new();
    for r in &list {
        let detail: Option<String> = col!(r, 2, Option<String>);
        out.push(json!({
            "at": iso(col!(r, 0, OffsetDateTime)),
            "what": col!(r, 1, String),
            "detail": detail.map(|d| d.chars().take(300).collect::<String>()),
            "owner": col!(r, 3, Option<String>),
        }));
    }
    Ok(json!({
        "failedStarts": col!(c, 0, i64),
        "hostErrors": col!(c, 1, i64),
        "modelErrors": null,
        "blocked": null,
        "list": out,
    }))
}

/// Hosts gone in the last 24 hours (the 30 newest) and how long each lived.
async fn hosts_gone(core: &Core) -> Result<Value, String> {
    let list = rows(
        core,
        "select name, kind, instance_id, created_at, gone_at, gone_reason,
                (select count(*) from hosts where status = 'gone' and gone_at > now() - interval '24 hours')
           from hosts where status = 'gone' and gone_at > now() - interval '24 hours'
          order by gone_at desc limit $1",
        &[&GONE_HOSTS_SHOWN],
    )
    .await?;
    let mut count = 0;
    let mut out = Vec::new();
    for r in &list {
        count = col!(r, 6, i64);
        let launched: OffsetDateTime = col!(r, 3, OffsetDateTime);
        let gone: Option<OffsetDateTime> = col!(r, 4, Option<OffsetDateTime>);
        out.push(json!({
            "name": col!(r, 0, String),
            "kind": col!(r, 1, String),
            "instanceId": col!(r, 2, Option<String>),
            "launchedAt": iso(launched),
            "goneAt": opt_iso(gone),
            "goneReason": col!(r, 5, Option<String>),
            "livedS": gone.map(|g| secs_between(launched, g)),
        }));
    }
    Ok(json!({"count24h": count, "list": out}))
}

/// The 60 newest host events with the sandbox's person (A3.F49).
async fn events_section(core: &Core) -> Result<Value, String> {
    let list = rows(
        core,
        "select e.at, e.kind, e.host, e.sandbox_id, s.person, s.conversation_id, e.data
           from host_events e left join sandboxes s on s.id = e.sandbox_id order by e.at desc, e.id desc limit $1",
        &[&EVENTS_SHOWN],
    )
    .await?;
    let mut out = Vec::new();
    for r in &list {
        out.push(json!({
            "at": iso(col!(r, 0, OffsetDateTime)),
            "kind": col!(r, 1, String),
            "host": col!(r, 2, Option<String>),
            "sandboxId": col!(r, 3, Option<Uuid>),
            "owner": col!(r, 4, Option<String>),
            "conversationId": col!(r, 5, Option<Uuid>),
            "data": col!(r, 6, Value),
        }));
    }
    Ok(Value::Array(out))
}

/// Starts (with how long they took) and releases (with how long the sandbox lived) of the last 24 hours, the 80
/// newest, and the summary (A3.F42). `what` is `start` or `release` and nothing else; `owner` is the email.
async fn starts_section(core: &Core) -> Result<Value, String> {
    let sum = rows(
        core,
        "select count(*), count(*) filter (where ok), count(*) filter (where not ok),
                percentile_cont(0.5) within group (order by ms) filter (where ok),
                percentile_cont(0.95) within group (order by ms) filter (where ok)
           from sandbox_starts where at > now() - interval '24 hours'",
        &[],
    )
    .await?;
    let releases = rows(
        core,
        "select coalesce(release_reason, '?'), count(*) from sandboxes
          where status = 'gone' and released_at > now() - interval '24 hours' group by 1 order by 2 desc, 1",
        &[],
    )
    .await?;
    let list = rows(
        core,
        "(select st.at, 'start' as what, st.ok, st.ms::bigint as ms, st.host, st.conversation_id,
                 coalesce(st.person, s.person) as owner, case when st.ok then null else coalesce(st.reason, 'failed') end
            from sandbox_starts st left join sandboxes s on s.id = st.sandbox_id
           where st.at > now() - interval '24 hours')
         union all
         (select released_at, 'release', null, (extract(epoch from released_at - created_at) * 1000)::bigint, host,
                 conversation_id, person, left(release_reason, 120)
            from sandboxes where status = 'gone' and released_at > now() - interval '24 hours')
         order by at desc limit $1",
        &[&STARTS_SHOWN],
    )
    .await?;
    let s0 = sum.first().ok_or("no summary")?;
    let mut rel = Vec::new();
    for r in &releases {
        rel.push(json!({"reason": col!(r, 0, String), "n": col!(r, 1, i64)}));
    }
    let mut out = Vec::new();
    for r in &list {
        out.push(json!({
            "at": iso(col!(r, 0, OffsetDateTime)),
            "what": col!(r, 1, String),
            "ok": col!(r, 2, Option<bool>),
            "ms": col!(r, 3, Option<i64>),
            "host": col!(r, 4, Option<String>),
            "conversationId": col!(r, 5, Option<Uuid>),
            "owner": col!(r, 6, Option<String>),
            "reason": col!(r, 7, Option<String>),
        }));
    }
    Ok(json!({
        "summary": {
            "n": col!(s0, 0, i64),
            "ok": col!(s0, 1, i64),
            "failed": col!(s0, 2, i64),
            "p50Ms": col!(s0, 3, Option<f64>),
            "p95Ms": col!(s0, 4, Option<f64>),
        },
        "releases": rel,
        "list": out,
    }))
}

/// The operator switches (16): sandboxes disabled and why, the fresh-host test's end.
async fn controls_section(core: &Core) -> Result<Value, String> {
    let r = rows(
        core,
        "select sandboxes_enabled, disabled_reason, fresh_host_until from controls where id = 1",
        &[],
    )
    .await?;
    let r = r.first().ok_or("no controls row")?;
    Ok(json!({
        "sandboxesEnabled": col!(r, 0, bool),
        "disabledReason": col!(r, 1, Option<String>),
        "freshHostUntil": opt_iso(col!(r, 2, Option<OffsetDateTime>)),
    }))
}

/// A replica reports every 10 s; one silent for longer than this has stopped reporting.
pub const REPLICA_SILENT_S: i64 = 30;

/// Each hermit replica's own report (its workspace mount, a refused workspace delete), with `reporting` false for a
/// replica silent for more than 30 s. [`build`] keeps a silent one only while its pod still runs.
async fn replicas_section(core: &Core) -> Result<Value, String> {
    let list = rows(
        core,
        "select pod, seen_at, store_error, delete_refused_at, delete_refused_path,
                seen_at > now() - $1::bigint * interval '1 second'
           from replicas where seen_at > now() - interval '1 day' order by pod",
        &[&REPLICA_SILENT_S],
    )
    .await?;
    let mut out = Vec::new();
    for r in &list {
        out.push(json!({
            "pod": col!(r, 0, String),
            "seenAt": iso(col!(r, 1, OffsetDateTime)),
            "storeError": col!(r, 2, Option<String>),
            "deleteRefusedAt": opt_iso(col!(r, 3, Option<OffsetDateTime>)),
            "deleteRefusedPath": col!(r, 4, Option<String>),
            "reporting": col!(r, 5, bool),
        }));
    }
    Ok(Value::Array(out))
}

/// Conversations per staff member (A17.F16): every explicit catalogue user (zeros when magpie has nothing), then
/// every other person magpie reports (`former` when the catalogue no longer allows them); newest activity first.
fn people_section(core: &Core, mp: &magpie_api::ops::PeopleResponse, live_by: &HashMap<String, u64>) -> Value {
    let catalogue = core.catalogue.get();
    let explicit: Vec<String> = catalogue
        .as_ref()
        .map(|c| c.users.iter().cloned().collect())
        .unwrap_or_default();
    let by: BTreeMap<&str, &magpie_api::ops::PersonStats> = mp.people.iter().map(|p| (p.owner.as_str(), p)).collect();
    let row = |owner: &str| {
        let p = by.get(owner);
        let allowed = core.catalogue.allowed(owner);
        let mut v = json!({
            "owner": owner,
            "total": p.map_or(0, |p| p.total),
            "today": p.map_or(0, |p| p.today),
            "last7d": p.map_or(0, |p| p.last7d),
            "live": live_by.get(owner).copied().unwrap_or(0),
            "deleted": p.map_or(0, |p| p.deleted),
            "purged": p.map_or(0, |p| p.purged),
            "lastActiveAt": p.and_then(|p| p.last_active_at),
        });
        if !allowed {
            v["former"] = json!(true);
        }
        v
    };
    let mut out: Vec<Value> = explicit.iter().map(|u| row(u)).collect();
    for p in &mp.people {
        if !explicit.iter().any(|u| u == &p.owner) {
            out.push(row(&p.owner));
        }
    }
    out.sort_by(|a, b| {
        b["lastActiveAt"]
            .as_str()
            .unwrap_or("")
            .cmp(a["lastActiveAt"].as_str().unwrap_or(""))
    });
    Value::Array(out)
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    #[test]
    fn day_and_month_in_the_zone() {
        let now = datetime!(2026-11-02 17:00 UTC);
        let (tz, d, m) = day_and_month("Asia/Hong_Kong", now);
        assert_eq!(
            (tz.as_str(), d, m),
            (
                "Asia/Hong_Kong",
                datetime!(2026-11-02 16:00 UTC),
                datetime!(2026-10-31 16:00 UTC)
            )
        );
        let (_, d, m) = day_and_month("UTC", now);
        assert_eq!(
            (d, m),
            (datetime!(2026-11-02 00:00 UTC), datetime!(2026-11-01 00:00 UTC))
        );
        // London's month began in summer time (BST), the day in winter time
        let (_, d, m) = day_and_month("Europe/London", datetime!(2026-10-01 12:00 UTC));
        assert_eq!(
            (d, m),
            (datetime!(2026-09-30 23:00 UTC), datetime!(2026-09-30 23:00 UTC))
        );
        // Chile skips midnight when summer time starts: the day begins at 01:00 local (-03)
        let (_, d, _) = day_and_month("America/Santiago", datetime!(2026-09-06 12:00 UTC));
        assert_eq!(d, datetime!(2026-09-06 04:00 UTC));
        // a zone the caller did not validate falls back to UTC, and says so
        assert_eq!(day_and_month("Mars/Olympus", now).0, "UTC");
    }

    #[test]
    fn a_deployment_scaled_to_zero_is_ok() {
        let dep = |name: &str, desired: i64, ready: i64| {
            json!({"metadata": {"name": name, "generation": 2}, "spec": {"replicas": desired},
                   "status": {"observedGeneration": 2, "replicas": ready, "readyReplicas": ready,
                              "updatedReplicas": ready}})
        };
        let names = ["off".to_owned(), "down".to_owned()];
        let v = service_of(
            &names,
            &[dep("off", 0, 0), dep("down", 2, 0)],
            &[],
            "",
            datetime!(2026-11-02 00:00 UTC),
        );
        let states: Vec<&str> = v["deployments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["state"].as_str().unwrap())
            .collect();
        assert_eq!(states, ["ok", "problem"]);
    }
}
