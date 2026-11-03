//! The scaler (hermit 13.4-13.9): the leader of lock 7300/4 runs a pass every 5 s: the gateway row, heartbeats,
//! vanished sandboxes, host loss, `JuiceFS` gating, network pushes, EC2 reconcile, capacity planning and launches,
//! the release of one empty host, moves off retiring hosts and packing (at most one terminate and one migration per
//! pass). Every network call runs outside a transaction; launches decide and record their `booting` rows under lock
//! 7300/7 before `RunInstances`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use hermit_api::sandbox::ReleaseReason;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::config::{Config, parse_quantity};
use crate::db::LOCK_CLASS;
use crate::fabric::ec2::{self, Instance};
use crate::fabric::gvisor::{Gvisor, bare_digest, now_ms};
use crate::fabric::hosts::{self, HostInfo, HostKind, HostRow, HostStatus, Sizing, Slot};
use crate::fabric::packing::{PackHost, PackSandbox, plan_packing};
use crate::fabric::placement::{self, LOCK_PLACE, Rules};
use crate::fabric::{FabricError, Launch, images, network};
use crate::sandboxes;
use crate::tasks::{self, Claim, Kind};
use crate::{Core, ms};

/// Lock 7300/7: a host launch (the `booting` rows are inserted and committed; `RunInstances` runs after).
pub const LOCK_LAUNCH: i32 = 7;
/// Demand hints count for this long (13.5).
pub const DEMAND_WINDOW: Duration = Duration::from_secs(120);
/// A live row whose instance is missing is lost after this long (13.9).
pub const MISSING_INSTANCE: Duration = Duration::from_secs(120);
/// A name a host lists with no row is an orphan once it is this old (12 step 8).
pub const ORPHAN_AGE: Duration = Duration::from_secs(60);
/// Random names drawn for a new host before the launch pass gives up (one in 16.7 million is taken per held name).
pub const HOST_NAME_TRIES: usize = 8;

/// What one pass did (tests and the log read it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub gateway_up: bool,
    pub activated: Vec<String>,
    pub retired: Vec<String>,
    pub lost_hosts: Vec<String>,
    pub vanished: Vec<Uuid>,
    pub launched: Vec<String>,
    pub terminated: Vec<String>,
    pub orphans: Vec<String>,
    pub migration: Option<(Uuid, String, String)>,
}

impl Report {
    #[must_use]
    pub fn is_quiet(&self) -> bool {
        Self {
            gateway_up: self.gateway_up,
            ..Self::default()
        } == *self
    }
}

fn secs(v: u32) -> Duration {
    Config::secs(v)
}

fn age(now: OffsetDateTime, t: OffsetDateTime) -> Duration {
    (now - t).try_into().unwrap_or(Duration::ZERO)
}

/// The host-role row of the configured gateway (13.4 step 1): upserted `active`; a row of another name at the same
/// overlay address goes `gone` (`replaced by <name>`).
async fn ensure_gateway(core: &Core) -> Result<(), FabricError> {
    let g = &core.cfg.file.fabric.gateway;
    let ip: IpAddr =
        gateway_ip(&g.url).ok_or_else(|| FabricError(format!("fabric.gateway.url {} has no address", g.url)))?;
    let (name, url) = (g.name.clone(), g.url.clone());
    aviary_pg::tx!(&core.pool, |t| -> Result<(), aviary_pg::Error> {
        t.execute(
            "update hosts set status = 'gone', gone_at = now(), gone_reason = 'replaced by ' || $1
             where name <> $1 and wg_ip = $2 and status <> 'gone'",
            &[&name, &ip],
        )
        .await?;
        t.execute(
            "insert into hosts (name, kind, url, wg_ip, status) values ($1, 'gateway', $2, $3, 'active')
             on conflict (name) do update set url = excluded.url, wg_ip = excluded.wg_ip,
               status = case when hosts.status = 'gone' then 'active' else hosts.status end,
               gone_at = case when hosts.status = 'gone' then null else hosts.gone_at end,
               gone_reason = case when hosts.status = 'gone' then null else hosts.gone_reason end",
            &[&name, &url, &ip],
        )
        .await?;
        Ok(())
    })
    .await?;
    Ok(())
}

/// The address of `https://<ip>:<port>`.
#[must_use]
pub fn gateway_ip(url: &str) -> Option<IpAddr> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?']).next()?;
    let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
    host.trim_start_matches('[').trim_end_matches(']').parse().ok()
}

/// A replica that does not lead: reads the gateway's "answering since" from its row, for the silence rule.
///
/// # Errors
/// A database error.
pub async fn follow(g: &Gvisor, core: &Core) -> Result<(), FabricError> {
    let name = core.cfg.file.fabric.gateway.name.clone();
    let row = hosts::get(&core.pool, &name).await?;
    let fresh = Config::secs(core.cfg.file.fabric.host_dead_seconds);
    let since = row
        .filter(|h| h.fresh(OffsetDateTime::now_utc(), fresh))
        .and_then(|h| h.info.and_then(|i| i.up_since))
        .unwrap_or(0);
    g.gateway_up_since.store(since, Ordering::Release);
    Ok(())
}

/// One heartbeat's outcome.
struct Beat {
    host: HostRow,
    answer: Option<sandboxd_api::Host>,
}

/// The CPU features of `want` (comma-separated) that `have` lacks.
#[must_use]
pub fn missing_features(want: &str, have: &str) -> Vec<String> {
    let have: HashSet<&str> = have.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    want.split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty() && !have.contains(f))
        .map(str::to_owned)
        .collect()
}

/// One scaler pass (hermit 13.4); its end is recorded on the gateway's row (`info.lastPassAt`) for the ops page.
///
/// # Errors
/// A database error (the pass stops; the next one starts over).
pub async fn pass(g: &Gvisor, core: &Arc<Core>) -> Result<Report, FabricError> {
    let r = steps(g, core).await?;
    let name = core.cfg.file.fabric.gateway.name.clone();
    core.pool
        .get()
        .await?
        .execute(
            "update hosts set info = jsonb_set(coalesce(info, '{}'::jsonb), '{lastPassAt}', to_jsonb($2::bigint))
             where name = $1 and status <> 'gone'",
            &[&name, &now_ms()],
        )
        .await?;
    Ok(r)
}

