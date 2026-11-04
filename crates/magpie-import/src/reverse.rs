//! The reverse hand-over of one conversation (cutover 14.2, DESIGN 9.10): `magpie import reverse <id>…`, for an owned,
//! idle conversation (a rollback after releases):
//! 1. the person's old child runs nothing (a running-set read of their schema: nothing listed, every child answered)
//!    or is not running; else the conversation stays owned and native and the answer is `waiting` (retried later);
//! 2. the reverse fence in magpie ([`magpie::legacy::fence::reverse_fence`]: idle on the new side too, no active turn,
//!    hold, background task, open ask or sandbox lease), renewed every second until the end, so every native write
//!    of it waits in its call;
//! 3. the export (14.3) by the writer sidecar (`export-server`, [`EXPORT_ROUTE`] on the pod-local [`EXPORT_URL`]),
//!    which alone writes the old side;
//! 4. `POST /__jarvis/restart?person=` on every ui pod through the old API's `pods/proxy` (a pod that holds no child of
//!    the person answers `none`): a `busy` lifts the fence with nothing changed (owned, native; `waiting`);
//! 5. the reopen, one transaction: `import_sources` reopened (or created, for a conversation born on the new side) as
//!    synced to what the export wrote (its files, the sessions row's, failed turns' and sent folders' fingerprints as
//!    the next pass reads them, so nothing is imported twice), `released_at` null and verification cleared, then
//!    `migrating` with the fence cleared and change `conversation_upserted` (pigeon's one timeline replacement per
//!    subscribed socket; its next request goes through its legacy upstream, where the next old request starts a fresh
//!    child that reads the exported files).
//!
//! Any failure after the fence lifts it (the conversation stays owned and native; the export's record of an imported
//! conversation, if it wrote one, is what `verify` then checks). A reverse hand-over killed between its fence and its
//! reopen leaves a fence that lapses in 3 s, and the next hand-over pass lifts it
//! ([`crate::handover::recover`]). A person without a schema on the old side yet is refused: the export writes no
//! sessions row for them, and the old stack's disk discovery would index the transcript under an id that is not the
//! conversation's.

use std::time::Duration;

use magpie::legacy::fence::{self, ReverseFence};
use serde::Serialize;
use uuid::Uuid;

use crate::kube::{self, Kube, Restart};
use crate::{Error, Importer, Result};

/// The writer sidecar's pod-local address (the same pod's network namespace; nothing outside the pod reaches it).
pub const EXPORT_URL: &str = "http://127.0.0.1:8081";
/// The writer sidecar's export route.
pub const EXPORT_ROUTE: &str = "/v1/export";
/// The export's bound (a 30 MB transcript is rewritten in at most 20 s, cutover 7.6).
pub const EXPORT_TIMEOUT: Duration = Duration::from_secs(300);
/// One pod's restart call's bound (the old router asks the child's busy route first).
pub const RESTART_TIMEOUT: Duration = Duration::from_secs(15);
/// How often the reverse fence's 3 s lease is renewed while the export and the stop run.
pub const RENEW_EVERY: Duration = Duration::from_secs(1);

/// The export's request (`POST /v1/export`).
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    /// The reverse-fenced conversations to export.
    pub conversations: Vec<Uuid>,
}

/// How one reverse hand-over ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    /// Legacy again (reopened).
    Reversed,
    /// Not now (the old child or the conversation is busy): still owned and native, nothing changed; retried later.
    Waiting,
    /// It failed (said in `why`): still owned and native.
    Failed,
}

/// One conversation's reverse hand-over.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reversed {
    /// The conversation.
    pub conversation: Uuid,
    /// Its person's key (never the email).
    pub person_key: String,
    /// How it ended.
    pub outcome: Outcome,
    /// Why it waits or failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// The ui pods whose restart stopped the person's child.
    pub stopped: Vec<String>,
    /// The export's answer for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub export: Option<serde_json::Value>,
}

/// The reverse hand-over's connections: the old API and the switch token, and the writer sidecar.
#[derive(Debug, Clone)]
pub struct Old<'a> {
    /// The old cluster's API.
    pub kube: &'a Kube,
    /// The switch module's token (`MAGPIE_IMPORT_TOKEN_UI`).
    pub ui_token: &'a str,
    /// The writer sidecar ([`EXPORT_URL`]).
    pub export_url: &'a str,
}

/// `reverse <id>…`, one conversation after the other.
///
/// # Errors
/// The leader lock lost; per-conversation failures are in the answers.
pub async fn reverse(imp: &Importer, old: &Old<'_>, ids: &[Uuid]) -> Result<Vec<Reversed>> {
    let mut out = Vec::new();
    for id in ids {
        imp.check_lock().await?;
        out.push(one(imp, old, *id).await?);
    }
    Ok(out)
}

/// The conversation's person, key and Claude Code session id; whether it is migrating.
async fn conversation(imp: &Importer, id: Uuid) -> Result<Option<(String, String, String, bool)>> {
    Ok(imp
        .magpie
        .pool
        .get()
        .await?
        .query_opt(
            "select c.person, p.person_key, coalesce(c.provider_session_id, c.id::text), c.migrating
             from conversations c join persons p on p.person = c.person where c.id = $1",
            &[&id],
        )
        .await?
        .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3))))
}

/// Step 1: whether the person's old child runs nothing; else why not.
async fn old_child_idle(kube: &Kube, schema: &str) -> Option<String> {
    let reads = match kube::pod_reads(kube).await {
        Ok(r) => r,
        Err(e) => return Some(format!("the ui pods could not be listed: {e}")),
    };
    for (pod, read) in reads {
        let children = match read {
            Ok(c) => c,
            Err(e) => return Some(format!("{}: the running set was not read: {e}", pod.name)),
        };
        for c in children.iter().filter(|c| c.schema == schema) {
            if !c.ok {
                return Some(format!("{}: the person's old child did not answer", pod.name));
            }
            if !c.sessions.is_empty() {
                return Some(format!(
                    "{}: the person's old child runs {} conversation(s)",
                    pod.name,
                    c.sessions.len()
                ));
            }
        }
    }
    None
}

async fn one(imp: &Importer, old: &Old<'_>, id: Uuid) -> Result<Reversed> {
    let Some((person, key, psid, migrating)) = conversation(imp, id).await? else {
        return Ok(failed(id, String::new(), "no such conversation"));
    };
    let mut r = Reversed {
        conversation: id,
        person_key: key.clone(),
        outcome: Outcome::Waiting,
        why: None,
        stopped: Vec::new(),
        export: None,
    };
    if migrating {
        r.outcome = Outcome::Failed;
        r.why = Some("the conversation is legacy already".to_owned());
        return Ok(r);
    }
    let schema = format!("cc_{key}");
    let snap = crate::source::read_schema(&imp.cloudcli, &schema).await?;
    if snap.persons.is_empty() {
        r.outcome = Outcome::Failed;
        r.why = Some("the person has no schema on the old stack (never signed in there)".to_owned());
        return Ok(r);
    }
    if let Some(why) = old_child_idle(old.kube, &schema).await {
        r.why = Some(why);
        return Ok(r);
    }
    match fence::reverse_fence(&imp.magpie.pool, id).await? {
        ReverseFence::Fenced(_) => {}
        ReverseFence::NotOwned => {
            r.outcome = Outcome::Failed;
            r.why = Some("not owned (legacy, or gone)".to_owned());
            return Ok(r);
        }
        ReverseFence::AlreadyFenced => {
            r.why = Some("already fenced".to_owned());
            return Ok(r);
        }
        ReverseFence::Busy(w) => {
            r.why = Some(w.to_owned());
            return Ok(r);
        }
    }
    tracing::info!(conversation = %id, "reverse-fenced");
    // the lease while the export and the stop run (a native write waits for at most magpie's REVERSE_FENCE_WAIT)
    let pool = imp.magpie.pool.clone();
    let renewer = tokio::spawn(async move {
        loop {
            tokio::time::sleep(RENEW_EVERY).await;
            match fence::renew_reverse(&pool, id).await {
                Ok(true) => {}
                Ok(false) => {
                    tracing::warn!(conversation = %id, "the reverse fence lapsed");
                    return;
                }
                Err(e) => tracing::warn!(conversation = %id, error = %e, "the reverse fence was not renewed"),
            }
        }
    });
    let done = fenced(imp, old, id, &person, &key, &schema, &psid, &mut r).await;
    renewer.abort();
    let lift = match &done {
        Ok(true) => false,
        Ok(false) => true,
        Err(e) => {
            r.outcome = Outcome::Failed;
            r.why = Some(e.to_string());
            true
        }
    };
    if lift {
        match fence::unfence_reverse(&imp.magpie.pool, id).await {
            Ok(_) => tracing::info!(conversation = %id, why = ?r.why, "the reverse fence lifted: still owned"),
            Err(e) => tracing::warn!(conversation = %id, error = %e, "the reverse fence was not lifted (it lapses)"),
        }
    }
    if let Err(e @ Error::LockLost) = done {
        return Err(e);
    }
    Ok(r)
}

fn failed(id: Uuid, key: String, why: &str) -> Reversed {
    Reversed {
        conversation: id,
        person_key: key,
        outcome: Outcome::Failed,
        why: Some(why.to_owned()),
        stopped: Vec::new(),
        export: None,
    }
}