/// What the ops page shows of the fabric (hermit 18.3, 18.6): the plan, the leader's last pass, the gateway's egress
/// target count, the slots of every live host.
///
/// # Errors
/// A database error.
pub async fn ops(core: &Core) -> Result<crate::fabric::FabricOps, FabricError> {
    let sizing = Gvisor::sizing(&core.cfg);
    let dead = secs(core.cfg.file.fabric.host_dead_seconds);
    let cfg = core.cfg.clone();
    let plan = aviary_pg::tx!(&core.pool, |t| -> Result<Plan, aviary_pg::Error> {
        let rules = placement::rules_in(t, sizing, dead).await?;
        plan_in(t, &cfg, &rules, false).await
    })
    .await?;
    let hosts = hosts::live(&core.pool).await?;
    let gw = hosts.iter().find(|h| h.is_gateway());
    let last_pass_at = gw
        .and_then(|h| h.info.as_ref())
        .and_then(|i| i.last_pass_at)
        .and_then(hermit_api::Timestamp::from_unix_millis);
    let egress_targets = gw.and_then(|h| h.info.as_ref()).map(|i| i.egress_targets.len());
    let planned = if plan.want > 0 {
        u32::try_from((plan.want + i64::from(plan.per_host.max(1)) - 1) / i64::from(plan.per_host.max(1)))
            .unwrap_or(u32::MAX)
    } else {
        0
    };
    Ok(crate::fabric::FabricOps {
        plan: Some(crate::fabric::ScalerPlan {
            need: plan.need,
            demand: plan.demand,
            waiting: plan.waiting,
            planned,
            available: plan.available,
            pending: plan.pending,
            per_host: plan.per_host,
        }),
        last_pass_at,
        egress_targets,
        slots: hosts.iter().map(|h| (h.name.clone(), sizing.host_slots(h))).collect(),
    })
}

#[allow(clippy::too_many_lines)] // the steps of hermit 13.4 in order
async fn steps(g: &Gvisor, core: &Arc<Core>) -> Result<Report, FabricError> {
    let mut report = Report::default();
    let f = &core.cfg.file.fabric;
    // 1. the gateway row
    ensure_gateway(core).await?;
    let live = hosts::live(&core.pool).await?;

    // 2. heartbeats, in parallel
    let t0 = OffsetDateTime::now_utc();
    let beats: Vec<Beat> = futures_util::future::join_all(live.iter().map(|h| async {
        let answer = match g.client(h) {
            Ok(c) => c.host().await.ok(),
            Err(_) => None,
        };
        Beat {
            host: h.clone(),
            answer,
        }
    }))
    .await;
    let now = OffsetDateTime::now_utc();
    let mut gateway_up = false;
    let mut down: Vec<(HostRow, Duration)> = Vec::new();
    let mut listed: HashMap<String, HostInfo> = HashMap::new();
    for b in &beats {
        let h = &b.host;
        let Some(a) = &b.answer else {
            if h.status == HostStatus::Booting {
                if age(now, h.created_at) > secs(f.boot_timeout_seconds) {
                    host_lost(g, core, h, "did not come up").await?;
                    report.lost_hosts.push(h.name.clone());
                }
            } else {
                down.push((h.clone(), age(now, h.last_seen_at.unwrap_or(h.created_at))));
            }
            continue;
        };
        if h.is_gateway() {
            gateway_up = true;
        }
        let (info, status, retire_by) = heartbeat(g, core, h, a, &mut report).await;
        let info_v = serde_json::to_value(&info).unwrap_or_default();
        let (mem_total, mem_reserve) = (
            i64::try_from(a.mem_total).unwrap_or(i64::MAX),
            i64::try_from(a.reserve_memory).unwrap_or(i64::MAX),
        );
        let cpus = i32::try_from(a.cpus).unwrap_or(i32::MAX);
        let name = h.name.clone();
        let st = status.as_str();
        core.pool
            .get()
            .await?
            .execute(
                "update hosts set last_seen_at = now(), mem_total = $2, mem_reserve = $3, cpus = $4,
                   wg_public_key = $5, info = $6, status = $7, retire_by = $8
                 where name = $1 and status <> 'gone'",
                &[
                    &name,
                    &mem_total,
                    &mem_reserve,
                    &cpus,
                    &a.wg_public_key,
                    &info_v,
                    &st,
                    &retire_by,
                ],
            )
            .await?;
        if status == HostStatus::Booting && age(now, h.created_at) > secs(f.boot_timeout_seconds) {
            host_lost(g, core, h, "JuiceFS not mounted").await?;
            report.lost_hosts.push(h.name.clone());
            continue;
        }
        if !h.is_gateway() {
            listed.insert(h.name.clone(), info);
        }
    }
    // the gateway's "answering since", kept on its row for every replica (12 step 3)
    let since = if gateway_up {
        let prev = g.gateway_up_since.load(Ordering::Acquire);
        if prev > 0 { prev } else { now_ms() }
    } else {
        0
    };
    g.gateway_up_since.store(since, Ordering::Release);
    report.gateway_up = gateway_up;
    {
        let name = f.gateway.name.clone();
        let v = (since > 0).then_some(since);
        core.pool
            .get()
            .await?
            .execute(
                "update hosts set info = jsonb_set(coalesce(info, '{}'::jsonb), '{upSince}', to_jsonb($2::bigint))
                 where name = $1 and status <> 'gone' and $2::bigint is not null",
                &[&name, &v],
            )
            .await?;
        if v.is_none() {
            core.pool
                .get()
                .await?
                .execute(
                    "update hosts set info = info - 'upSince' where name = $1 and info is not null",
                    &[&name],
                )
                .await?;
        }
    }

    // 3. vanished sandboxes: a ready row its reachable host does not list, or lists as not alive
    report.vanished = vanished(core, &listed, t0).await?;

    // 4. host loss
    for (h, silent) in &down {
        if h.is_gateway() {
            if *silent > secs(f.gateway_dead_seconds)
                && h.last_seen_at.is_some()
                && !g.gateway_lost_reported.swap(true, Ordering::AcqRel)
            {
                tracing::error!(host = %h.name, "gateway lost: every sandbox has lost its egress and control path");
                hosts::event(
                    &core.pool,
                    "host-lost",
                    Some(&h.name),
                    None,
                    serde_json::json!({"reason": "stopped answering", "sandboxes": 0}),
                )
                .await;
            }
            continue;
        }
        // AWS hosts are reached through the gateway: judged only once it has answered for hostDeadSeconds
        if g.gateway_up_for(secs(f.host_dead_seconds)) && *silent > secs(f.host_dead_seconds) {
            host_lost(g, core, h, "stopped answering").await?;
            report.lost_hosts.push(h.name.clone());
        }
    }
    if !gateway_up {
        // without the gateway nothing else can be trusted
        return Ok(report);
    }
    g.gateway_lost_reported.store(false, Ordering::Release);

    // 6. network
    if let Err(e) = g.push_gateway_peers(core).await {
        tracing::warn!(error = %e, "gateway peers");
    }
    push_mesh_and_egress(g, core).await;

    // 8. EC2 reconcile and the orphan sweep, at most every Timings::reconcile
    let due = {
        let mut last = g
            .last_reconcile
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let due = last.is_none_or(|l| l.elapsed() >= core.timings.reconcile);
        if due {
            *last = Some(tokio::time::Instant::now());
        }
        due
    };
    if due {
        if let Err(e) = reconcile(g, core, &mut report).await {
            tracing::warn!(error = %e, "EC2 reconcile");
        }
        report.orphans = orphans(g, core, &beats).await?;
    }

    // 9. bookkeeping and launches
    let sizing = Gvisor::sizing(&core.cfg);
    let units = units(core, &sizing).await?;
    {
        let c = core.pool.get().await?;
        for h in hosts::live(&core.pool)
            .await?
            .iter()
            .filter(|h| h.kind == HostKind::Aws)
        {
            let empty = units.get(&h.name).copied().unwrap_or(0) == 0;
            c.execute(
                "update hosts set empty_since = case when $2::boolean then coalesce(empty_since, now()) else null end
                 where name = $1",
                &[&h.name, &empty],
            )
            .await?;
        }
        c.execute("delete from demand_hints where at < now() - interval '1 hour'", &[])
            .await?;
    }
    match launch(g, core, "scaler", false).await {
        Ok(Launch::Launched) => report.launched.push("scaler".into()),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "launch"),
    }
    let hosts_now = hosts::live(&core.pool).await?;
    let retiring_busy = hosts_now
        .iter()
        .any(|h| h.status == HostStatus::Retiring && units.get(&h.name).copied().unwrap_or(0) > 0);
    if retiring_busy {
        match launch(g, core, "replacing a retiring host", true).await {
            Ok(Launch::Launched) => report.launched.push("transient".into()),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "launch (transient)"),
        }
    }

    // 10. release one empty host
    if let Some(name) = release_empty(g, core).await? {
        report.terminated.push(name);
        return Ok(report);
    }

    // 11. move off retiring hosts, 12. packing
    report.migration = moves(g, core, &sizing).await?;
    Ok(report)
}