/// Steps 3-5 inside the reverse fence: whether the conversation was reopened (`false`: `r` says why not, the caller
/// lifts the fence).
#[allow(clippy::too_many_arguments)]
async fn fenced(
    imp: &Importer,
    old: &Old<'_>,
    id: Uuid,
    person: &str,
    key: &str,
    schema: &str,
    psid: &str,
    r: &mut Reversed,
) -> Result<bool> {
    // 3. the export, by the writer sidecar
    let exported = export(old.export_url, id).await?;
    let mine = exported["conversations"]
        .as_array()
        .and_then(|cs| cs.iter().find(|c| c["conversation"] == id.to_string()))
        .cloned();
    let errors = exported["errors"].as_array().cloned().unwrap_or_default();
    r.export.clone_from(&mine);
    let Some(mine) = mine.filter(|_| errors.is_empty()) else {
        r.outcome = Outcome::Failed;
        r.why = Some(format!("the export failed: {}", serde_json::Value::Array(errors)));
        return Ok(false);
    };
    if mine["row"] != true {
        r.outcome = Outcome::Failed;
        r.why = Some("the export wrote no sessions row".to_owned());
        return Ok(false);
    }
    let recorded: crate::sync::Files = serde_json::from_value(mine["recorded"].clone())?;
    // 4. the old child's stop, on every ui pod (one that holds no child of the person answers `none`)
    let pods = old.kube.ui_pods().await?;
    for p in &pods {
        match kube::restart(old.kube, &p.name, old.ui_token, person, RESTART_TIMEOUT).await? {
            Restart::None => {}
            Restart::Stopped => r.stopped.push(p.name.clone()),
            Restart::Busy(why) => {
                r.outcome = Outcome::Waiting;
                r.why = Some(format!("{}: the old child is busy ({why})", p.name));
                return Ok(false);
            }
        }
    }
    // 5. the reopen, synced to what the export wrote
    let snap = crate::source::read_schema(&imp.cloudcli, schema).await?;
    let Some((ps, row)) = crate::pass::find(&snap, id) else {
        r.outcome = Outcome::Failed;
        r.why = Some("the exported sessions row is not in the person's schema".to_owned());
        return Ok(false);
    };
    let failed_fp = crate::failed::fingerprint(
        &ps.failed
            .get(&id.to_string())
            .map(|f| f.iter().map(|t| t.id).collect())
            .unwrap_or_default(),
    );
    let (_, sent_fp) = crate::sent::read(&imp.jfs, id)?;
    let row_fp = row.fingerprint();
    let files = serde_json::to_value(&recorded)?;
    let reopened =
        aviary_pg::tx!(&imp.magpie.pool, |t| -> Result<bool> {
            imp.check_lock_in(t).await?;
            t.execute(
            "insert into import_sources (conversation_id, person, user_key, source_schema, provider_session, row_fp,
                                         files, sent_fp, failed_turns_fp, first_imported_at, last_synced_at,
                                         last_change_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, now(), now(), now())
             on conflict (conversation_id) do update
               set row_fp = excluded.row_fp, files = import_sources.files || excluded.files,
                   sent_fp = excluded.sent_fp, failed_turns_fp = excluded.failed_turns_fp,
                   last_synced_at = now(), last_change_at = now(), verified_at = null, verified_stats = null,
                   released_at = null, release_reason = null, last_error = null",
            &[&id, &person, &key, &schema, &psid, &row_fp, &files, &sent_fp, &failed_fp],
        )
        .await?;
            if !fence::reopen_in(t, id).await? {
                // the fence lapsed (a native write may have gone through it): nothing is reopened
                return Err(Error::Source(
                    "the reverse fence lapsed before the reopen: run it again".to_owned(),
                ));
            }
            t.execute("update import_control set all_owned_at = null", &[]).await?;
            Ok(true)
        })
        .await?;
    if reopened {
        r.outcome = Outcome::Reversed;
        tracing::info!(conversation = %id, stopped = r.stopped.len(), "reverse hand-over: legacy again");
    }
    Ok(reopened)
}

/// The writer sidecar's export of `id` (its JSON answer, [`crate::export::Exported`]).
async fn export(base: &str, id: Uuid) -> Result<serde_json::Value> {
    let build = aviary_core::version::Build::new("magpie-import");
    let http = aviary_core::http::client_builder(&build, None)
        .timeout(EXPORT_TIMEOUT)
        .build()
        .map_err(|e| Error::Source(format!("the export client: {e}")))?;
    let res = http
        .post(format!("{}{EXPORT_ROUTE}", base.trim_end_matches('/')))
        .json(&ExportRequest {
            conversations: vec![id],
        })
        .send()
        .await
        .map_err(|e| Error::Source(format!("the writer sidecar: {e}")))?;
    let status = res.status();
    let body = res
        .text()
        .await
        .map_err(|e| Error::Source(format!("the writer sidecar: {e}")))?;
    if !status.is_success() {
        return Err(Error::Source(format!("the writer sidecar answered {status}: {body}")));
    }
    Ok(serde_json::from_str(&body)?)
}