/// Step 2 for one answered host: the new info, status and `retire_by`, with their events.
async fn heartbeat(
    g: &Gvisor,
    core: &Core,
    h: &HostRow,
    a: &sandboxd_api::Host,
    report: &mut Report,
) -> (HostInfo, HostStatus, Option<String>) {
    let f = &core.cfg.file.fabric;
    let now = now_ms();
    let prev = h.info.as_ref();
    let mut info = HostInfo::from_host(a, prev);
    let mut status = h.status;
    let mut retire_by = h.retire_by.clone();
    if h.is_gateway() {
        return (info, status, retire_by);
    }
    // a booting host is active once sandboxd answers and its JuiceFS mount is there
    if status == HostStatus::Booting && a.jfs_mounted != Some(false) {
        status = HostStatus::Active;
        info.active_at = Some(now);
        let boot_ms = now - h.created_at.unix_timestamp() * 1000;
        tracing::info!(host = %h.name, boot_ms, "host active");
        hosts::event(
            &core.pool,
            "active",
            Some(&h.name),
            None,
            serde_json::json!({"bootMs": boot_ms}),
        )
        .await;
        report.activated.push(h.name.clone());
        images::warm(g, h, &core.cfg.image_digest);
    }
    // JuiceFS gating (step 5)
    let jfs_down = status == HostStatus::Active && a.jfs_mounted == Some(false);
    if jfs_down {
        if prev.and_then(|p| p.jfs_mounted) != Some(false) || info.jfs_unmounted_since.is_none() {
            tracing::error!(host = %h.name, "JuiceFS not mounted on an active host (no new sandboxes there)");
            hosts::event(&core.pool, "jfs-unmounted", Some(&h.name), None, serde_json::json!({})).await;
        }
        let since = *info.jfs_unmounted_since.get_or_insert(now);
        if h.kind == HostKind::Aws && now - since > ms(secs(f.jfs_retire_seconds)) {
            tracing::error!(host = %h.name, "JuiceFS not remounted: retiring the host");
            hosts::event(
                &core.pool,
                "jfs-retire",
                Some(&h.name),
                None,
                serde_json::json!({"since": since}),
            )
            .await;
            status = HostStatus::Retiring;
            retire_by = Some("system".into());
        }
    } else {
        if h.status == HostStatus::Active
            && a.jfs_mounted == Some(true)
            && prev.and_then(|p| p.jfs_mounted) == Some(false)
        {
            let down_ms = info.jfs_unmounted_since.map(|s| now - s);
            hosts::event(
                &core.pool,
                "jfs-mounted",
                Some(&h.name),
                None,
                serde_json::json!({"downMs": down_ms}),
            )
            .await;
        }
        info.jfs_unmounted_since = None;
    }
    let missing = missing_features(&f.cpu_features, &a.cpu_features);
    if !missing.is_empty() && matches!(status, HostStatus::Active | HostStatus::Booting) {
        tracing::error!(host = %h.name, missing = %missing.join(","), "host lacks CPU features of the pool: retiring");
        hosts::event(
            &core.pool,
            "retire",
            Some(&h.name),
            None,
            serde_json::json!({"missing": missing}),
        )
        .await;
        status = HostStatus::Retiring;
        retire_by = Some("system".into());
    }
    if status == HostStatus::Retiring && h.status != HostStatus::Retiring {
        report.retired.push(h.name.clone());
    }
    (info, status, retire_by)
}

/// 13.4 step 3: every reachable host's `ready` rows with no live claim and no `migrating_to` that it does not list
/// (or lists with `alive: false`) are lost `vanished`. Only rows ready before the heartbeat was asked count, and
/// none moved since (a heartbeat taken before a row became ready or moved cannot list it).
async fn vanished(
    core: &Arc<Core>,
    listed: &HashMap<String, HostInfo>,
    t0: OffsetDateTime,
) -> Result<Vec<Uuid>, FabricError> {
    if listed.is_empty() {
        return Ok(Vec::new());
    }
    let names: Vec<String> = listed.keys().cloned().collect();
    let rows = core
        .pool
        .get()
        .await?
        .query(
            "select s.id, s.name, s.host from sandboxes s
             where s.status = 'ready' and s.host = any($1) and s.migrating_to is null
               and (s.task is null or s.task_until < now()) and s.ready_at < $2
               and not exists (select 1 from host_events e where e.sandbox_id = s.id
                               and e.kind in ('migration', 'migration-adopted') and e.at >= $2)",
            &[&names, &t0],
        )
        .await?;
    let mut out = Vec::new();
    for r in rows {
        let (id, name, host): (Uuid, String, String) = (r.try_get(0)?, r.try_get(1)?, r.try_get(2)?);
        let Some(info) = listed.get(&host) else { continue };
        let gone = match info.sandbox(&name) {
            None => Some("not listed by its host"),
            Some(s) if s.alive == Some(false) => Some("its host lists it as not alive"),
            Some(_) => None,
        };
        if let Some(why) = gone {
            tracing::warn!(sandbox = %id, host = %host, why, "sandbox vanished");
            hosts::event(
                &core.pool,
                "vanished",
                Some(&host),
                Some(id),
                serde_json::json!({"why": why}),
            )
            .await;
            sandboxes::request(core, id, ReleaseReason::Vanished, true, Some(why.to_owned())).await?;
            out.push(id);
        }
    }
    Ok(out)
}

/// 13.7: every non-gone sandbox on the host (or migrating to it with its source also gone) goes `gone`
/// (`host_lost`, `sandbox_lost`); an AWS host is then terminated.
///
/// # Errors
/// A database error.
pub async fn host_lost(g: &Gvisor, core: &Arc<Core>, h: &HostRow, reason: &str) -> Result<(), FabricError> {
    let name = h.name.clone();
    let gone_reason = format!("lost: {reason}");
    if h.kind == HostKind::Aws {
        core.pool
            .get()
            .await?
            .execute(
                "update hosts set status = 'gone', gone_at = now(), gone_reason = $2 where name = $1 and status <> 'gone'",
                &[&name, &gone_reason],
            )
            .await?;
        g.clients.forget(&name);
    }
    let ids: Vec<Uuid> = core
        .pool
        .get()
        .await?
        .query(
            "select id from sandboxes s where s.status <> 'gone'
               and (s.host = $1 or (s.migrating_to = $1 and (s.host is null or not exists
                    (select 1 from hosts h where h.name = s.host and h.status <> 'gone'))))",
            &[&name],
        )
        .await?
        .iter()
        .filter_map(|r| r.try_get(0).ok())
        .collect();
    for id in &ids {
        // nothing can be fetched from a lost host (platform 15.4): the row says so, and the release's destroy
        // does not spend 5 s asking
        crate::console::record_lost(core, *id).await;
        sandboxes::request(core, *id, ReleaseReason::HostLost, true, Some(reason.to_owned())).await?;
    }
    tracing::error!(host = %name, reason, sandboxes = ids.len(), "host lost");
    hosts::event(
        &core.pool,
        "host-lost",
        Some(&name),
        None,
        serde_json::json!({"reason": reason, "sandboxes": ids.len()}),
    )
    .await;
    if h.kind == HostKind::Aws {
        terminate_instance(g, core, h, &gone_reason).await;
    }
    Ok(())
}

/// `TerminateInstances` for a host whose row is already `gone`, and its `terminate` event.
async fn terminate_instance(g: &Gvisor, core: &Core, h: &HostRow, reason: &str) {
    if let (Some(id), Some(ec2)) = (&h.instance_id, &g.ec2)
        && let Err(e) = ec2.terminate(std::slice::from_ref(id)).await
    {
        // the reconcile terminates it as an orphan later
        tracing::warn!(host = %h.name, error = %e, "terminate failed");
    }
    let up_ms = ms(age(OffsetDateTime::now_utc(), h.created_at));
    hosts::event(
        &core.pool,
        "terminate",
        Some(&h.name),
        None,
        serde_json::json!({"instance": h.instance_id, "reason": reason, "upMs": up_ms}),
    )
    .await;
}

/// The mesh peers of every fresh non-booting AWS host and the egress targets on the gateway (13.8).
async fn push_mesh_and_egress(g: &Gvisor, core: &Core) {
    let Ok(hosts) = hosts::live(&core.pool).await else {
        return;
    };
    let dead = secs(core.cfg.file.fabric.host_dead_seconds);
    let now = OffsetDateTime::now_utc();
    let aws: Vec<&HostRow> = hosts
        .iter()
        .filter(|h| h.kind == HostKind::Aws && h.status != HostStatus::Booting && h.fresh(now, dead))
        .collect();
    futures_util::future::join_all(aws.iter().map(|h| async {
        let peers = network::mesh_peers(h, &hosts);
        if let Ok(c) = g.client(h)
            && let Err(e) = c.set_peers(peers).await
        {
            tracing::warn!(host = %h.name, error = %e, "mesh peers");
        }
    }))
    .await;
    if let (Some(targets), Some(gw)) = (g.egress.targets(), hosts.iter().find(|h| h.is_gateway())) {
        // with no ready endpoints the last list is kept (the ops page shows the problem)
        if targets.is_empty() {
            tracing::warn!("no ready egress endpoints: the gateway keeps its last targets");
        } else if let Ok(c) = g.client(gw)
            && let Err(e) = c.set_egress(targets).await
        {
            tracing::warn!(error = %e, "egress targets");
        }
    }
}

/// 13.9: unknown instances older than `orphanInstanceSeconds` are terminated; a live row whose instance is missing
/// (older than 2 min) is lost; missing private addresses are filled in.
async fn reconcile(g: &Gvisor, core: &Arc<Core>, report: &mut Report) -> Result<(), FabricError> {
    let Some(ec2) = &g.ec2 else {
        return Ok(());
    };
    let f = &core.cfg.file.fabric;
    let tags: Vec<(String, String)> = f.aws.tags.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let insts = ec2.describe(&tags).await.map_err(|e| FabricError(e.to_string()))?;
    let rows: Vec<HostRow> = core
        .pool
        .get()
        .await?
        .query("select * from hosts where kind = 'aws'", &[])
        .await?
        .iter()
        .map(HostRow::from_row)
        .collect::<Result<_, _>>()?;
    let known: HashMap<&str, &HostRow> = rows
        .iter()
        .filter(|r| r.status != HostStatus::Gone)
        .filter_map(|r| Some((r.instance_id.as_deref()?, r)))
        .collect();
    let now = now_ms();
    let orphans: Vec<String> = insts
        .iter()
        .filter(|i| {
            !known.contains_key(i.id.as_str())
                && i.state != "shutting-down"
                && now - i.launched_ms > ms(secs(f.orphan_instance_seconds))
        })
        .map(|i| i.id.clone())
        .collect();
    if !orphans.is_empty() {
        tracing::warn!(instances = ?orphans, "terminating instances hermit does not know");
        ec2.terminate(&orphans).await.map_err(|e| FabricError(e.to_string()))?;
        for id in &orphans {
            hosts::event(
                &core.pool,
                "terminate",
                None,
                None,
                serde_json::json!({"instance": id, "reason": "orphan"}),
            )
            .await;
        }
    }
    let running: HashMap<&str, &Instance> = insts.iter().map(|i| (i.id.as_str(), i)).collect();
    for h in known.values() {
        let id = h.instance_id.as_deref().unwrap_or_default();
        match running.get(id) {
            None if age(OffsetDateTime::now_utc(), h.created_at) > MISSING_INSTANCE => {
                host_lost(g, core, h, "instance no longer exists").await?;
                report.lost_hosts.push(h.name.clone());
            }
            Some(i) if h.private_ip.is_none() && i.private_ip.is_some() => {
                core.pool
                    .get()
                    .await?
                    .execute(
                        "update hosts set private_ip = $2 where name = $1",
                        &[&h.name, &i.private_ip],
                    )
                    .await?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Janitor step 8 (here, where the hosts' lists are): a name listed by a reachable host with no non-gone row, older
/// than 60 s, is destroyed there (`host_events` `orphan`).
async fn orphans(g: &Gvisor, core: &Core, beats: &[Beat]) -> Result<Vec<String>, FabricError> {
    let live: HashSet<String> = core
        .pool
        .get()
        .await?
        .query("select name from sandboxes where status <> 'gone'", &[])
        .await?
        .iter()
        .filter_map(|r| r.try_get(0).ok())
        .collect();
    let now = OffsetDateTime::now_utc();
    let mut out = Vec::new();
    for b in beats {
        let Some(a) = &b.answer else { continue };
        for s in &a.sandboxes {
            if live.contains(&s.name) {
                continue;
            }
            let created = OffsetDateTime::parse(&s.created_at, &time::format_description::well_known::Rfc3339).ok();
            if created.is_none_or(|c| age(now, c) < ORPHAN_AGE) {
                continue;
            }
            let Ok(c) = g.client(&b.host) else { continue };
            match c.delete(&s.name).await {
                Ok(_) => {
                    tracing::warn!(host = %b.host.name, sandbox = %s.name, "orphan destroyed");
                    hosts::event(
                        &core.pool,
                        "orphan",
                        Some(&b.host.name),
                        None,
                        serde_json::json!({"name": s.name}),
                    )
                    .await;
                    out.push(s.name.clone());
                }
                Err(e) => tracing::warn!(host = %b.host.name, sandbox = %s.name, error = %e, "orphan not destroyed"),
            }
        }
    }
    Ok(out)
}

/// The units each host carries (13.3).
async fn units(core: &Core, sizing: &Sizing) -> Result<HashMap<String, u32>, FabricError> {
    let s = *sizing;
    Ok(aviary_pg::tx!(&core.pool, |t| placement::units_in(t, &s).await).await?)
}

/// The numbers behind a launch decision (13.5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Free slots on placeable hosts.
    pub available: u32,
    /// Booting AWS hosts x `per_host`.
    pub pending: u32,
    pub per_host: u32,
    /// Demand hints younger than 120 s.
    pub demand: u32,
    /// Starting rows in phase `host`.
    pub waiting: u32,
    /// Sandboxes on retiring hosts (transient launches only).
    pub retiring: u32,
    pub need: u32,
    pub want: i64,
}

/// The plan, inside the launch's transaction.
async fn plan_in(
    t: &aviary_pg::Tx<'_>,
    cfg: &Config,
    rules: &Rules,
    transient: bool,
) -> Result<Plan, aviary_pg::Error> {
    let f = &cfg.file.fabric;
    let sizing = rules.sizing;
    let available: u32 = placement::slots_in(t, rules).await?.iter().map(|s| s.free).sum();
    let latest = t
        .query_opt(
            "select mem_total, mem_reserve, cpus from hosts where kind = 'aws' and mem_total is not null
             order by created_at desc limit 1",
            &[],
        )
        .await?;
    let per_host = if let Some(r) = latest {
        sizing.slots(r.try_get(0)?, r.try_get(1)?, r.try_get(2)?, None)
    } else {
        let fb = &f.aws.fallback_host;
        sizing.slots(
            i64::try_from(parse_quantity(&fb.memory).unwrap_or(0)).unwrap_or(0),
            i64::try_from(parse_quantity(&fb.reserve).unwrap_or(0)).unwrap_or(0),
            i32::try_from(fb.cpus).unwrap_or(0),
            None,
        )
    }
    .max(1);
    let count = |r: aviary_pg::Row| -> Result<u32, aviary_pg::Error> {
        Ok(u32::try_from(r.try_get::<_, i64>(0)?).unwrap_or(u32::MAX))
    };
    let booting = count(
        t.query_one(
            "select count(*) from hosts where kind = 'aws' and status = 'booting'",
            &[],
        )
        .await?,
    )?;
    let waiting = count(
        t.query_one(
            "select count(*) from sandboxes where status = 'starting' and phase = 'host'",
            &[],
        )
        .await?,
    )?;
    let window = ms(DEMAND_WINDOW);
    let demand = count(
        t.query_one(
            "select count(*) from demand_hints where at > now() - $1::bigint * interval '1 millisecond'",
            &[&window],
        )
        .await?,
    )?;
    let retiring = if transient {
        count(
            t.query_one(
                "select count(*) from sandboxes s join hosts h on h.name = s.host
                 where s.status <> 'gone' and h.status = 'retiring'",
                &[],
            )
            .await?,
        )?
    } else {
        0
    };
    let pending = booting.saturating_mul(per_host);
    let need = f.spare_slots + demand + waiting + retiring;
    Ok(Plan {
        available,
        pending,
        per_host,
        demand,
        waiting,
        retiring,
        need,
        want: i64::from(need) - i64::from(available) - i64::from(pending),
    })
}

/// A host the launch decided on: its row is committed `booting` before `RunInstances`.
struct Planned {
    name: String,
    keys: ec2::WgKeys,
    wg_ip: IpAddr,
}

/// `launch(reason, transient)` (13.5): under lock 7300/7 decides and inserts the `booting` rows and commits; then
/// `RunInstances` for each. `transient`: one more than `maxHosts` while a retiring host still has sandboxes.
///
/// # Errors
/// A database error.
pub async fn launch(g: &Gvisor, core: &Core, reason: &str, transient: bool) -> Result<Launch, FabricError> {
    let f = &core.cfg.file.fabric;
    let enabled: bool = core
        .pool
        .get()
        .await?
        .query_one("select sandboxes_enabled from controls where id = 1", &[])
        .await?
        .try_get(0)?;
    if f.aws.max_hosts == 0 || !enabled || g.ec2.is_none() || g.host_secrets.is_none() {
        return Ok(Launch::Off);
    }
    let sizing = Gvisor::sizing(&core.cfg);
    let dead = secs(f.host_dead_seconds);
    let cfg = core.cfg.clone();
    let max_hosts = f.aws.max_hosts;
    let host_cidr = f.host_cidr.clone();
    let gateway = f.gateway.name.clone();
    let decided = aviary_pg::tx!(&core.pool, |t| -> Result<
        (Launch, Vec<Planned>, Plan, String),
        FabricError,
    > {
        t.execute(
            "select pg_advisory_xact_lock($1::int4, $2::int4)",
            &[&LOCK_CLASS, &LOCK_LAUNCH],
        )
        .await?;
        let rules = placement::rules_in(t, sizing, dead).await?;
        let plan = plan_in(t, &cfg, &rules, transient).await?;
        if plan.want <= 0 {
            return Ok((Launch::Planned, Vec::new(), plan, String::new()));
        }
        let hosts = hosts::live_in(t).await?;
        let aws: Vec<&HostRow> = hosts.iter().filter(|h| h.kind == HostKind::Aws).collect();
        let retiring = aws.iter().filter(|h| h.status == HostStatus::Retiring).count();
        let cap = if transient && retiring > 0 {
            max_hosts + 1
        } else {
            max_hosts
        };
        let count = if transient { aws.len() } else { aws.len() - retiring };
        let room = i64::from(cap) - i64::try_from(count).unwrap_or(i64::MAX);
        let n = (plan.want + i64::from(plan.per_host) - 1) / i64::from(plan.per_host);
        let n = n.min(room);
        if n <= 0 {
            return Ok((Launch::Full, Vec::new(), plan, String::new()));
        }
        let Some(gw_key) = hosts
            .iter()
            .find(|h| h.name == gateway)
            .and_then(|h| h.wg_public_key.clone())
        else {
            // before the gateway's first heartbeat: the next pass launches (the waiting start counts)
            return Ok((Launch::Planned, Vec::new(), plan, String::new()));
        };
        let mut used: BTreeSet<IpAddr> = hosts.iter().map(|h| h.wg_ip).collect();
        let mut planned = Vec::new();
        for _ in 0..n {
            let ip = placement::free_address(&host_cidr, &used, 10, Some(251))
                .ok_or_else(|| FabricError("no free host address".into()))?;
            let ip = IpAddr::V4(ip);
            used.insert(ip);
            let keys = ec2::wg_keys().map_err(|_| FabricError("no random source".into()))?;
            let url = format!("https://{ip}:{}", sandboxd_api::PORT);
            let name = insert_host(t, &url, ip, &keys.public_key, &|| {
                ec2::host_name().map_err(|_| FabricError("no random source".into()))
            })
            .await?;
            planned.push(Planned { name, keys, wg_ip: ip });
        }
        Ok((Launch::Launched, planned, plan, gw_key))
    })
    .await?;
    let (answer, planned, plan, gw_key) = decided;
    if planned.is_empty() {
        return Ok(answer);
    }
    let why = format!(
        "{reason}; need {} (demand {}, waiting {}), planned {} (available {}, pending {}), {} slots per host",
        plan.need,
        plan.demand,
        plan.waiting,
        plan.available + plan.pending,
        plan.available,
        plan.pending,
        plan.per_host
    );
    for p in planned {
        run_instance(g, core, &p, &gw_key, &why, transient).await?;
    }
    Ok(Launch::Launched)
}

/// Inserts a new host's `booting` row under a name from `names` (the launch pass: [`ec2::host_name`], random), drawn
/// again while the name is one `hosts` holds already (an imported old host, hermit 22.3, or an earlier one of
/// hermit's own); returns the name.
///
/// # Errors
/// The database, `names`, or [`HOST_NAME_TRIES`] names all taken.
pub async fn insert_host(
    t: &aviary_pg::Tx<'_>,
    url: &str,
    ip: IpAddr,
    public_key: &str,
    names: &(dyn Fn() -> Result<String, FabricError> + Sync),
) -> Result<String, FabricError> {
    for _ in 0..HOST_NAME_TRIES {
        let name = names()?;
        let made = t
            .execute(
                "insert into hosts (name, kind, url, wg_ip, wg_public_key, status)
                 values ($1, 'aws', $2, $3, $4, 'booting') on conflict (name) do nothing",
                &[&name, &url, &ip, &public_key],
            )
            .await?;
        if made == 1 {
            return Ok(name);
        }
    }
    Err(FabricError("no free host name".into()))
}

/// The user data of a new host (13.5); at most 16 KiB.
#[must_use]
pub fn user_data(
    cfg: &Config,
    name: &str,
    wg_ip: IpAddr,
    private_key: &str,
    gateway_key: &str,
    s: &crate::fabric::gvisor::HostSecrets,
) -> serde_json::Value {
    let f = &cfg.file.fabric;
    let gw_ip = gateway_ip(&f.gateway.url).map(|i| i.to_string()).unwrap_or_default();
    serde_json::json!({
        "host": {
            "name": name,
            "role": "burst",
            "wgAddress": wg_ip.to_string(),
            "hostCidr": f.host_cidr,
            "controlCidr": f.control_cidr,
            "sandboxCidr": f.sandbox_cidr,
            "egressVip": f.egress_vip.to_string(),
            "egressPorts": f.egress_ports.iter().map(|p| [p.vip, p.target]).collect::<Vec<_>>(),
            "reserveMemory": parse_quantity(&f.aws.fallback_host.reserve).unwrap_or(1 << 30),
            "imageSource": format!("http://{gw_ip}:{}", sandboxd_api::IMAGE_PORT),
            // sandboxd's host.json names the hub peer `hub` (crates/sandboxd/src/config.rs)
            "hub": {
                "publicKey": gateway_key,
                "endpoint": f.gateway.endpoint,
                "allowedIps": [f.host_cidr.clone(), f.control_cidr.clone(), format!("{}/32", f.egress_vip)],
            },
        },
        "wgPrivateKey": private_key,
        "tls": { "ca": s.ca, "cert": s.cert, "key": s.key },
        "juicefs": { "metaUrl": s.juicefs_meta_url, "metaPassword": s.juicefs_meta_password, "rootSquash": "1000:1000" },
    })
}

/// EC2's user data limit.
pub const USER_DATA_MAX: usize = 16 << 10;

#[allow(clippy::many_single_char_names)]
async fn run_instance(
    g: &Gvisor,
    core: &Core,
    p: &Planned,
    gw_key: &str,
    reason: &str,
    transient: bool,
) -> Result<(), FabricError> {
    let (Some(ec2), Some(secrets)) = (&g.ec2, &g.host_secrets) else {
        return Ok(());
    };
    let f = &core.cfg.file.fabric;
    let ud = user_data(&core.cfg, &p.name, p.wg_ip, &p.keys.private_key, gw_key, secrets).to_string();
    let mut tags: Vec<(String, String)> = f.aws.tags.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    tags.push(("Name".into(), format!("hermit-host-{}", p.name)));
    let r = if ud.len() > USER_DATA_MAX {
        Err(ec2::Ec2Error {
            code: "UserDataTooLarge".into(),
            message: format!("{} bytes of user data", ud.len()),
        })
    } else {
        ec2.run_instance(&f.aws.launch_template, &ud, &tags).await
    };
    let c = core.pool.get().await?;
    match r {
        Ok(inst) => {
            c.execute(
                "update hosts set instance_id = $2, private_ip = $3 where name = $1",
                &[&p.name, &inst.id, &inst.private_ip],
            )
            .await?;
            tracing::info!(host = %p.name, instance = %inst.id, reason, "AWS host launched");
            hosts::event(
                &core.pool,
                "launch",
                Some(&p.name),
                None,
                serde_json::json!({"instance": inst.id, "reason": reason, "transient": transient}),
            )
            .await;
        }
        Err(e) => {
            let why = crate::db::clip(&format!("launch failed: {e}"), 500);
            c.execute(
                "update hosts set status = 'gone', gone_at = now(), gone_reason = $2 where name = $1",
                &[&p.name, &why],
            )
            .await?;
            tracing::error!(host = %p.name, code = %e.code, error = %e.message, "AWS launch failed");
            hosts::event(
                &core.pool,
                "launch-failed",
                Some(&p.name),
                None,
                serde_json::json!({"reason": reason, "code": e.code, "error": crate::db::clip(&e.message, 300)}),
            )
            .await;
        }
    }
    Ok(())
}

/// 13.6: an AWS host with no non-gone rows (none migrating to it either), not booting, older than
/// `minHostLifetimeSeconds` -> `TerminateInstances`, row `gone`. Decided under the placement lock, so no placement
/// lands on it meanwhile.
async fn release_empty(g: &Gvisor, core: &Core) -> Result<Option<String>, FabricError> {
    let min_life = ms(secs(core.cfg.file.fabric.min_host_lifetime_seconds));
    let gone = aviary_pg::tx!(&core.pool, |t| -> Result<Option<HostRow>, aviary_pg::Error> {
        t.execute(
            "select pg_advisory_xact_lock($1::int4, $2::int4)",
            &[&LOCK_CLASS, &LOCK_PLACE],
        )
        .await?;
        let Some(r) = t
            .query_opt(
                "select * from hosts h where h.kind = 'aws' and h.status in ('active', 'retiring')
                   and h.created_at < now() - $1::bigint * interval '1 millisecond'
                   and not exists (select 1 from sandboxes s where s.status <> 'gone'
                                   and (s.host = h.name or s.migrating_to = h.name))
                 order by h.created_at limit 1",
                &[&min_life],
            )
            .await?
        else {
            return Ok(None);
        };
        let h = HostRow::from_row(&r)?;
        let reason = if h.status == HostStatus::Active {
            "empty".to_owned()
        } else {
            format!("{}, empty", h.status.as_str())
        };
        t.execute(
            "update hosts set status = 'gone', gone_at = now(), gone_reason = $2 where name = $1",
            &[&h.name, &reason],
        )
        .await?;
        Ok(Some(h))
    })
    .await?;
    let Some(h) = gone else {
        return Ok(None);
    };
    let reason = if h.status == HostStatus::Active {
        "empty".to_owned()
    } else {
        format!("{}, empty", h.status.as_str())
    };
    g.clients.forget(&h.name);
    tracing::info!(host = %h.name, reason = %reason, "AWS host released");
    terminate_instance(g, core, &h, &reason).await;
    Ok(Some(h.name))
}

/// A `ready` row the scaler may move.
#[derive(Clone, Debug)]
struct Movable {
    id: Uuid,
    name: String,
    host: String,
    image: Option<String>,
    memory: u64,
    created_at: OffsetDateTime,
    moved_at: i64,
}

/// `movable(s, from, to)` (13.10): no live claim and no pending release (the query), older than `minAgeSeconds`,
/// the same gVisor release on both hosts, the target has the image or it is the configured one (every host fetches
/// that), and, across kinds or regions only, idle.
fn movable(cfg: &Config, s: &Movable, from: &HostRow, to: &HostRow) -> bool {
    let m = &cfg.file.fabric.migration;
    if age(OffsetDateTime::now_utc(), s.created_at) < secs(m.min_age_seconds) {
        return false;
    }
    if from.gvisor_release() != to.gvisor_release() || from.gvisor_release().is_none() {
        return false;
    }
    let image_ok = s.image.as_deref().is_none_or(|i| {
        bare_digest(i) == bare_digest(&cfg.image_digest) || to.info.as_ref().is_some_and(|x| x.has_image(i))
    });
    if !image_ok {
        return false;
    }
    // the stop of a full scratch's checksummed final delta (13.10). Unknown (a sandboxd that does not report it,
    // which also predates the checksummed delta, or a scratch that is not mounted, whose size nobody knows) is not
    // movable while the bound is on. The figure is the source's last listing (sandboxd reads it at each listing);
    // what the scratch grows during the send's live rounds is not bounded here.
    if m.max_scratch_mib > 0 {
        let scratch = from
            .info
            .as_ref()
            .and_then(|i| i.sandbox(&s.name))
            .and_then(|x| x.scratch_used);
        if scratch.is_none_or(|b| b > u64::from(m.max_scratch_mib) << 20) {
            return false;
        }
    }
    if from.kind != to.kind {
        let usage = from
            .info
            .as_ref()
            .and_then(|i| i.sandbox(&s.name))
            .map_or(1.0, |x| x.cpu_usage);
        if usage > m.idle_cpu {
            return false;
        }
    }
    true
}

/// Steps 11 and 12: one move off a retiring host (longest idle first), else one packing move.
async fn moves(g: &Gvisor, core: &Arc<Core>, sizing: &Sizing) -> Result<Option<(Uuid, String, String)>, FabricError> {
    let f = &core.cfg.file.fabric;
    let dead = secs(f.host_dead_seconds);
    let rules = aviary_pg::tx!(&core.pool, |t| placement::rules_in(t, *sizing, dead).await).await?;
    let slots: Vec<Slot> = aviary_pg::tx!(&core.pool, |t| placement::slots_in(t, &rules).await).await?;
    let hosts = hosts::live(&core.pool).await?;
    let rows: Vec<Movable> = core
        .pool
        .get()
        .await?
        .query(
            "select s.id, s.name, s.host, s.image, s.memory_bytes, s.created_at,
                    (select max(e.at) from host_events e where e.sandbox_id = s.id and e.kind in ('migration', 'migration-adopted'))
             from sandboxes s
             where s.status = 'ready' and s.host is not null and s.migrating_to is null and s.release_pending is null
               and (s.task is null or s.task_until < now())
             order by s.last_active_at",
            &[],
        )
        .await?
        .iter()
        .map(|r| {
            Ok(Movable {
                id: r.try_get(0)?,
                name: r.try_get(1)?,
                host: r.try_get(2)?,
                image: r.try_get(3)?,
                memory: u64::try_from(r.try_get::<_, i64>(4)?).unwrap_or(0),
                created_at: r.try_get(5)?,
                moved_at: r
                    .try_get::<_, Option<OffsetDateTime>>(6)?
                    .map_or(0, |t| i64::try_from(t.unix_timestamp_nanos() / 1_000_000).unwrap_or(0)),
            })
        })
        .collect::<Result<_, aviary_pg::Error>>()?;
    let host = |n: &str| hosts.iter().find(|h| h.name == n);

    // 11. move off retiring hosts
    for from in hosts.iter().filter(|h| h.status == HostStatus::Retiring) {
        for s in rows.iter().filter(|s| s.host == from.name) {
            let Some(to) = hosts::pick(&slots, Some(&from.name), sizing.size(s.memory)) else {
                break;
            };
            if !movable(&core.cfg, s, from, &to.host) {
                continue;
            }
            if start_migration(g, core, s.id, &from.name, &to.host.name, "retiring").await? {
                return Ok(Some((s.id, from.name.clone(), to.host.name.clone())));
            }
        }
    }

    // 12. packing over active, fresh, non-retiring hosts
    let now = OffsetDateTime::now_utc();
    let active: Vec<&HostRow> = hosts
        .iter()
        .filter(|h| h.kind == HostKind::Aws && h.status == HostStatus::Active && h.fresh(now, dead))
        .collect();
    let pack_hosts: Vec<PackHost> = active
        .iter()
        .map(|h| PackHost {
            name: h.name.clone(),
            slots: sizing.host_slots(h),
            open: !sizing.under_pressure(h) && h.jfs_ok() && rules.placeable(h, now),
        })
        .collect();
    let pack: Vec<PackSandbox> = rows
        .iter()
        .filter(|s| active.iter().any(|h| h.name == s.host))
        .map(|s| PackSandbox {
            id: s.id.to_string(),
            host: s.host.clone(),
            size: sizing.size(s.memory),
            moved_at: s.moved_at,
        })
        .collect();
    let plan = plan_packing(&pack_hosts, &pack);
    for m in &plan {
        let (Some(s), Some(from), Some(to)) = (
            rows.iter().find(|r| r.id.to_string() == m.id),
            host(&m.from),
            host(&m.to),
        ) else {
            continue;
        };
        if !movable(&core.cfg, s, from, to) {
            continue;
        }
        hosts::event(
            &core.pool,
            "pack",
            Some(&from.name),
            Some(s.id),
            serde_json::json!({"to": to.name, "moves": plan.len(),
                               "plan": plan.iter().map(|x| format!("{}->{}", x.from, x.to)).collect::<Vec<_>>()}),
        )
        .await;
        if start_migration(g, core, s.id, &from.name, &to.name, "packing").await? {
            return Ok(Some((s.id, from.name.clone(), to.name.clone())));
        }
    }
    Ok(None)
}

/// Starts a live migration (13.10 step 1): in one transaction under the placement lock, takes the row's `migrate`
/// claim (the row still `ready` on `from`, nothing pending), checks the target still has room, and records the
/// intent (`migrating_to`, `migration_started_at`, event `migration-start`); then hands the task to this replica.
///
/// # Errors
/// A database error.
pub async fn start_migration(
    g: &Gvisor,
    core: &Arc<Core>,
    id: Uuid,
    from: &str,
    to: &str,
    reason: &str,
) -> Result<bool, FabricError> {
    if core.is_draining() {
        return Ok(false);
    }
    let sizing = Gvisor::sizing(&core.cfg);
    let dead = secs(core.cfg.file.fabric.host_dead_seconds);
    let (from_s, to_s, reason_s) = (from.to_owned(), to.to_owned(), reason.to_owned());
    let c = core.clone();
    let took = aviary_pg::tx!(&core.pool, |t| -> Result<bool, aviary_pg::Error> {
        t.execute(
            "select pg_advisory_xact_lock($1::int4, $2::int4)",
            &[&LOCK_CLASS, &LOCK_PLACE],
        )
        .await?;
        let Some(r) = t
            .query_opt(
                "select memory_bytes from sandboxes where id = $1 and status = 'ready' and host = $2
                   and migrating_to is null and release_pending is null for update",
                &[&id, &from_s],
            )
            .await?
        else {
            return Ok(false);
        };
        let need = sizing.size(u64::try_from(r.try_get::<_, i64>(0)?).unwrap_or(0));
        let rules = placement::rules_in(t, sizing, dead).await?;
        let room = placement::slots_in(t, &rules)
            .await?
            .iter()
            .any(|s| s.host.name == to_s && s.free >= need);
        if !room || !tasks::take_in(t, &c, id, Kind::Migrate).await? {
            return Ok(false);
        }
        t.execute(
            "update sandboxes set migrating_to = $2, migration_started_at = now() where id = $1",
            &[&id, &to_s],
        )
        .await?;
        t.execute(
            "insert into host_events (kind, host, sandbox_id, data) values ('migration-start', $1, $2, $3)",
            &[
                &from_s,
                &id,
                &serde_json::json!({"from": from_s, "to": to_s, "reason": reason_s}),
            ],
        )
        .await?;
        Ok(true)
    })
    .await?;
    if took {
        g.fresh_migrations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id);
        tasks::run(core, Claim::adopt(core, id, Kind::Migrate));
    }
    Ok(took)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(
            gateway_ip("https://10.99.0.1:7443"),
            Some("10.99.0.1".parse().unwrap_or(IpAddr::from([0, 0, 0, 0])))
        );
        assert_eq!(gateway_ip("https://[fd00::1]:7443/x"), "fd00::1".parse().ok());
        assert_eq!(gateway_ip("https://sandboxd:7443"), None);
        assert_eq!(missing_features("sse4_2, avx2", "fpu,sse4_2"), vec!["avx2".to_owned()]);
        assert_eq!(missing_features("", "fpu"), Vec::<String>::new());
        let r = Report {
            gateway_up: true,
            ..Report::default()
        };
        assert!(r.is_quiet());
    }
}
