//! The scenarios of platform 7.8 the driver runs, by number (`e1` …).

use std::time::{Duration, Instant};

use anyhow::{Context as _, bail, ensure};
use reqwest::Method;
use serde_json::{Value, json};

use crate::ws::Ws;

mod chaos;
use crate::{ADMIN, ALICE, Harness, JFS, PIGEON, STRANGER, TURN_MODEL, eventually};

/// The scenarios in the order `aviary-e2e` runs them when none is named.
pub const ALL: &[&str] = &[
    "e1", "e2", "e3", "e4", "e5", "e19", "e16", "e14", "cc", "e6", "e7", "e8", "e10", "e11", "e18", "e9", "e23", "e20",
    "e21", "e22", "e13", "e15", "e24", "e12", "e17", "stale",
];

/// How long a turn may take, sandbox start included (the image is local; the first start makes the host).
const TURN_WAIT: Duration = Duration::from_secs(420);

/// Runs scenario `name`.
pub async fn run(h: &Harness, name: &str) -> anyhow::Result<()> {
    match name {
        "e1" => e1(h).await,
        "e2" => e2(h).await,
        "e3" => e3(h).await,
        "e14" => e14(h).await,
        "e16" => e16(h).await,
        "e19" => e19(h).await,
        "cc" => cc(h).await,
        "e8" => e8(h).await,
        "e10" => e10(h).await,
        "e11" => e11(h).await,
        "e12" => e12(h).await,
        "e17" => e17(h).await,
        "e18" => e18(h).await,
        "e24" => e24(h).await,
        "e6" => e6(h).await,
        "stale" => stale(h).await,
        "e9" => e9(h).await,
        "e13" => e13(h).await,
        "e4" => e4(h).await,
        "e5" => e5(h).await,
        "e23" => e23(h).await,
        "e21" => e21(h).await,
        "e20" => e20(h).await,
        "e22" => e22(h).await,
        "e15" => e15(h).await,
        "e7" => e7(h).await,
        "chaos" => Box::pin(chaos::run(h, 60)).await,
        other if other.starts_with("chaos:") => {
            let minutes: u64 = other["chaos:".len()..].parse().context("chaos:<minutes>")?;
            Box::pin(chaos::run(h, minutes)).await
        }
        other => bail!("no scenario {other}"),
    }
}

fn marker(tag: &str) -> String {
    format!("e2e-{tag}-{}", uuid::Uuid::new_v4().simple())
}

/// A new conversation with `prompt` as its first message; its id.
async fn create(ws: &mut Ws, prompt: &str) -> anyhow::Result<String> {
    let key = uuid::Uuid::new_v4().to_string();
    let (kind, p) = ws
        .rpc(
            "workspace.create.request",
            json!({"idempotencyKey": key, "subscribe": false,
                   "agent": {"config": {"provider": "claude", "cwd": "/workspace"}, "initialPrompt": prompt,
                             "clientMessageId": uuid::Uuid::new_v4().to_string()}}),
        )
        .await?;
    ensure!(kind == "workspace.create.response", "create answered {kind}: {p}");
    ensure!(p["error"].is_null(), "create failed: {}", p["error"]);
    let id = p["agent"]["id"].as_str().context("no agent id")?.to_owned();
    let (_, s) = ws
        .rpc("agent.timeline.set_subscription.request", json!({"agentIds": [id]}))
        .await?;
    ensure!(s["agentIds"][0] == id.as_str(), "subscription: {s}");
    Ok(id)
}

/// Sends `text` to conversation `id`.
async fn send(ws: &mut Ws, id: &str, text: &str) -> anyhow::Result<()> {
    let (_, p) = ws
        .rpc(
            "send_agent_message_request",
            json!({"agentId": id, "text": text, "messageId": uuid::Uuid::new_v4().to_string()}),
        )
        .await?;
    ensure!(p["accepted"] == true, "send refused: {p}");
    Ok(())
}

/// The conversation's snapshot (`fetch_agent_request`).
async fn agent(ws: &mut Ws, id: &str) -> anyhow::Result<Value> {
    let (_, p) = ws.rpc("fetch_agent_request", json!({"agentId": id})).await?;
    Ok(p["agent"].clone())
}

/// Every row of the conversation (`fetch_agent_timeline_request`, tail, 500).
async fn timeline(ws: &mut Ws, id: &str) -> anyhow::Result<Vec<Value>> {
    let (_, p) = ws
        .rpc(
            "fetch_agent_timeline_request",
            json!({"agentId": id, "direction": "tail", "limit": 500}),
        )
        .await?;
    ensure!(p["error"].is_null(), "timeline: {}", p["error"]);
    Ok(p["entries"].as_array().cloned().unwrap_or_default())
}

/// Waits for the turn's end push (`turn_completed`, or fails on `turn_failed`/`turn_canceled`); the pushes of the
/// conversation that came before it.
async fn turn_end(ws: &mut Ws, id: &str) -> anyhow::Result<Vec<Value>> {
    let id2 = id.to_owned();
    let end = ws
        .wait_for("the turn's end", TURN_WAIT, move |v| {
            let p = &v["message"]["payload"];
            v["message"]["type"] == "agent_stream"
                && p["agentId"] == id2.as_str()
                && matches!(
                    p["event"]["type"].as_str(),
                    Some("turn_completed" | "turn_failed" | "turn_canceled")
                )
        })
        .await?;
    let kind = end["message"]["payload"]["event"]["type"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    ensure!(
        kind == "turn_completed",
        "the turn ended {kind}: {}",
        end["message"]["payload"]
    );
    let before: Vec<Value> = ws
        .log
        .iter()
        .filter(|(dir, v)| {
            *dir == "out" && v["message"]["type"] == "agent_stream" && v["message"]["payload"]["agentId"] == id
        })
        .map(|(_, v)| v["message"]["payload"].clone())
        .collect();
    Ok(before)
}

fn texts_of(rows: &[Value], kind: &str) -> Vec<String> {
    rows.iter()
        .filter(|r| r["item"]["type"] == kind)
        .filter_map(|r| r["item"]["text"].as_str().map(str::to_owned))
        .collect()
}

/// E1: sign-in.
async fn e1(h: &Harness) -> anyhow::Result<()> {
    let ok = h.mint(ALICE, 600, None).await?;
    let r = h.pigeon(Method::GET, "/api/jarvis/me", &ok).send().await?;
    let status = r.status().as_u16();
    let body: Value = r.json().await.unwrap_or_default();
    ensure!(status == 200, "me as {ALICE}: {status} {body}");
    ensure!(body["firstName"] == "alice", "me: {body}");
    let stranger = h.mint(STRANGER, 600, None).await?;
    let s = h
        .pigeon(Method::GET, "/api/jarvis/me", &stranger)
        .send()
        .await?
        .status()
        .as_u16();
    ensure!(s == 403, "me as {STRANGER}: {s}");
    let expired = h.mint(ALICE, -60, None).await?;
    let s = h
        .pigeon(Method::GET, "/api/jarvis/me", &expired)
        .send()
        .await?
        .status()
        .as_u16();
    ensure!(s == 401, "an expired token: {s}");
    // the second key: published, then a token it signed is accepted once pigeon refetched the JWKS (unknown kid,
    // at most every 30 s per team)
    h.publish_key("k2", true).await?;
    let rotated = h.mint(ALICE, 600, Some("k2")).await?;
    eventually(
        "a token of the second key",
        Duration::from_secs(45),
        Duration::from_secs(2),
        || async {
            let s = h
                .pigeon(Method::GET, "/api/jarvis/me", &rotated)
                .send()
                .await?
                .status()
                .as_u16();
            Ok((s == 200).then_some(()))
        },
    )
    .await?;
    Ok(())
}

/// E2: a new conversation, one text turn streamed in 40 chunks.
async fn e2(h: &Harness) -> anyhow::Result<()> {
    let token = h.mint(ALICE, 3600, None).await?;
    let (mut ws, _) = Ws::open(PIGEON, &token).await?;
    let m = marker("E2");
    let reply = format!("The answer of {m} comes in many small pieces, forty of them in all, then the turn ends.");
    h.script(json!({"match": {"textContains": m, "model": TURN_MODEL},
                    "reply": [{"text": reply, "chunkCount": 40, "chunkMs": 20}], "times": 1}))
        .await?;
    let r = async {
        let id = create(&mut ws, &format!("Say something long ({m})")).await?;
        let pushes = turn_end(&mut ws, &id).await?;
        let rows = timeline(&mut ws, &id).await?;
        let said = texts_of(&rows, "assistant_message");
        ensure!(said.iter().any(|t| t == &reply), "assistant rows {said:?}");
        // rows arrive whole: one push per row change, row seqs dense (each push's seq the previous plus one)
        let seqs: Vec<u64> = pushes
            .iter()
            .filter(|p| p["event"]["type"] == "timeline")
            .filter_map(|p| p["seq"].as_u64())
            .collect();
        ensure!(!seqs.is_empty(), "no timeline pushes");
        ensure!(
            seqs.windows(2).all(|w| w[1] == w[0] + 1),
            "row seqs not dense: {seqs:?}"
        );
        let live: Vec<String> = pushes
            .iter()
            .filter(|p| p["event"]["type"] == "timeline" && p["event"]["item"]["type"] == "assistant_message")
            .filter_map(|p| p["event"]["item"]["text"].as_str().map(str::to_owned))
            .collect();
        ensure!(
            live.iter().any(|t| t == &reply),
            "the reply was not pushed whole: {live:?}"
        );
        // the title replaced by the title model's answer
        let until = Instant::now() + Duration::from_secs(90);
        loop {
            let a = agent(&mut ws, &id).await?;
            if a["title"] == "測試標題" {
                break;
            }
            ensure!(Instant::now() < until, "the title is still {}", a["title"]);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if r.is_err() {
        eprintln!("{}", ws.tail(40));
    }
    ws.close().await;
    r
}

/// The conversation E3 left (E19 probes its sandbox).
static E3_CONVERSATION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// E3: a tool turn writes /workspace/a.txt; the file downloads byte-equal and is on the `JuiceFS` mount.
async fn e3(h: &Harness) -> anyhow::Result<()> {
    let token = h.mint(ALICE, 3600, None).await?;
    let (mut ws, _) = Ws::open(PIGEON, &token).await?;
    let m = marker("E3");
    h.script(json!({"match": {"textContains": m, "model": TURN_MODEL},
                    "reply": [{"toolUse": {"name": "Bash", "input": {"command": "echo hi > /workspace/a.txt",
                                                                      "description": "write a.txt"}}}],
                    "times": 1}))
        .await?;
    let r = async {
        let id = create(&mut ws, &format!("Write the file ({m})")).await?;
        turn_end(&mut ws, &id).await?;
        let rows = timeline(&mut ws, &id).await?;
        ensure!(
            rows.iter()
                .any(|r| r["item"]["type"] == "tool_call" && r["item"]["status"] == "completed"),
            "no completed tool row: {rows:?}"
        );
        let r = h
            .pigeon(
                Method::GET,
                &format!("/api/jarvis/sessions/{id}/files/raw?path=a.txt"),
                &token,
            )
            .send()
            .await?;
        let status = r.status().as_u16();
        let body = r.bytes().await?;
        ensure!(status == 200 && &body[..] == b"hi\n", "download: {status} {body:?}");
        let on_host = std::fs::read(format!("{JFS}/ws/{id}/a.txt")).context("ws/<id>/a.txt on the JuiceFS mount")?;
        ensure!(on_host == b"hi\n", "the mount's copy: {on_host:?}");
        let _ = E3_CONVERSATION.set(id);
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if r.is_err() {
        eprintln!("{}", ws.tail(40));
    }
    ws.close().await;
    r
}

/// E19: from a sandbox Pod, nothing but the proxy is reachable.
async fn e19(h: &Harness) -> anyhow::Result<()> {
    let id = E3_CONVERSATION
        .get()
        .context("e19 runs after e3 (it probes e3's sandbox)")?;
    let pod = h.sandbox_pod(id).await?.context("e3's sandbox Pod is gone")?;
    let name = pod.metadata.name.clone().unwrap_or_default();
    let magpie = h.pod_ip("magpie").await?;
    let hermit = h.pod_ip("hermit").await?;
    let svc: k8s_openapi::api::core::v1::Service =
        kube::Api::namespaced(h.kube.clone(), crate::NS).get("magpie").await?;
    let magpie_svc = svc.spec.and_then(|s| s.cluster_ip).unwrap_or_default();
    let probes = [
        ("magpie by name", "http://magpie/healthz".to_owned()),
        ("magpie's Service", format!("http://{magpie_svc}/healthz")),
        ("a magpie Pod", format!("http://{magpie}:8080/healthz")),
        ("hermit's port 8080", format!("http://{hermit}:8080/healthz")),
        ("the internet", "https://1.1.1.1/".to_owned()),
    ];
    for (what, url) in probes {
        let (ok, out) = h
            .exec(
                &name,
                &["curl", "-sS", "-o", "/dev/null", "-m", "5", "--noproxy", "*", &url],
            )
            .await?;
        ensure!(!ok, "a sandbox reached {what} ({url}) without the proxy: {out}");
    }
    // the control: the proxy itself answers (403 for a path outside a fixed name's routes)
    let (ok, out) = h
        .exec(
            &name,
            &[
                "sh",
                "-c",
                "curl -sS -o /dev/null -w '%{http_code}' -m 10 -x \"$HTTP_PROXY\" http://magpie/healthz",
            ],
        )
        .await?;
    ensure!(ok && out.trim() == "403", "the proxy from the sandbox: {ok} {out}");
    Ok(())
}

/// E16: the ops page through pigeon (admin only) and hermit's snapshot for the monitor.
async fn e16(h: &Harness) -> anyhow::Result<()> {
    let admin = h.mint(ADMIN, 600, None).await?;
    let s = h.pigeon(Method::GET, "/ops/", &admin).send().await?.status().as_u16();
    ensure!(s == 200, "/ops/ as admin: {s}");
    let alice = h.mint(ALICE, 600, None).await?;
    let s = h.pigeon(Method::GET, "/ops/", &alice).send().await?.status().as_u16();
    ensure!(s == 403, "/ops/ as staff: {s}");
    let r = h.hermit_monitor("/ops/snapshot")?.send().await?;
    let status = r.status().as_u16();
    let v: Value = r.json().await.unwrap_or_default();
    ensure!(status == 200 && v.is_object(), "/ops/snapshot: {status} {v}");
    Ok(())
}

/// E14: delete, restore, delete for good.
async fn e14(h: &Harness) -> anyhow::Result<()> {
    let token = h.mint(ALICE, 3600, None).await?;
    let (mut ws, _) = Ws::open(PIGEON, &token).await?;
    let (mut watcher, _) = Ws::open(PIGEON, &token).await?;
    let (_, _) = watcher
        .rpc(
            "fetch_agents_request",
            json!({"subscribe": {"subscriptionId": "e2e-list"}, "page": {"limit": 200}}),
        )
        .await?;
    let r = async {
        let id = create(&mut ws, &format!("A conversation to delete ({})", marker("E14"))).await?;
        turn_end(&mut ws, &id).await?;
        let (kind, p) = ws.rpc("archive_agent_request", json!({"agentId": id})).await?;
        ensure!(kind == "agent_archived", "delete: {kind} {p}");
        ensure!(!agent(&mut ws, &id).await?["archivedAt"].is_null(), "not deleted");
        let (kind, p) = ws.rpc("refresh_agent_request", json!({"agentId": id})).await?;
        ensure!(
            kind == "status" && p["status"] == "agent_refreshed",
            "restore: {kind} {p}"
        );
        ensure!(agent(&mut ws, &id).await?["archivedAt"].is_null(), "not restored");
        let id2 = id.clone();
        watcher
            .wait_for("a list push on the other socket", Duration::from_secs(30), move |v| {
                matches!(v["message"]["type"].as_str(), Some("agent_update" | "workspace_update"))
                    && v.to_string().contains(&id2)
            })
            .await?;
        let (kind, _) = ws.rpc("archive_agent_request", json!({"agentId": id})).await?;
        ensure!(kind == "agent_archived", "second delete: {kind}");
        let (kind, p) = ws.rpc("delete_agent_request", json!({"agentId": id})).await?;
        ensure!(kind == "agent_deleted", "delete for good: {kind} {p}");
        eventually(
            "magpie forgets it",
            Duration::from_secs(30),
            Duration::from_secs(1),
            || async {
                let r = h.magpie_admin(&format!("/v1/admin/conversations/{id}"))?.send().await?;
                let s = r.status().as_u16();
                if s == 404 {
                    return Ok(Some(()));
                }
                bail!("magpie answers {s}: {}", r.text().await.unwrap_or_default())
            },
        )
        .await?;
        let started = Instant::now();
        eventually(
            "hermit releases the sandbox",
            Duration::from_secs(120),
            Duration::from_secs(2),
            || async { Ok(h.sandbox_pod(&id).await?.is_none().then_some(())) },
        )
        .await?;
        eventually(
            "hermit deletes the workspace",
            Duration::from_secs(180),
            Duration::from_secs(2),
            || async { Ok((!std::path::Path::new(&format!("{JFS}/ws/{id}")).exists()).then_some(())) },
        )
        .await?;
        eprintln!("e14: released and deleted {:?} after the purge", started.elapsed());
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if r.is_err() {
        eprintln!("{}", ws.tail(40));
        eprintln!("watcher:\n{}", watcher.tail(20));
    }
    ws.close().await;
    watcher.close().await;
    r
}

/// Every event of a conversation from magpie's admin route.
async fn events(h: &Harness, id: &str) -> anyhow::Result<Vec<Value>> {
    let mut out = Vec::new();
    let mut after = 0u64;
    loop {
        let r = h
            .magpie_admin(&format!("/v1/admin/conversations/{id}/events?after={after}&limit=500"))?
            .send()
            .await?;
        let status = r.status().as_u16();
        let page: Value = r.json().await?;
        ensure!(status == 200, "events: {status} {page}");
        let evs = page["events"].as_array().cloned().unwrap_or_default();
        if let Some(last) = evs.last().and_then(|e| e["seq"].as_u64()) {
            after = last;
        }
        let more = page["hasNewer"] == true && !evs.is_empty();
        out.extend(evs);
        if !more {
            return Ok(out);
        }
    }
}

/// magpie 20's Claude Code acceptance on the real binary (WP-12's carried item): Claude Code 2.1.304 in the sandbox
/// image, its transcript written through the supervisor's FUSE mount into magpie, person turns (each prompt the echo
/// of its `user.sent`): a foreground subagent with its own tool call, `/compact` (with
/// `CLAUDE_CODE_TRANSCRIPT_LOCAL_GC=1` from hermit.json's `sandbox.env`, the compaction's temp file renamed over the
/// transcript), and a turn after it. Passes when every turn completes, magpie's main transcript holds the compaction,
/// the subagent's transcript and its meta file are in magpie, no two `cc.*` events carry one line `uuid` (a retract's
/// replacement aside), every user or assistant line of the final transcript that is not a prompt echo has its event,
/// and no event comes from the subagent's transcript. The tombstone of 9.1's script is not reproduced by 2.1.304
/// (crates/supervisor/GATE.md, open item 3).
#[allow(clippy::too_many_lines)] // one scenario: its turns, then its checks in the order of the doc
async fn cc(h: &Harness) -> anyhow::Result<()> {
    let token = h.mint(ALICE, 3600, None).await?;
    let (mut ws, _) = Ws::open(PIGEON, &token).await?;
    let m = marker("CC");
    let sub = format!("{m}-subagent");
    h.script(
        json!({"match": {"textContains": format!("{m}-agent"), "model": TURN_MODEL},
                    "reply": [{"toolUse": {"name": "Agent", "input": {
                        "description": "harness subagent", "prompt": format!("Subagent task ({sub})."),
                        "subagent_type": "general-purpose", "run_in_background": false}}}],
                    "times": 1}),
    )
    .await?;
    h.script(json!({"match": {"textContains": sub},
                    "reply": [{"toolUse": {"name": "Bash", "input": {"command": "echo from-subagent",
                                                                      "description": "sub"}}}],
                    "times": 1}))
        .await?;
    let r = async {
        let id = create(&mut ws, &format!("Start a subagent ({m}-agent)")).await?;
        turn_end(&mut ws, &id).await?;
        send(&mut ws, &id, "/compact").await?;
        turn_end_nth(&mut ws, &id, 2).await?;
        send(&mut ws, &id, &format!("After compaction ({m}-after)")).await?;
        turn_end_nth(&mut ws, &id, 3).await?;
        let pod = h.sandbox_pod(&id).await?.context("the sandbox went away")?;
        let pod = pod.metadata.name.clone().unwrap_or_default();
        let dir = "/home/sandbox/.claude/projects/-workspace";
        let (ok, main) = h.exec(&pod, &["cat", &format!("{dir}/{id}.jsonl")]).await?;
        ensure!(ok, "the main transcript: {main}");
        let (ok, agents) = h
            .exec(&pod, &["sh", "-c", &format!("cat {dir}/{id}/subagents/agent-*.jsonl")])
            .await?;
        ensure!(ok && !agents.is_empty(), "the subagent's transcript: {agents}");
        let (ok, meta) = h
            .exec(
                &pod,
                &["sh", "-c", &format!("cat {dir}/{id}/subagents/agent-*.meta.json")],
            )
            .await?;
        ensure!(
            ok && meta.contains("general-purpose"),
            "the subagent's meta file: {meta}"
        );
        let lines: Vec<Value> = main.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        ensure!(
            lines
                .iter()
                .any(|l| l["isCompactSummary"] == true || l["subtype"] == "compact_boundary"),
            "no compaction in the transcript"
        );
        let evs = events(h, &id).await?;
        let cc: Vec<&Value> = evs
            .iter()
            .filter(|e| e["kind"].as_str().is_some_and(|k| k.starts_with("cc.")))
            .collect();
        let mut seen = std::collections::BTreeMap::<String, u64>::new();
        for e in &cc {
            let Some(u) = e["data"]["uuid"].as_str() else { continue };
            if e["meta"]["replaces"].is_null() {
                ensure!(
                    seen.insert(u.to_owned(), e["seq"].as_u64().unwrap_or(0)).is_none(),
                    "a duplicate event of {u}"
                );
            }
        }
        let prompts: Vec<String> = evs
            .iter()
            .filter(|e| e["kind"] == "user.sent")
            .filter_map(|e| e["data"]["text"].as_str().map(str::to_owned))
            .collect();
        // a prompt's echo: its text, or a slash command as Claude Code writes it (`<command-name>/compact</command-name>…`)
        let is_echo = |l: &Value| {
            l["type"] == "user"
                && l["message"]["content"].as_str().is_some_and(|t| {
                    prompts
                        .iter()
                        .any(|p| p == t || t.starts_with(&format!("<command-name>{p}</command-name>")))
                })
        };
        let missing: Vec<String> = lines
            .iter()
            .filter(|l| matches!(l["type"].as_str(), Some("user" | "assistant")))
            .filter(|l| l["isSidechain"] != true && l["isMeta"] != true && !is_echo(l))
            .filter_map(|l| l["uuid"].as_str())
            .filter(|u| !seen.contains_key(*u))
            .map(str::to_owned)
            .collect();
        ensure!(missing.is_empty(), "lines without their event: {missing:?}");
        let sub_uuids: Vec<&str> = agents
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|l| l["uuid"].as_str().map(str::to_owned))
            .filter(|u| seen.contains_key(u))
            .map(|_| "x")
            .collect();
        ensure!(
            sub_uuids.is_empty(),
            "{} events from the subagent's transcript",
            sub_uuids.len()
        );
        eprintln!(
            "cc: {} lines, {} cc events, {} prompts",
            lines.len(),
            cc.len(),
            prompts.len()
        );
        // what claude-code-gate compares between the pinned and the newest Claude Code (nightly.yml): the events
        // magpie derived, by kind (the timing-dependent `sandbox.*` and the title aside), and the rows
        let mut kinds = std::collections::BTreeMap::<String, u64>::new();
        for e in &evs {
            let k = e["kind"].as_str().unwrap_or_default();
            if !k.starts_with("sandbox.") && k != "title.changed" {
                *kinds.entry(k.to_owned()).or_default() += 1;
            }
        }
        let rows = timeline(&mut ws, &id).await?;
        let mut row_kinds = std::collections::BTreeMap::<String, u64>::new();
        for r in &rows {
            *row_kinds
                .entry(r["item"]["type"].as_str().unwrap_or_default().to_owned())
                .or_default() += 1;
        }
        eprintln!("cc-counts {}", json!({"events": kinds, "rows": row_kinds}));
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if r.is_err() {
        eprintln!("{}", ws.tail(40));
    }
    ws.close().await;
    r
}

/// [`turn_end`] for the conversation's `n`th turn end on this socket.
async fn turn_end_nth(ws: &mut Ws, id: &str, n: usize) -> anyhow::Result<()> {
    let ended = |ws: &Ws| {
        ws.log
            .iter()
            .filter(|(dir, v)| {
                *dir == "out"
                    && v["message"]["type"] == "agent_stream"
                    && v["message"]["payload"]["agentId"] == id
                    && matches!(
                        v["message"]["payload"]["event"]["type"].as_str(),
                        Some("turn_completed" | "turn_failed" | "turn_canceled")
                    )
            })
            .count()
    };
    let until = Instant::now() + TURN_WAIT;
    while ended(ws) < n {
        ensure!(Instant::now() < until, "turn {n} did not end");
        turn_end(ws, id).await?;
    }
    Ok(())
}

/// Waits for the conversation's next turn end of kind `kind` (`turn_completed`, `turn_failed`, `turn_canceled`); its
/// payload.
async fn turn_end_kind(ws: &mut Ws, id: &str, within: Duration) -> anyhow::Result<Value> {
    let id2 = id.to_owned();
    let end = ws
        .wait_for("a turn's end", within, move |v| {
            let p = &v["message"]["payload"];
            v["message"]["type"] == "agent_stream"
                && p["agentId"] == id2.as_str()
                && matches!(
                    p["event"]["type"].as_str(),
                    Some("turn_completed" | "turn_failed" | "turn_canceled")
                )
        })
        .await?;
    Ok(end["message"]["payload"]["event"].clone())
}

/// A slow reply: `head`, a pause of `ms`, then `tail`.
fn slow(m: &str, ms: u64, times: u32) -> Value {
    json!({"match": {"textContains": m, "model": TURN_MODEL},
           "reply": [{"text": "Working on it."}, {"sleepMs": ms}, {"text": "Done after the pause."}],
           "times": times})
}

/// Runs `f` on a fresh socket as Alice, printing the socket's last frames when it fails.
async fn as_alice<F, Fut>(h: &Harness, f: F) -> anyhow::Result<()>
where
    F: FnOnce(Ws) -> Fut,
    Fut: std::future::Future<Output = (Ws, anyhow::Result<()>)>,
{
    let token = h.mint(ALICE, 3600, None).await?;
    let (ws, _) = Ws::open(PIGEON, &token).await?;
    let (ws, r) = f(ws).await;
    if r.is_err() {
        eprintln!("{}", ws.tail(40));
    }
    ws.close().await;
    r
}

/// The sandbox Pod of a conversation, waiting for it.
async fn sandbox_of(h: &Harness, id: &str) -> anyhow::Result<String> {
    eventually(
        "the sandbox Pod",
        Duration::from_secs(120),
        Duration::from_secs(1),
        || async { Ok(h.sandbox_pod(id).await?.and_then(|p| p.metadata.name)) },
    )
    .await
}

/// E8: Stop during a 60 s streaming turn: canceled within 30 s, and the next turn works.
async fn e8(h: &Harness) -> anyhow::Result<()> {
    let m = marker("E8");
    h.script(slow(&m, 60_000, 1)).await?;
    as_alice(h, |mut ws| async move {
        let r = async {
            let id = create(&mut ws, &format!("Take a minute ({m})")).await?;
            sandbox_of(h, &id).await?;
            tokio::time::sleep(Duration::from_secs(5)).await;
            let asked = Instant::now();
            let (kind, p) = ws.rpc("cancel_agent_request", json!({"agentId": id})).await?;
            ensure!(
                kind == "cancel_agent_response" && p["error"].is_null(),
                "cancel: {kind} {p}"
            );
            let end = turn_end_kind(&mut ws, &id, Duration::from_secs(30)).await?;
            ensure!(end["type"] == "turn_canceled", "the turn ended {end}");
            eprintln!("e8: canceled {:?} after the stop", asked.elapsed());
            send(&mut ws, &id, "And now a short one").await?;
            let end = turn_end_kind(&mut ws, &id, TURN_WAIT).await?;
            ensure!(end["type"] == "turn_completed", "the next turn ended {end}");
            Ok(())
        }
        .await;
        (ws, r)
    })
    .await
}

/// The main transcript's lines in magpie (read in the sandbox, through the supervisor's mount).
async fn transcript(h: &Harness, pod: &str, id: &str) -> anyhow::Result<Vec<Value>> {
    let (ok, main) = h
        .exec(
            pod,
            &["cat", &format!("/home/sandbox/.claude/projects/-workspace/{id}.jsonl")],
        )
        .await?;
    ensure!(ok, "the transcript: {main}");
    Ok(main.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
}

/// E10: `kill -9` of the supervisor in the sandbox mid-turn: the turn completes, and magpie holds every transcript
/// line exactly once.
async fn e10(h: &Harness) -> anyhow::Result<()> {
    let m = marker("E10");
    h.script(slow(&m, 20_000, 1)).await?;
    as_alice(h, |mut ws| async move {
        let r = async {
            let id = create(&mut ws, &format!("Survive a restart ({m})")).await?;
            let pod = sandbox_of(h, &id).await?;
            tokio::time::sleep(Duration::from_secs(6)).await;
            let (ok, out) = h.exec(&pod, &["sh", "-c", "kill -9 $(pidof supervisor)"]).await?;
            ensure!(ok, "kill -9 the supervisor: {out}");
            let end = turn_end_kind(&mut ws, &id, TURN_WAIT).await?;
            ensure!(end["type"] == "turn_completed", "the turn ended {end}");
            let lines = transcript(h, &pod, &id).await?;
            let mut seen = std::collections::BTreeSet::new();
            for l in &lines {
                if let Some(u) = l["uuid"].as_str() {
                    ensure!(seen.insert(u.to_owned()), "line {u} twice in magpie's transcript");
                }
            }
            let rows = timeline(&mut ws, &id).await?;
            ensure!(
                texts_of(&rows, "assistant_message")
                    .iter()
                    .any(|t| t == "Done after the pause."),
                "the reply's end is not in the timeline"
            );
            Ok(())
        }
        .await;
        (ws, r)
    })
    .await
}

/// E11: the sandbox Pod deleted mid-turn: the turn fails `sandbox.lost` within 40 s; the next turn starts a new
/// sandbox and resumes the history (the model sees the earlier messages).
async fn e11(h: &Harness) -> anyhow::Result<()> {
    let m = marker("E11");
    h.script(slow(&m, 120_000, 1)).await?;
    as_alice(h, |mut ws| async move {
        let r = async {
            let id = create(&mut ws, &format!("Lose the computer ({m})")).await?;
            let pod = sandbox_of(h, &id).await?;
            tokio::time::sleep(Duration::from_secs(5)).await;
            let deleted = Instant::now();
            h.delete_pod(&pod, 0).await?;
            let end = turn_end_kind(&mut ws, &id, Duration::from_secs(40)).await?;
            ensure!(
                end["type"] == "turn_failed" && end["code"] == "sandbox.lost",
                "the turn ended {end} ({:?} after the delete)",
                deleted.elapsed()
            );
            send(&mut ws, &id, &format!("Again on a new computer ({m}-again)")).await?;
            let end = turn_end_kind(&mut ws, &id, TURN_WAIT).await?;
            ensure!(end["type"] == "turn_completed", "the next turn ended {end}");
            let new_pod = sandbox_of(h, &id).await?;
            ensure!(new_pod != pod, "the same sandbox Pod");
            resumed(h, &id).await
        }
        .await;
        (ws, r)
    })
    .await
}

/// The conversation's last turn request to the model carried the earlier messages.
async fn resumed(h: &Harness, id: &str) -> anyhow::Result<()> {
    let reqs = h.model_requests(id).await?;
    let last = reqs
        .iter()
        .rev()
        .find(|r| r["model"] == TURN_MODEL)
        .context("no turn request")?;
    let n = last["messages"].as_u64().unwrap_or(0);
    ensure!(n >= 3, "the model saw {n} messages: the history was not resumed");
    Ok(())
}

/// E12: the sandbox released when idle (2 min); the next turn makes a new one and Claude Code resumes from magpie's
/// `~/.claude` (the model sees the earlier messages).
async fn e12(h: &Harness) -> anyhow::Result<()> {
    as_alice(h, |mut ws| async move {
        let r = async {
            let id = create(&mut ws, &format!("Before the idle release ({})", marker("E12"))).await?;
            let end = turn_end_kind(&mut ws, &id, TURN_WAIT).await?;
            ensure!(end["type"] == "turn_completed", "the first turn ended {end}");
            let pod = sandbox_of(h, &id).await?;
            let waited = Instant::now();
            // the socket keeps reading meanwhile: pigeon's pings want their pongs (pigeon 7.1.3)
            let until = Instant::now() + Duration::from_secs(300);
            while h.sandbox_pod(&id).await?.is_some() {
                ensure!(Instant::now() < until, "{pod} not released within 300 s");
                let _ = ws.drain(Duration::from_secs(3)).await;
            }
            eprintln!("e12: {pod} released {:?} after the turn", waited.elapsed());
            send(&mut ws, &id, "After the release").await?;
            let end = turn_end_kind(&mut ws, &id, TURN_WAIT).await?;
            ensure!(
                end["type"] == "turn_completed",
                "the turn after the release ended {end}"
            );
            resumed(h, &id).await
        }
        .await;
        (ws, r)
    })
    .await
}

/// E17: seven conversations start at once with six slots: six run, the seventh waits (no 500) and runs once a slot
/// frees.
async fn e17(h: &Harness) -> anyhow::Result<()> {
    // start from an empty fabric: the earlier scenarios' sandboxes go when idle
    eventually(
        "no sandbox left",
        Duration::from_secs(300),
        Duration::from_secs(3),
        || async {
            let l = kube::Api::<k8s_openapi::api::core::v1::Pod>::namespaced(h.kube.clone(), crate::NS)
                .list(&kube::api::ListParams::default().labels("app=sandbox"))
                .await?;
            Ok(l.items.is_empty().then_some(()))
        },
    )
    .await?;
    let m = marker("E17");
    h.script(slow(&m, 25_000, 0)).await?;
    as_alice(h, |mut ws| async move {
        let r = async {
            let mut ids = Vec::new();
            for i in 0..7 {
                ids.push(create(&mut ws, &format!("Hold a slot {i} ({m})")).await?);
            }
            let (_, _) = ws
                .rpc("agent.timeline.set_subscription.request", json!({"agentIds": ids}))
                .await?;
            let mut most = 0usize;
            let until = Instant::now() + Duration::from_secs(150);
            let mut done = std::collections::BTreeSet::new();
            while done.len() < ids.len() {
                ensure!(Instant::now() < until, "only {} of 7 turns ended", done.len());
                let l = kube::Api::<k8s_openapi::api::core::v1::Pod>::namespaced(h.kube.clone(), crate::NS)
                    .list(&kube::api::ListParams::default().labels("app=sandbox"))
                    .await?;
                let running = l
                    .items
                    .iter()
                    .filter(|p| p.metadata.deletion_timestamp.is_none())
                    .count();
                most = most.max(running);
                for v in ws.drain(Duration::from_millis(500)).await {
                    let p = &v["message"]["payload"];
                    if v["message"]["type"] == "agent_stream" {
                        let kind = p["event"]["type"].as_str().unwrap_or_default();
                        if kind.starts_with("turn_") && kind != "turn_started" {
                            ensure!(kind == "turn_completed", "a turn ended {}", p["event"]);
                            done.insert(p["agentId"].as_str().unwrap_or_default().to_owned());
                        }
                    }
                }
            }
            ensure!(most <= 6, "{most} sandboxes at once with 6 slots");
            eprintln!("e17: all 7 completed, at most {most} sandboxes at once");
            Ok(())
        }
        .await;
        (ws, r)
    })
    .await
}

/// E18: two pigeon replicas answer alike: the same requests through each Pod give the same results.
async fn e18(h: &Harness) -> anyhow::Result<()> {
    let token = h.mint(ALICE, 3600, None).await?;
    let pods = kube::Api::<k8s_openapi::api::core::v1::Pod>::namespaced(h.kube.clone(), crate::NS)
        .list(&kube::api::ListParams::default().labels("app=pigeon"))
        .await?;
    let ips: Vec<String> = pods
        .items
        .iter()
        .filter_map(|p| p.status.as_ref()?.pod_ip.clone())
        .collect();
    ensure!(ips.len() >= 2, "pigeon Pods: {ips:?}");
    let (mut a, _) = Ws::open(&format!("http://{}:8080", ips[0]), &token).await?;
    let (mut b, _) = Ws::open(&format!("http://{}:8080", ips[1]), &token).await?;
    let r = async {
        let id = create(&mut a, &format!("Two replicas ({})", marker("E18"))).await?;
        let end = turn_end_kind(&mut a, &id, TURN_WAIT).await?;
        ensure!(end["type"] == "turn_completed", "the turn ended {end}");
        let strip = |mut v: Value| {
            if let Some(o) = v.as_object_mut() {
                o.remove("requestId");
                o.remove("subscriptionId");
            }
            v
        };
        for (kind, msg) in [
            ("fetch_agent_request", json!({"agentId": id})),
            (
                "fetch_agent_timeline_request",
                json!({"agentId": id, "direction": "tail", "limit": 50}),
            ),
            ("agent.timeline.list_prompts.request", json!({"agentId": id})),
        ] {
            let (ka, pa) = a.rpc(kind, msg.clone()).await?;
            let (kb, pb) = b.rpc(kind, msg).await?;
            ensure!(
                ka == kb && strip(pa.clone()) == strip(pb.clone()),
                "{kind} differs:\n{pa}\n{pb}"
            );
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if r.is_err() {
        eprintln!("{}\n--\n{}", a.tail(20), b.tail(20));
    }
    a.close().await;
    b.close().await;
    r
}

/// Scales a Deployment.
async fn scale(h: &Harness, name: &str, replicas: i32) -> anyhow::Result<()> {
    let api = kube::Api::<k8s_openapi::api::apps::v1::Deployment>::namespaced(h.kube.clone(), crate::NS);
    let patch = json!({"spec": {"replicas": replicas}});
    api.patch_scale(
        name,
        &kube::api::PatchParams::default(),
        &kube::api::Patch::Merge(&patch),
    )
    .await?;
    Ok(())
}

/// E24: hermit at 0 replicas for 3 minutes during a turn: the turn completes (model, tools and FUSE writes flow
/// through the gateway on its cached liveness), with no 401, 403 or 407 anywhere.
async fn e24(h: &Harness) -> anyhow::Result<()> {
    let m = marker("E24");
    h.script(json!({"match": {"textContains": m, "model": TURN_MODEL},
                    "reply": [{"text": "Before."}, {"sleepMs": 60_000},
                              {"toolUse": {"name": "Bash", "input": {"command": "echo during > /workspace/e24.txt",
                                                                      "description": "write during the outage"}}}],
                    "times": 1}))
        .await?;
    as_alice(h, |mut ws| async move {
        let r = async {
            let id = create(&mut ws, &format!("Outlive hermit ({m})")).await?;
            sandbox_of(h, &id).await?;
            tokio::time::sleep(Duration::from_secs(5)).await;
            scale(h, "hermit", 0).await?;
            let down = Instant::now();
            let end = turn_end_kind(&mut ws, &id, Duration::from_secs(240)).await;
            // hermit comes back after 3 minutes whatever the turn did
            let left = Duration::from_secs(180).saturating_sub(down.elapsed());
            tokio::time::sleep(left).await;
            scale(h, "hermit", 2).await?;
            let end = end?;
            ensure!(
                end["type"] == "turn_completed",
                "the turn ended {end} while hermit was away"
            );
            ensure!(
                std::fs::read(format!("{JFS}/ws/{id}/e24.txt")).ok().as_deref() == Some(b"during\n"),
                "the tool's file is not on the mount"
            );
            Ok(())
        }
        .await;
        (ws, r)
    })
    .await?;
    eventually(
        "hermit back",
        Duration::from_secs(180),
        Duration::from_secs(2),
        || async {
            let d = kube::Api::<k8s_openapi::api::apps::v1::Deployment>::namespaced(h.kube.clone(), crate::NS)
                .get("hermit")
                .await?;
            Ok((d.status.and_then(|s| s.ready_replicas).unwrap_or(0) >= 2).then_some(()))
        },
    )
    .await?;
    // a start right after hermit's return fails `hosts_full` until the hosts' next heartbeat (every host looks dead
    // after an outage longer than hostDeadSeconds): the next scenario waits for that
    tokio::time::sleep(Duration::from_secs(20)).await;
    Ok(())
}

/// One page of `fetch_agent_timeline_request`.
async fn page(ws: &mut Ws, id: &str, direction: &str, cursor: Option<&Value>, limit: u32) -> anyhow::Result<Value> {
    let mut m = json!({"agentId": id, "direction": direction, "limit": limit});
    if let Some(c) = cursor {
        m["cursor"] = c.clone();
    }
    let (_, p) = ws.rpc("fetch_agent_timeline_request", m).await?;
    ensure!(p["error"].is_null(), "timeline: {}", p["error"]);
    Ok(p)
}

/// E6: 120 turns of history, then `tail` and `before` pages of 40: no gap, no duplicate, each page's time within
/// twice the first's.
async fn e6(h: &Harness) -> anyhow::Result<()> {
    as_alice(h, |mut ws| async move {
        let r = async {
            let id = create(&mut ws, &format!("History 0 ({})", marker("E6"))).await?;
            let end = turn_end_kind(&mut ws, &id, TURN_WAIT).await?;
            ensure!(end["type"] == "turn_completed", "turn 0 ended {end}");
            for i in 1..120 {
                send(&mut ws, &id, &format!("History {i}")).await?;
                let end = turn_end_kind(&mut ws, &id, TURN_WAIT).await?;
                ensure!(end["type"] == "turn_completed", "turn {i} ended {end}");
            }
            let mut rows: Vec<Value> = Vec::new();
            let started = Instant::now();
            let mut pg = page(&mut ws, &id, "tail", None, 40).await?;
            let first = started.elapsed();
            let mut times = Vec::new();
            loop {
                let entries = pg["entries"].as_array().cloned().unwrap_or_default();
                ensure!(entries.len() <= 40, "a page of {}", entries.len());
                let mut chunk = entries;
                chunk.extend(rows);
                rows = chunk;
                if pg["hasOlder"] != true {
                    break;
                }
                let cursor = pg["startCursor"].clone();
                let started = Instant::now();
                pg = page(&mut ws, &id, "before", Some(&cursor), 40).await?;
                times.push(started.elapsed());
            }
            // rows in order, each starting after the previous ended: no duplicate, no overlap
            for w in rows.windows(2) {
                let (end, next) = (
                    w[0]["seqEnd"].as_u64().unwrap_or(0),
                    w[1]["seqStart"].as_u64().unwrap_or(0),
                );
                ensure!(next > end, "rows out of order or repeated: {end} then {next}");
            }
            let users = rows.iter().filter(|r| r["item"]["type"] == "user_message").count();
            let replies = rows.iter().filter(|r| r["item"]["type"] == "assistant_message").count();
            ensure!(
                users == 120 && replies >= 120,
                "{users} user rows and {replies} replies in the pages"
            );
            let all = timeline(&mut ws, &id).await?;
            ensure!(
                all.len() == rows.len(),
                "pages hold {} rows, the whole timeline {}",
                rows.len(),
                all.len()
            );
            let slow = times
                .iter()
                .filter(|d| **d > first * 2 + Duration::from_millis(50))
                .count();
            ensure!(
                slow == 0,
                "{slow} pages slower than twice the first ({first:?}): {times:?}"
            );
            eprintln!(
                "e6: {} rows in {} pages, first {first:?}, others {times:?}",
                rows.len(),
                times.len() + 1
            );
            Ok(())
        }
        .await;
        (ws, r)
    })
    .await
}

/// A row's identity in pushes and pages: its message id, else its tool call id.
fn row_key(item: &Value) -> String {
    item["messageId"]
        .as_str()
        .or_else(|| item["callId"].as_str())
        .map_or_else(|| item.to_string(), str::to_owned)
}

/// E7: the socket closed at a frame mid-turn and reopened: the rows after the resume equal the uninterrupted run's.
async fn e7(h: &Harness) -> anyhow::Result<()> {
    let token = h.mint(ALICE, 3600, None).await?;
    let m = marker("E7");
    h.script(json!({"match": {"textContains": m, "model": TURN_MODEL},
                    "reply": [{"text": "The first part."}, {"sleepMs": 3000},
                              {"toolUse": {"name": "Bash", "input": {"command": "sleep 3; echo between",
                                                                      "description": "between"}}}],
                    "times": 1}))
        .await?;
    let (mut ws, _) = Ws::open(PIGEON, &token).await?;
    let id = create(&mut ws, &format!("Reconnect in the middle ({m})")).await?;
    let mut seen: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let note = |seen: &mut std::collections::BTreeMap<String, u64>, v: &Value| {
        let p = &v["message"]["payload"];
        if v["message"]["type"] == "agent_stream" && p["agentId"] == id.as_str() && p["event"]["type"] == "timeline" {
            let seq = p["seq"].as_u64().unwrap_or(0);
            let e = seen.entry(row_key(&p["event"]["item"])).or_insert(0);
            *e = (*e).max(seq);
        }
    };
    // the app's first load: the tail, then the live rows
    let loaded = page(&mut ws, &id, "tail", None, 40).await?;
    for r in loaded["entries"].as_array().into_iter().flatten() {
        let e = seen.entry(row_key(&r["item"])).or_insert(0);
        *e = (*e).max(r["seqEnd"].as_u64().unwrap_or(0));
    }
    // close at the first live row
    let id2 = id.clone();
    let first = ws
        .wait_for("the first row", TURN_WAIT, move |v| {
            v["message"]["type"] == "agent_stream"
                && v["message"]["payload"]["agentId"] == id2.as_str()
                && v["message"]["payload"]["event"]["type"] == "timeline"
        })
        .await?;
    note(&mut seen, &first);
    let cursor = json!({"epoch": first["message"]["payload"]["epoch"], "seq": first["message"]["payload"]["seq"]});
    ws.close().await;
    let (mut ws, _) = Ws::open(PIGEON, &token).await?;
    let r = async {
        let (_, _) = ws
            .rpc("agent.timeline.set_subscription.request", json!({"agentIds": [id]}))
            .await?;
        let catch_up = page(&mut ws, &id, "after", Some(&cursor), 500).await?;
        ensure!(catch_up["reset"] != true, "the resume reset the timeline: {catch_up}");
        for r in catch_up["entries"].as_array().into_iter().flatten() {
            let e = seen.entry(row_key(&r["item"])).or_insert(0);
            *e = (*e).max(r["seqEnd"].as_u64().unwrap_or(0));
        }
        let end = turn_end_kind(&mut ws, &id, TURN_WAIT).await?;
        ensure!(end["type"] == "turn_completed", "the turn ended {end}");
        for (_, v) in ws.log.clone() {
            note(&mut seen, &v);
        }
        let all = timeline(&mut ws, &id).await?;
        for r in &all {
            let k = row_key(&r["item"]);
            let got = seen.get(&k).copied();
            ensure!(
                got == r["seqEnd"].as_u64(),
                "row {k} ended at seq {} but the resumed socket last saw {got:?}",
                r["seqEnd"]
            );
        }
        ensure!(
            seen.len() == all.len(),
            "{} rows seen, the timeline has {}",
            seen.len(),
            all.len()
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if r.is_err() {
        eprintln!("{}", ws.tail(40));
    }
    ws.close().await;
    r
}

/// The stale append of magpie 6.10 end to end (WP-12's carried item), through the real gateway from inside a
/// sandbox: an append, its retry under a new `opId` at the same offset (rule 0: the range already holds those bytes,
/// so it succeeds and writes nothing twice), a different append at that now stale offset (409 `offset_mismatch`, not
/// an overwrite), and the append at the file's size. It takes the conversation's lease from the supervisor, so it
/// runs last on a conversation of its own.
async fn stale(h: &Harness) -> anyhow::Result<()> {
    as_alice(h, |mut ws| async move {
        let r = async {
            let id = create(&mut ws, &format!("A sandbox to write in ({})", marker("STALE"))).await?;
            let end = turn_end_kind(&mut ws, &id, TURN_WAIT).await?;
            ensure!(end["type"] == "turn_completed", "the turn ended {end}");
            let pod = sandbox_of(h, &id).await?;
            let script = r#"
set -u
m=http://magpie/v1/sandbox
H="-H aviary-protocol:magpie-sandbox=1"
lease=$(curl -sS -m 10 -x "$HTTP_PROXY" $H -X POST -d '{}' -H content-type:application/json "$m/lease" | sed -n 's/.*"leaseId":"\([^"]*\)".*/\1/p')
[ -n "$lease" ] || { echo "no lease"; exit 1; }
w() { # w <offset> <opId> <create> <bytes>: the status and the answer
  printf '%s' "$4" | curl -sS -m 10 -x "$HTTP_PROXY" $H -H "aviary-lease: $lease" -H content-type:application/octet-stream \
    --data-binary @- -w ' %{http_code}' \
    "$m/cc/write?path=projects/-workspace/e2e-stale.txt&offset=$1&mtimeNs=1&opId=$2&create=$3&mode=420&append=1"
  echo
}
u() { cat /proc/sys/kernel/random/uuid; }
echo "first $(w 0 "$(u)" 1 'one
')"
echo "retry $(w 0 "$(u)" 0 'one
')"
echo "stale $(w 0 "$(u)" 0 'two
')"
echo "next $(w 4 "$(u)" 0 'two
')"
echo "read $(curl -sS -m 10 -x "$HTTP_PROXY" $H -H "aviary-lease: $lease" "$m/cc/file?path=projects/-workspace/e2e-stale.txt" | base64 -w0)"
"#;
            let (ok, out) = h.exec(&pod, &["sh", "-c", script]).await?;
            eprintln!("stale:\n{out}");
            ensure!(ok, "the script failed: {out}");
            let line = |k: &str| out.lines().find(|l| l.starts_with(k)).unwrap_or_default().to_owned();
            ensure!(line("first").ends_with(" 200") && line("first").contains("\"size\":4"), "the append: {}", line("first"));
            ensure!(
                line("retry").ends_with(" 200") && line("retry").contains("\"size\":4"),
                "the retry under a new opId wrote again: {}",
                line("retry")
            );
            ensure!(
                line("stale").ends_with(" 409") && line("stale").contains("offset_mismatch"),
                "a stale append was not refused: {}",
                line("stale")
            );
            ensure!(line("next").ends_with(" 200") && line("next").contains("\"size\":8"), "the next append: {}", line("next"));
            ensure!(line("read") == "read b25lCnR3bwo=", "magpie's file: {}", line("read"));
            Ok(())
        }
        .await;
        (ws, r)
    })
    .await
}

/// Restarts a Deployment the way `kubectl rollout restart` does and waits until the rollout is done.
async fn rollout(h: &Harness, name: &str) -> anyhow::Result<()> {
    let api = kube::Api::<k8s_openapi::api::apps::v1::Deployment>::namespaced(h.kube.clone(), crate::NS);
    let patch = json!({"spec": {"template": {"metadata": {"annotations": {
        "aviary-e2e/restarted-at": uuid::Uuid::now_v7().to_string()}}}}});
    let d = api
        .patch(
            name,
            &kube::api::PatchParams::default(),
            &kube::api::Patch::Merge(&patch),
        )
        .await?;
    let generation = d.metadata.generation.unwrap_or(0);
    eventually(
        &format!("the rollout of {name}"),
        Duration::from_secs(300),
        Duration::from_secs(1),
        || async {
            let d = api.get(name).await?;
            let want = d.spec.as_ref().and_then(|s| s.replicas).unwrap_or(1);
            let st = d.status.unwrap_or_default();
            let done = st.observed_generation.unwrap_or(0) >= generation
                && st.updated_replicas.unwrap_or(0) == want
                && st.available_replicas.unwrap_or(0) == want
                && st.replicas.unwrap_or(0) == want;
            Ok(done.then_some(()))
        },
    )
    .await
}

/// E9: rollouts of pigeon, magpie, hermit and the gateway, one after the other, during a 90 s turn: the turn
/// completes, the socket (reopened when pigeon's rollout closes it, resuming after the last row it saw) gets every
/// row once, and every request the driver made answered.
async fn e9(h: &Harness) -> anyhow::Result<()> {
    let token = h.mint(ALICE, 3600, None).await?;
    let m = marker("E9");
    h.script(json!({"match": {"textContains": m, "model": TURN_MODEL},
                    "reply": [{"text": "Before the rollouts."}, {"sleepMs": 90_000}, {"text": "After the rollouts."}],
                    "times": 1}))
        .await?;
    let (mut ws, _) = Ws::open(PIGEON, &token).await?;
    let id = create(&mut ws, &format!("Ride out the rollouts ({m})")).await?;
    let mut pushes: Vec<(String, u64)> = Vec::new();
    let mut last = json!(null);
    let loaded = page(&mut ws, &id, "tail", None, 40).await?;
    let mut epoch = loaded["epoch"].clone();
    for r in loaded["entries"].as_array().into_iter().flatten() {
        pushes.push((row_key(&r["item"]), r["seqEnd"].as_u64().unwrap_or(0)));
        last = json!(r["seqEnd"]);
    }
    let mut ended: Option<Value> = None;
    let mut reopened = 0;
    let deployments = ["pigeon", "magpie", "hermit", "test-gateway"];
    let rollouts = {
        let h2 = h;
        async move {
            for d in deployments {
                rollout(h2, d).await?;
            }
            Ok::<_, anyhow::Error>(())
        }
    };
    tokio::pin!(rollouts);
    let mut rolled = false;
    let until = Instant::now() + Duration::from_secs(600);
    while ended.is_none() || !rolled {
        ensure!(
            Instant::now() < until,
            "not done in 600 s (rolled {rolled}, ended {ended:?})"
        );
        tokio::select! {
            r = &mut rollouts, if !rolled => { r?; rolled = true; }
            frames = ws.drain(Duration::from_millis(500)) => {
                for v in frames {
                    let p = &v["message"]["payload"];
                    if v["message"]["type"] != "agent_stream" || p["agentId"] != id.as_str() { continue; }
                    match p["event"]["type"].as_str() {
                        Some("timeline") => {
                            let seq = p["seq"].as_u64().unwrap_or(0);
                            pushes.push((row_key(&p["event"]["item"]), seq));
                            last = json!(seq);
                            epoch = p["epoch"].clone();
                        }
                        Some("turn_completed" | "turn_failed" | "turn_canceled") => ended = Some(p["event"].clone()),
                        _ => {}
                    }
                }
                if ws.closed.is_some() {
                    // pigeon's drain closed the socket: reopen on another replica and resume after the last row
                    reopened += 1;
                    let (w, _) = Ws::open(PIGEON, &token).await?;
                    ws = w;
                    let (_, _) = ws.rpc("agent.timeline.set_subscription.request", json!({"agentIds": [id]})).await?;
                    if !last.is_null() {
                        let c = page(&mut ws, &id, "after", Some(&json!({"epoch": epoch, "seq": last})), 500).await?;
                        ensure!(c["reset"] != true, "the resume reset the timeline");
                        for r in c["entries"].as_array().into_iter().flatten() {
                            let seq = r["seqEnd"].as_u64().unwrap_or(0);
                            pushes.push((row_key(&r["item"]), seq));
                            last = json!(seq);
                        }
                    }
                    let a = agent(&mut ws, &id).await?;
                    if a["status"] == "idle" && ended.is_none() {
                        ended = Some(json!({"type": "turn_completed", "seen": "after a reopen"}));
                    }
                }
            }
        }
    }
    let end = ended.unwrap_or_default();
    ensure!(end["type"] == "turn_completed", "the turn ended {end}");
    let all = timeline(&mut ws, &id).await?;
    let mut counts = std::collections::BTreeMap::<(String, u64), u32>::new();
    for p in &pushes {
        *counts.entry(p.clone()).or_default() += 1;
    }
    let twice: Vec<_> = counts.iter().filter(|(_, n)| **n > 1).collect();
    ensure!(twice.is_empty(), "rows seen twice: {twice:?}");
    for r in &all {
        let k = (row_key(&r["item"]), r["seqEnd"].as_u64().unwrap_or(0));
        ensure!(counts.contains_key(&k), "row {k:?} never seen");
    }
    ensure!(
        texts_of(&all, "assistant_message")
            .iter()
            .any(|t| t == "After the rollouts."),
        "the reply's end is not in the timeline"
    );
    eprintln!("e9: four rollouts during the turn, the socket reopened {reopened} times");
    ws.close().await;
    Ok(())
}

/// Another allowed person (the catalogue allows `@example.org`).
const BOB: &str = "bob@example.org";

/// E13: a share viewed by another person shows the rows up to the share point; continuing it makes a conversation
/// of the viewer's with the shared `/workspace` as it was.
async fn e13(h: &Harness) -> anyhow::Result<()> {
    let alice = h.mint(ALICE, 3600, None).await?;
    let bob = h.mint(BOB, 3600, None).await?;
    let m = marker("E13");
    h.script(json!({"match": {"textContains": m, "model": TURN_MODEL},
                    "reply": [{"toolUse": {"name": "Bash", "input": {"command": "echo shared > /workspace/s.txt",
                                                                      "description": "the shared file"}}}],
                    "times": 1}))
        .await?;
    as_alice(h, |mut ws| async move {
        let r = async {
            let id = create(&mut ws, &format!("Something to share ({m})")).await?;
            let end = turn_end_kind(&mut ws, &id, TURN_WAIT).await?;
            ensure!(end["type"] == "turn_completed", "the turn ended {end}");
            let resp = h
                .pigeon(Method::POST, &format!("/api/jarvis/sessions/{id}/shares"), &alice)
                .send()
                .await?;
            let status = resp.status().as_u16();
            let share: Value = resp.json().await.unwrap_or_default();
            ensure!(status == 201, "share: {status} {share}");
            let share_id = share["id"].as_str().context("no share id")?.to_owned();
            let shared_rows = timeline(&mut ws, &id).await?.len();
            // after the share point: more rows the viewer must not see
            send(&mut ws, &id, "After the share").await?;
            let end = turn_end_kind(&mut ws, &id, TURN_WAIT).await?;
            ensure!(end["type"] == "turn_completed", "the later turn ended {end}");
            let resp = h
                .pigeon(Method::GET, &format!("/api/jarvis/shares/{share_id}"), &bob)
                .send()
                .await?;
            let status = resp.status().as_u16();
            let view: Value = resp.json().await.unwrap_or_default();
            ensure!(
                status == 200 && view["isOwner"] == false,
                "the share as {BOB}: {status} {view}"
            );
            let n = view["timeline"].as_array().map_or(0, Vec::len);
            ensure!(
                n == shared_rows,
                "the share shows {n} rows, {shared_rows} at the share point"
            );
            let resp = h
                .pigeon(Method::POST, &format!("/api/jarvis/shares/{share_id}/continue"), &bob)
                .send()
                .await?;
            let status = resp.status().as_u16();
            let cont: Value = resp.json().await.unwrap_or_default();
            ensure!(status == 201, "continue: {status} {cont}");
            let new_id = cont["sessionId"].as_str().context("no sessionId")?.to_owned();
            let (mut bws, _) = Ws::open(PIGEON, &bob).await?;
            let a = agent(&mut bws, &new_id).await?;
            bws.close().await;
            ensure!(
                a["id"] == new_id.as_str(),
                "{BOB} cannot open the continued conversation: {a}"
            );
            let copy = std::fs::read(format!("{JFS}/ws/{new_id}/s.txt")).context("the frozen /workspace")?;
            ensure!(copy == b"shared\n", "the continued /workspace: {copy:?}");
            Ok(())
        }
        .await;
        (ws, r)
    })
    .await
}

/// E15: view as (pigeon 6): an admin starts a view of Alice, sees her conversations read-only; the link is refused
/// to anyone else.
async fn e15(h: &Harness) -> anyhow::Result<()> {
    let admin = h.mint(ADMIN, 3600, None).await?;
    let alice = h.mint(ALICE, 3600, None).await?;
    // a conversation of Alice's to look at
    let (mut aws, _) = Ws::open(PIGEON, &alice).await?;
    let id = create(&mut aws, &format!("Alice's own ({})", marker("E15"))).await?;
    turn_end_kind(&mut aws, &id, TURN_WAIT).await?;
    aws.close().await;
    let no_redirect = aviary_core::http::client_builder(&aviary_core::version::Build::new("aviary-e2e"), None)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()?;
    let start = |token: &str| {
        no_redirect
            .post(format!("{PIGEON}/api/jarvis/view-as"))
            .header("cf-access-token", token)
            .header("sec-fetch-site", "same-origin")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(format!("user={ALICE}"))
    };
    let code = start(&alice).send().await?.status().as_u16();
    ensure!(code == 403, "a view started by staff: {code}");
    let resp = start(&admin).send().await?;
    let code = resp.status().as_u16();
    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    ensure!(code == 303, "the view's start: {code}");
    let token = location
        .split_once("#jarvis-view-as=")
        .map(|(_, t)| t.to_owned())
        .context("no token")?;
    let query = format!("?jarvis_view_as={token}");
    let resp = h
        .pigeon(Method::GET, &format!("/api/jarvis/me{query}"), &admin)
        .send()
        .await?;
    let me: Value = resp.json().await.unwrap_or_default();
    ensure!(me["viewAs"]["user"] == ALICE, "me in the view: {me}");
    let s = h
        .pigeon(Method::GET, &format!("/api/jarvis/me{query}"), &alice)
        .send()
        .await?
        .status()
        .as_u16();
    ensure!(s == 403, "the link used by someone else: {s}");
    let (mut vws, _) = Ws::open_query(PIGEON, &admin, &query).await?;
    let r = async {
        let (_, list) = vws.rpc("fetch_agents_request", json!({"page": {"limit": 200}})).await?;
        ensure!(
            list.to_string().contains(&id),
            "Alice's conversation is not in the view's list"
        );
        let (kind, p) = vws
            .rpc("update_agent_request", json!({"agentId": id, "title": "renamed"}))
            .await?;
        ensure!(
            kind == "rpc_error" && p["code"] == "view_as_read_only",
            "a rename in the view: {kind} {p}"
        );
        let (kind, p) = vws.rpc("archive_agent_request", json!({"agentId": id})).await?;
        ensure!(
            kind == "rpc_error" && p["code"] == "view_as_read_only",
            "a delete in the view: {kind} {p}"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if r.is_err() {
        eprintln!("{}", vws.tail(20));
    }
    vws.close().await;
    r
}

/// E4: `SendUserFile` of a 5 MiB file: the sent-file snapshot downloads byte-equal after the workspace file is deleted.
async fn e4(h: &Harness) -> anyhow::Result<()> {
    use sha2::Digest as _;
    let m = marker("E4");
    // one assistant message with both calls: Claude Code runs them in order (neither is safe to run alongside)
    h.script(json!({"match": {"textContains": m, "model": TURN_MODEL},
                    "reply": [{"toolUse": {"name": "Bash", "input": {
                                  "command": "head -c 5242880 /dev/urandom > /workspace/big.bin",
                                  "description": "a 5 MiB file"}}},
                              {"toolUse": {"name": "SendUserFile", "input": {
                                  "files": ["/workspace/big.bin"], "caption": "the big file"}}}],
                    "times": 1}))
        .await?;
    let token = h.mint(ALICE, 3600, None).await?;
    as_alice(h, |mut ws| async move {
        let r = async {
            let id = create(&mut ws, &format!("Send me a file ({m})")).await?;
            let end = turn_end_kind(&mut ws, &id, TURN_WAIT).await?;
            ensure!(end["type"] == "turn_completed", "the turn ended {end}");
            let rows = timeline(&mut ws, &id).await?;
            let sent = rows
                .iter()
                .find_map(|r| {
                    r["item"]["metadata"]["sentFiles"]
                        .as_array()
                        .and_then(|a| a.first())
                        .cloned()
                })
                .context("no sentFiles on any tool row")?;
            let call = sent["sent"].as_str().context("no sent id")?.to_owned();
            let path = sent["path"].as_str().context("no path")?.to_owned();
            let original = std::fs::read(format!("{JFS}/ws/{id}/{path}")).context("the file on the mount")?;
            let want = sha2::Sha256::digest(&original);
            let pod = sandbox_of(h, &id).await?;
            let (ok, out) = h.exec(&pod, &["rm", &format!("/workspace/{path}")]).await?;
            ensure!(ok, "rm: {out}");
            let resp = h
                .pigeon(
                    Method::GET,
                    &format!("/api/jarvis/sessions/{id}/files/raw?path={path}&sent={call}"),
                    &token,
                )
                .send()
                .await?;
            let status = resp.status().as_u16();
            let body = resp.bytes().await?;
            ensure!(status == 200, "the sent file: {status}");
            ensure!(
                body.len() == 5 << 20 && sha2::Sha256::digest(&body) == want,
                "the snapshot differs ({} bytes)",
                body.len()
            );
            Ok(())
        }
        .await;
        (ws, r)
    })
    .await
}

/// A 6000×4000 JPEG whose EXIF orientation says "rotate 90°" (6).
fn rotated_jpeg() -> anyhow::Result<Vec<u8>> {
    #[allow(clippy::cast_possible_truncation)] // the low byte of a coordinate, a gradient
    let img = image::RgbImage::from_fn(6000, 4000, |x, y| image::Rgb([(x / 24) as u8, (y / 16) as u8, 128]));
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85).encode_image(&img)?;
    // APP1 Exif after SOI: a big-endian TIFF header and one IFD entry, Orientation (0x0112) SHORT 6
    let tiff: [u8; 26] = [
        b'M', b'M', 0, 42, 0, 0, 0, 8, 0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, 0, 0, 0, 0,
    ];
    let mut app1 = vec![0xff, 0xe1, 0, 34];
    app1.extend_from_slice(b"Exif\0\0");
    app1.extend_from_slice(&tiff);
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&app1);
    out.extend_from_slice(&jpeg[2..]);
    Ok(out)
}

/// E5: an image attachment (6000×4000, EXIF-rotated): the model gets one image with its long edge at most 2000 px
/// and at most 3,932,160 bytes; the timeline's thumbnail loads with its signature and 404s with a bad one.
async fn e5(h: &Harness) -> anyhow::Result<()> {
    use base64::Engine as _;
    let token = h.mint(ALICE, 3600, None).await?;
    let jpeg = rotated_jpeg()?;
    let data = base64::engine::general_purpose::STANDARD.encode(&jpeg);
    as_alice(h, |mut ws| async move {
        let r = async {
            let id = create(&mut ws, &format!("A first message ({})", marker("E5"))).await?;
            turn_end_kind(&mut ws, &id, TURN_WAIT).await?;
            let (_, p) = ws
                .rpc(
                    "send_agent_message_request",
                    json!({"agentId": id, "text": "What is in this picture?",
                           "messageId": uuid::Uuid::new_v4().to_string(),
                           "images": [{"data": data, "mimeType": "image/jpeg"}]}),
                )
                .await?;
            ensure!(p["accepted"] == true, "the send with an image: {p}");
            let end = turn_end_kind(&mut ws, &id, TURN_WAIT).await?;
            ensure!(end["type"] == "turn_completed", "the turn ended {end}");
            let reqs = h.model_requests(&id).await?;
            let images: Vec<&Value> = reqs
                .iter()
                .filter(|r| r["model"] == TURN_MODEL)
                .flat_map(|r| r["images"].as_array().into_iter().flatten())
                .collect();
            let last = images.last().context("the model got no image")?;
            let (w, hgt, bytes) = (
                last["width"].as_u64().unwrap_or(0),
                last["height"].as_u64().unwrap_or(0),
                last["bytes"].as_u64().unwrap_or(u64::MAX),
            );
            ensure!(
                w.max(hgt) <= 2000 && bytes <= 3_932_160,
                "the model's image: {w}×{hgt}, {bytes} bytes"
            );
            ensure!(hgt > w, "the EXIF rotation was not applied: {w}×{hgt}");
            let rows = timeline(&mut ws, &id).await?;
            let thumb = rows
                .iter()
                .filter(|r| r["item"]["type"] == "user_message")
                .find_map(|r| r["item"]["images"][0]["thumbnailUrl"].as_str().map(str::to_owned))
                .context("no thumbnail on the user row")?;
            let ok = h.pigeon(Method::GET, &thumb, &token).send().await?.status().as_u16();
            ensure!(ok == 200, "the thumbnail: {ok}");
            let bad = match thumb.split_once("sig=") {
                Some((head, sig)) => format!(
                    "{head}sig={}{}",
                    if sig.starts_with('A') { "B" } else { "A" },
                    &sig[1..]
                ),
                None => bail!("no sig in {thumb}"),
            };
            let code = h.pigeon(Method::GET, &bad, &token).send().await?.status().as_u16();
            ensure!(code == 404, "the thumbnail with a bad signature: {code}");
            Ok(())
        }
        .await;
        (ws, r)
    })
    .await
}

/// The sum of `drain_cut_total` on a gateway Pod's metrics, or `None` once it no longer answers.
async fn drain_cut(h: &Harness, ip: &str) -> Option<f64> {
    let text = h
        .http
        .get(format!("http://{ip}:9090/metrics"))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    Some(
        text.lines()
            .filter(|l| l.starts_with("drain_cut_total"))
            .filter_map(|l| l.rsplit(' ').next()?.parse::<f64>().ok())
            .sum(),
    )
}

/// E23: both gateway Pods get SIGTERM during a 60 s model stream: the stream completes unbroken, the turn's later
/// calls land on the new Pods, and no request is cut at a drain bound (`drain_cut_total` stays 0 on the draining
/// Pods until they exit).
async fn e23(h: &Harness) -> anyhow::Result<()> {
    let m = marker("E23");
    h.script(slow(&m, 60_000, 1)).await?;
    as_alice(h, |mut ws| async move {
        let r = async {
            let id = create(&mut ws, &format!("Through a gateway drain ({m})")).await?;
            sandbox_of(h, &id).await?;
            tokio::time::sleep(Duration::from_secs(5)).await;
            let pods = kube::Api::<k8s_openapi::api::core::v1::Pod>::namespaced(h.kube.clone(), crate::NS)
                .list(&kube::api::ListParams::default().labels("app=test-gateway"))
                .await?;
            let old: Vec<(String, String)> = pods
                .items
                .iter()
                .filter_map(|p| Some((p.metadata.name.clone()?, p.status.as_ref()?.pod_ip.clone()?)))
                .collect();
            for (name, _) in &old {
                h.delete_pod(name, 620).await?;
            }
            let mut ended = None;
            let mut cut = 0f64;
            let mut scraped = 0u32;
            let until = Instant::now() + Duration::from_secs(300);
            while ended.is_none() {
                ensure!(Instant::now() < until, "the turn did not end");
                for (_, ip) in &old {
                    if let Some(c) = drain_cut(h, ip).await {
                        cut = cut.max(c);
                        scraped += 1;
                    }
                }
                if let Ok(e) = turn_end_kind(&mut ws, &id, Duration::from_secs(2)).await {
                    ended = Some(e);
                }
            }
            let end = ended.unwrap_or_default();
            ensure!(end["type"] == "turn_completed", "the turn ended {end}");
            ensure!(scraped > 0, "no draining gateway's metrics were read");
            ensure!(cut == 0.0, "drain_cut_total {cut} on a draining gateway");
            eprintln!("e23: {scraped} scrapes of the draining gateways, drain_cut_total 0");
            let rows = timeline(&mut ws, &id).await?;
            ensure!(
                texts_of(&rows, "assistant_message")
                    .iter()
                    .any(|t| t == "Done after the pause."),
                "the stream's end is missing"
            );
            Ok(())
        }
        .await;
        (ws, r)
    })
    .await
}

/// E21: the socket closed while Claude Code compacts (the transcript renamed over) and reopened: one catch-up after
/// the last seen row, no reset (the epoch is unchanged: no visible row changed, magpie 7.3 rule 9), and every row of
/// the timeline seen once at its last seq.
async fn e21(h: &Harness) -> anyhow::Result<()> {
    let token = h.mint(ALICE, 3600, None).await?;
    let (mut ws, _) = Ws::open(PIGEON, &token).await?;
    let id = create(&mut ws, &format!("Before a compaction ({})", marker("E21"))).await?;
    let end = turn_end_kind(&mut ws, &id, TURN_WAIT).await?;
    ensure!(end["type"] == "turn_completed", "the first turn ended {end}");
    let before = page(&mut ws, &id, "tail", None, 500).await?;
    let epoch = before["epoch"].clone();
    let mut seen: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut last = 0u64;
    for r in before["entries"].as_array().into_iter().flatten() {
        let seq = r["seqEnd"].as_u64().unwrap_or(0);
        seen.insert(row_key(&r["item"]), seq);
        last = last.max(seq);
    }
    send(&mut ws, &id, "/compact").await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    ws.close().await;
    let (mut ws, _) = Ws::open(PIGEON, &token).await?;
    let r = async {
        let (_, _) = ws
            .rpc("agent.timeline.set_subscription.request", json!({"agentIds": [id]}))
            .await?;
        let caught = page(&mut ws, &id, "after", Some(&json!({"epoch": epoch, "seq": last})), 500).await?;
        ensure!(
            caught["reset"] != true && caught["staleCursor"] != true,
            "the catch-up reset: {caught}"
        );
        ensure!(
            caught["epoch"] == epoch,
            "the epoch changed across the compaction: {} -> {}",
            epoch,
            caught["epoch"]
        );
        for r in caught["entries"].as_array().into_iter().flatten() {
            seen.insert(row_key(&r["item"]), r["seqEnd"].as_u64().unwrap_or(0));
        }
        let snapshot = agent(&mut ws, &id).await?;
        if snapshot["status"] != "idle" {
            let end = turn_end_kind(&mut ws, &id, TURN_WAIT).await?;
            ensure!(end["type"] == "turn_completed", "the compaction ended {end}");
        }
        for (_, v) in ws.log.clone() {
            let p = &v["message"]["payload"];
            if v["message"]["type"] == "agent_stream" && p["agentId"] == id.as_str() && p["event"]["type"] == "timeline"
            {
                ensure!(p["epoch"] == epoch, "a live row of another epoch: {p}");
                let e = seen.entry(row_key(&p["event"]["item"])).or_insert(0);
                *e = (*e).max(p["seq"].as_u64().unwrap_or(0));
            }
        }
        let all = timeline(&mut ws, &id).await?;
        for r in &all {
            let k = row_key(&r["item"]);
            ensure!(
                seen.get(&k).copied() == r["seqEnd"].as_u64(),
                "row {k} not seen at its last seq"
            );
        }
        ensure!(
            seen.len() == all.len(),
            "{} rows seen, {} in the timeline",
            seen.len(),
            all.len()
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if r.is_err() {
        eprintln!("{}", ws.tail(30));
    }
    ws.close().await;
    r
}

/// Waits until Deployment `name` has its replicas ready again.
async fn ready(h: &Harness, name: &str, within: Duration) -> anyhow::Result<()> {
    let api = kube::Api::<k8s_openapi::api::apps::v1::Deployment>::namespaced(h.kube.clone(), crate::NS);
    eventually(&format!("{name} ready"), within, Duration::from_secs(1), || async {
        let d = api.get(name).await?;
        let want = d.spec.as_ref().and_then(|s| s.replicas).unwrap_or(1);
        let st = d.status.unwrap_or_default();
        Ok((st.ready_replicas.unwrap_or(0) >= want && st.replicas.unwrap_or(0) == want).then_some(()))
    })
    .await
}

/// A client's live request on one Pod while it drains: what it saw.
async fn live_client(
    h: &Harness,
    app: &str,
    ip: &str,
    token: &str,
    id: &str,
    deleted: &std::sync::OnceLock<Instant>,
) -> anyhow::Result<String> {
    match app {
        "pigeon" => {
            // a live socket on the draining Pod, subscribed to the running turn: closed 1012 by the drain
            let (mut ws, _) = Ws::open(&format!("http://{ip}:8080"), token).await?;
            let (_, _) = ws
                .rpc("agent.timeline.set_subscription.request", json!({"agentIds": [id]}))
                .await?;
            let until = Instant::now() + Duration::from_secs(120);
            while ws.closed.is_none() {
                ensure!(Instant::now() < until, "the draining pigeon kept the socket open");
                ws.drain(Duration::from_millis(500)).await;
            }
            let code = ws.closed.unwrap_or_default();
            ensure!(code == 1012, "the draining pigeon closed the socket {code}, not 1012");
            Ok(format!("socket closed {code}"))
        }
        "magpie" | "hermit" => {
            // 25 s long-polls on the draining Pod, one after the other (pigeon's change feed on magpie, for a person
            // with no activity; magpie's sandbox feed on hermit): the one in flight at the SIGTERM must answer
            let (path, proto, key) = if app == "magpie" {
                (
                    "/v1/changes?after=latest&waitMs=25000",
                    "magpie-person=1",
                    "E2E_MAGPIE_PIGEON",
                )
            } else {
                (
                    "/v1/sandbox-events?after=latest&waitMs=25000",
                    "hermit-core=1",
                    "E2E_HERMIT_MAGPIE",
                )
            };
            let until = Instant::now() + Duration::from_secs(180);
            loop {
                ensure!(Instant::now() < until, "no long-poll on {app} across the SIGTERM");
                let t = Instant::now();
                let r = h
                    .http
                    .get(format!("http://{ip}:8080{path}"))
                    .header("aviary-protocol", proto)
                    .header("aviary-person", BOB)
                    .bearer_auth(h.token_of(key)?)
                    .timeout(Duration::from_secs(90))
                    .send()
                    .await;
                let across = deleted.get().is_some_and(|d| t < *d);
                match r {
                    Ok(r) => {
                        let status = r.status().as_u16();
                        let body = r.text().await;
                        ensure!(
                            status == 200 && body.is_ok(),
                            "a long-poll on the draining {app} answered {status}: {body:?}"
                        );
                        if across {
                            return Ok(format!(
                                "the long-poll across the SIGTERM answered 200 after {:.1} s",
                                t.elapsed().as_secs_f64()
                            ));
                        }
                    }
                    Err(e) => {
                        ensure!(!across, "the long-poll in flight at the SIGTERM on {app} failed: {e:#}");
                        bail!("{app} stopped answering before any long-poll spanned the SIGTERM: {e:#}");
                    }
                }
            }
        }
        other => bail!("no live client for {other}"),
    }
}

/// SIGTERM to one Pod of `app` with a live client on it: the Pod exits by itself (log `drained`) before its grace,
/// the client's request ends normally, and the same request through the Service then lands on the other replica.
#[allow(clippy::too_many_lines)] // one drain: the Pod, its log, the client, then the checks
async fn drain_one(h: &Harness, app: &str, token: &str, id: &str) -> anyhow::Result<String> {
    use futures_util::{AsyncBufReadExt as _, StreamExt as _};
    ready(h, app, Duration::from_secs(300)).await?;
    let pods = kube::Api::<k8s_openapi::api::core::v1::Pod>::namespaced(h.kube.clone(), crate::NS);
    let list = pods
        .list(&kube::api::ListParams::default().labels(&format!("app={app}")))
        .await?;
    let victim = list
        .items
        .iter()
        .find(|p| {
            p.metadata.deletion_timestamp.is_none() && p.status.as_ref().and_then(|s| s.pod_ip.as_ref()).is_some()
        })
        .with_context(|| format!("no running {app} Pod"))?;
    let name = victim.metadata.name.clone().unwrap_or_default();
    let ip = victim
        .status
        .as_ref()
        .and_then(|s| s.pod_ip.clone())
        .unwrap_or_default();
    let grace = victim
        .spec
        .as_ref()
        .and_then(|s| s.termination_grace_period_seconds)
        .unwrap_or(30);
    // the process's own log, followed until the container ends
    let lp = kube::api::LogParams {
        container: Some(app.to_owned()),
        follow: true,
        since_seconds: Some(1),
        ..Default::default()
    };
    let logs = pods.log_stream(&name, &lp).await?;
    let log_task = tokio::spawn(async move {
        let mut lines = logs.lines();
        let mut out = Vec::new();
        while let Some(Ok(l)) = lines.next().await {
            out.push(l);
        }
        out
    });
    let deleted = std::sync::OnceLock::new();
    let client = live_client(h, app, &ip, token, id, &deleted);
    let signal = async {
        // the client's request is in flight before the signal
        tokio::time::sleep(Duration::from_secs(2)).await;
        let t0 = Instant::now();
        let _ = deleted.set(t0);
        h.delete_pod(&name, u32::try_from(grace).unwrap_or(30)).await?;
        Ok::<_, anyhow::Error>(t0)
    };
    let (seen, t0) = tokio::join!(client, signal);
    let (seen, t0) = (seen?, t0?);
    let gone = eventually(
        &format!("{name} gone"),
        Duration::from_secs(u64::try_from(grace).unwrap_or(30) + 30),
        Duration::from_millis(250),
        || async {
            match pods.get_opt(&name).await? {
                None => Ok(Some(t0.elapsed())),
                Some(_) => Ok(None),
            }
        },
    )
    .await?;
    let log = tokio::time::timeout(Duration::from_secs(10), log_task)
        .await
        .context("the log stream did not end")??;
    ensure!(
        gone < Duration::from_secs(u64::try_from(grace).unwrap_or(30)),
        "{name} took {gone:?}, its grace is {grace} s (killed, not drained)"
    );
    ensure!(
        log.iter().any(|l| l.contains("drained")),
        "{name} exited without its `drained` log; its last lines:\n{}",
        log.iter().rev().take(15).rev().cloned().collect::<Vec<_>>().join("\n")
    );
    // the client resumes on the other replica
    match app {
        "pigeon" => {
            let (mut ws, _) = Ws::open(PIGEON, token).await?;
            let a = agent(&mut ws, id).await?;
            ensure!(a["id"] == id, "the other pigeon's snapshot: {a}");
            ws.close().await;
        }
        "magpie" => {
            let r = h
                .http
                .get(format!("{}/v1/changes?after=latest&waitMs=0", crate::MAGPIE))
                .header("aviary-protocol", "magpie-person=1")
                .header("aviary-person", ALICE)
                .bearer_auth(h.token_of("E2E_MAGPIE_PIGEON")?)
                .send()
                .await?;
            ensure!(
                r.status().is_success(),
                "the change feed on the other magpie: {}",
                r.status()
            );
        }
        _ => {
            let r = h
                .http
                .get(format!("{}/v1/sandbox-events?after=latest&waitMs=0", crate::HERMIT))
                .header("aviary-protocol", "hermit-core=1")
                .bearer_auth(h.token_of("E2E_HERMIT_MAGPIE")?)
                .send()
                .await?;
            ensure!(
                r.status().is_success(),
                "the sandbox feed on the other hermit: {}",
                r.status()
            );
        }
    }
    Ok(format!(
        "{app}: {seen}, the Pod gone after {:.1} s of {grace} s",
        gone.as_secs_f64()
    ))
}

/// E20: SIGTERM to a pigeon, a magpie and a hermit Pod in turn, each with a live client on it (a subscribed socket;
/// a 25 s long-poll), during a 150 s turn: each Pod exits by itself within its grace, each client's request ends
/// normally and its next lands on the other replica, and the turn completes with every row seen once by a socket
/// that reopens whenever pigeon's drain closes it.
#[allow(clippy::too_many_lines)] // one scenario: the turn, the three drains and the socket that rides them out
async fn e20(h: &Harness) -> anyhow::Result<()> {
    let token = h.mint(ALICE, 3600, None).await?;
    let m = marker("E20");
    h.script(json!({"match": {"textContains": m, "model": TURN_MODEL},
                    "reply": [{"text": "Before the drains."}, {"sleepMs": 150_000}, {"text": "After the drains."}],
                    "times": 1}))
        .await?;
    let (mut ws, _) = Ws::open(PIGEON, &token).await?;
    let id = create(&mut ws, &format!("Ride out the drains ({m})")).await?;
    sandbox_of(h, &id).await?;
    let mut pushes: Vec<(String, u64)> = Vec::new();
    let loaded = page(&mut ws, &id, "tail", None, 40).await?;
    let mut epoch = loaded["epoch"].clone();
    let mut last = json!(null);
    for r in loaded["entries"].as_array().into_iter().flatten() {
        pushes.push((row_key(&r["item"]), r["seqEnd"].as_u64().unwrap_or(0)));
        last = json!(r["seqEnd"]);
    }
    let drains = {
        let (token, id) = (token.clone(), id.clone());
        async move {
            let mut said = Vec::new();
            for app in ["pigeon", "magpie", "hermit"] {
                said.push(drain_one(h, app, &token, &id).await?);
            }
            Ok::<_, anyhow::Error>(said)
        }
    };
    tokio::pin!(drains);
    let mut said: Option<Vec<String>> = None;
    let mut ended: Option<Value> = None;
    let until = Instant::now() + Duration::from_mins(15);
    while ended.is_none() || said.is_none() {
        ensure!(
            Instant::now() < until,
            "not done in 15 min (drained {said:?}, ended {ended:?})"
        );
        tokio::select! {
            r = &mut drains, if said.is_none() => { said = Some(r?); }
            frames = ws.drain(Duration::from_millis(500)) => {
                for v in frames {
                    let p = &v["message"]["payload"];
                    if v["message"]["type"] != "agent_stream" || p["agentId"] != id.as_str() { continue; }
                    match p["event"]["type"].as_str() {
                        Some("timeline") => {
                            let seq = p["seq"].as_u64().unwrap_or(0);
                            pushes.push((row_key(&p["event"]["item"]), seq));
                            last = json!(seq);
                            epoch = p["epoch"].clone();
                        }
                        Some("turn_completed" | "turn_failed" | "turn_canceled") => ended = Some(p["event"].clone()),
                        _ => {}
                    }
                }
                if ws.closed.is_some() {
                    let (w, _) = Ws::open(PIGEON, &token).await?;
                    ws = w;
                    let (_, _) = ws.rpc("agent.timeline.set_subscription.request", json!({"agentIds": [id]})).await?;
                    if !last.is_null() {
                        let c = page(&mut ws, &id, "after", Some(&json!({"epoch": epoch, "seq": last})), 500).await?;
                        ensure!(c["reset"] != true, "the resume reset the timeline");
                        for r in c["entries"].as_array().into_iter().flatten() {
                            let seq = r["seqEnd"].as_u64().unwrap_or(0);
                            pushes.push((row_key(&r["item"]), seq));
                            last = json!(seq);
                        }
                    }
                    let a = agent(&mut ws, &id).await?;
                    if a["status"] == "idle" && ended.is_none() {
                        ended = Some(json!({"type": "turn_completed", "seen": "after a reopen"}));
                    }
                }
            }
        }
    }
    let end = ended.unwrap_or_default();
    ensure!(end["type"] == "turn_completed", "the turn ended {end}");
    let all = timeline(&mut ws, &id).await?;
    let mut counts = std::collections::BTreeMap::<(String, u64), u32>::new();
    for p in &pushes {
        *counts.entry(p.clone()).or_default() += 1;
    }
    let twice: Vec<_> = counts.iter().filter(|(_, n)| **n > 1).collect();
    ensure!(twice.is_empty(), "rows seen twice: {twice:?}");
    for r in &all {
        let k = (row_key(&r["item"]), r["seqEnd"].as_u64().unwrap_or(0));
        ensure!(counts.contains_key(&k), "row {k:?} never seen");
    }
    ensure!(
        texts_of(&all, "assistant_message")
            .iter()
            .any(|t| t == "After the drains."),
        "the reply's end is not in the timeline"
    );
    for s in said.unwrap_or_default() {
        eprintln!("e20: {s}");
    }
    ws.close().await;
    for app in ["pigeon", "magpie", "hermit"] {
        ready(h, app, Duration::from_secs(300)).await?;
    }
    Ok(())
}

/// One socket's view of a conversation's rows, classified as the app classifies them (pigeon 7.6: a row seq that is
/// not the next is stale when at most the last, a gap when beyond it; a gap is what makes the app refetch).
#[derive(Debug, Default)]
struct Feed {
    epoch: Value,
    last: u64,
    /// (row, seq) of every live push, counted.
    pushed: std::collections::BTreeMap<(String, u64), u32>,
    /// (row, seq) from the first load and each replacement's tail.
    loaded: std::collections::BTreeSet<(String, u64)>,
    gaps: Vec<String>,
    replacements: Vec<Value>,
    /// Texts of assistant rows ever pushed or loaded.
    said: std::collections::BTreeSet<String>,
}

impl Feed {
    fn load(&mut self, page: &Value) {
        self.epoch = page["epoch"].clone();
        for r in page["entries"].as_array().into_iter().flatten() {
            let seq = r["seqEnd"].as_u64().unwrap_or(0);
            self.loaded.insert((row_key(&r["item"]), seq));
            self.last = self.last.max(seq);
            if let Some(t) = r["item"]["text"].as_str() {
                self.said.insert(t.to_owned());
            }
        }
    }

    /// Takes the socket's frames; a replacement is followed by the app's one tail fetch on that socket.
    async fn take(&mut self, ws: &mut Ws, id: &str, frames: Vec<Value>) -> anyhow::Result<()> {
        for v in frames {
            let p = &v["message"]["payload"];
            if p["agentId"] != id {
                continue;
            }
            match v["message"]["type"].as_str() {
                Some("agent.timeline.replacement") => {
                    self.replacements.push(p["epoch"].clone());
                    let tail = page(ws, id, "tail", None, 500).await?;
                    ensure!(
                        tail["epoch"] == p["epoch"],
                        "the tail after the replacement is in epoch {}, the replacement said {}",
                        tail["epoch"],
                        p["epoch"]
                    );
                    self.last = 0;
                    self.load(&tail);
                }
                Some("agent_stream") if p["event"]["type"] == "timeline" => {
                    let seq = p["seq"].as_u64().unwrap_or(0);
                    let key = row_key(&p["event"]["item"]);
                    if let Some(t) = p["event"]["item"]["text"].as_str() {
                        self.said.insert(t.to_owned());
                    }
                    *self.pushed.entry((key.clone(), seq)).or_default() += 1;
                    if p["epoch"] != self.epoch {
                        self.gaps.push(format!(
                            "row {key} seq {seq} of epoch {} while in {}",
                            p["epoch"], self.epoch
                        ));
                    } else if seq == self.last + 1 {
                        self.last = seq;
                    } else if seq > self.last {
                        self.gaps.push(format!("row {key}: seq {seq} after {}", self.last));
                        self.last = seq;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// E22: one turn whose events interleave `sandbox.*`, `title.changed`, `task.status` (a background Bash) and a
/// `retract` (a tool rewrites the transcript without the turn's first text, renamed over it) between its rows, watched
/// by two sockets: on each, every row arrives once with no false gap (zero refetches), and the retract that removed
/// the visible row arrives as exactly one `agent.timeline.replacement`, after which the app's one tail fetch has the
/// row gone.
#[allow(clippy::too_many_lines)] // one scenario: the turn, the two sockets, then the checks
async fn e22(h: &Harness) -> anyhow::Result<()> {
    let token = h.mint(ALICE, 3600, None).await?;
    let m = marker("E22");
    let gone = "Soon to be retracted.";
    // the command names the line's text only in two halves, so its own tool_use line survives the filter
    let rewrite = format!(
        "sleep 4; d=$HOME/.claude/projects/-workspace; f=$(grep -l '{m}' \"$d\"/*.jsonl | head -n1); \
         p='Soon to be'; p=\"$p retracted.\"; grep -vF \"$p\" \"$f\" > \"$f.e22\" && mv -f \"$f.e22\" \"$f\"; \
         sleep 12; echo rewritten"
    );
    h.script(json!({"match": {"textContains": m, "model": TURN_MODEL},
                    "reply": [{"text": gone},
                              {"toolUse": {"name": "Bash", "input": {"command": "sleep 8; echo background-done",
                                  "description": "a background task", "run_in_background": true}}},
                              {"toolUse": {"name": "Bash", "input": {"command": rewrite,
                                  "description": "rewrite the transcript"}}}],
                    "times": 1}))
        .await?;
    let (mut wa, _) = Ws::open(PIGEON, &token).await?;
    let (mut wb, _) = Ws::open(PIGEON, &token).await?;
    let mut fa = Feed::default();
    let mut fb = Feed::default();
    let res = async {
        let id = create(&mut wa, &format!("Interleaved events ({m})")).await?;
        let (_, _) = wb
            .rpc("agent.timeline.set_subscription.request", json!({"agentIds": [id]}))
            .await?;
        let la = page(&mut wa, &id, "tail", None, 500).await?;
        fa.load(&la);
        let lb = page(&mut wb, &id, "tail", None, 500).await?;
        fb.load(&lb);
        let first_epoch = fa.epoch.clone();
        let is_end = |v: &Value| {
            v["message"]["type"] == "agent_stream"
                && v["message"]["payload"]["agentId"] == id.as_str()
                && matches!(
                    v["message"]["payload"]["event"]["type"].as_str(),
                    Some("turn_completed" | "turn_failed" | "turn_canceled")
                )
        };
        let mut ended: Option<Value> = None;
        let mut quiet_since: Option<Instant> = None;
        let until = Instant::now() + TURN_WAIT + Duration::from_secs(120);
        // until a turn has ended and both sockets were quiet for 8 s (the background task's follow-up, if Claude
        // Code makes one, ends too)
        loop {
            ensure!(Instant::now() < until, "the turn did not end (ended {ended:?})");
            let va = wa.drain(Duration::from_millis(400)).await;
            let vb = wb.drain(Duration::from_millis(400)).await;
            ensure!(
                wa.closed.is_none() && wb.closed.is_none(),
                "a socket closed: {:?} {:?}",
                wa.closed,
                wb.closed
            );
            if let Some(e) = va.iter().rev().find(|v| is_end(v)) {
                ended = Some(e["message"]["payload"]["event"].clone());
            }
            let busy = va
                .iter()
                .chain(vb.iter())
                .any(|v| v["message"]["payload"]["agentId"] == id.as_str());
            fa.take(&mut wa, &id, va).await?;
            fb.take(&mut wb, &id, vb).await?;
            if busy {
                quiet_since = None;
            } else if ended.is_some() {
                let q = *quiet_since.get_or_insert_with(Instant::now);
                if q.elapsed() > Duration::from_secs(8) && agent(&mut wa, &id).await?["status"] == "idle" {
                    break;
                }
            }
        }
        let end = ended.unwrap_or_default();
        ensure!(end["type"] == "turn_completed", "the turn ended {end}");
        // the events interleave between the rows' events
        let evs = events(h, &id).await?;
        let row_seqs: Vec<u64> = evs
            .iter()
            .filter(|e| e["kind"] == "user.sent" || e["kind"].as_str().is_some_and(|k| k.starts_with("cc.")))
            .filter_map(|e| e["seq"].as_u64())
            .collect();
        let (lo, hi) = (
            row_seqs.iter().min().copied().unwrap_or(0),
            row_seqs.iter().max().copied().unwrap_or(0),
        );
        let kinds: Vec<String> = evs
            .iter()
            .map(|e| format!("{}:{}", e["seq"], e["kind"].as_str().unwrap_or("?")))
            .collect();
        // `sandbox.*` comes with a start that waits for its sandbox; a warm one handed over at once makes none
        // (a later run of the scenario on the same harness), so it is required only when the start made one
        let warm = !evs
            .iter()
            .any(|e| e["kind"].as_str().is_some_and(|k| k.starts_with("sandbox.")));
        if warm {
            eprintln!("e22: the sandbox was handed over at once, so the turn has no sandbox.* event");
        }
        for want in ["sandbox.", "title.changed", "task.status", "retract"] {
            if warm && want == "sandbox." {
                continue;
            }
            ensure!(
                evs.iter().any(|e| {
                    let s = e["seq"].as_u64().unwrap_or(0);
                    e["kind"].as_str().is_some_and(|k| k.starts_with(want)) && s > lo && s < hi
                }),
                "no {want} event between the rows' events ({lo}..{hi}): {kinds:?}"
            );
        }
        let all = timeline(&mut wa, &id).await?;
        ensure!(
            !texts_of(&all, "assistant_message").iter().any(|t| t == gone),
            "the retracted text is still a row"
        );
        for (name, f) in [("a", &fa), ("b", &fb)] {
            ensure!(
                f.gaps.is_empty(),
                "socket {name} saw gaps (each a refetch): {:?}",
                f.gaps
            );
            ensure!(
                f.replacements.len() == 1 && f.replacements[0] != first_epoch,
                "socket {name}: replacements {:?} (the first epoch {first_epoch})",
                f.replacements
            );
            ensure!(
                f.said.contains(gone),
                "socket {name} never had the row the retract removed"
            );
            let twice: Vec<_> = f.pushed.iter().filter(|(_, n)| **n > 1).collect();
            ensure!(twice.is_empty(), "socket {name}: rows pushed twice: {twice:?}");
            for r in &all {
                let k = (row_key(&r["item"]), r["seqEnd"].as_u64().unwrap_or(0));
                ensure!(
                    f.pushed.contains_key(&k) || f.loaded.contains(&k),
                    "socket {name} never had row {k:?}"
                );
            }
        }
        eprintln!(
            "e22: {} rows, {} pushes on a and {} on b, one replacement each, events {kinds:?}",
            all.len(),
            fa.pushed.len(),
            fb.pushed.len()
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if res.is_err() {
        eprintln!("{}\n--\n{}", wa.tail(30), wb.tail(30));
    }
    wa.close().await;
    wb.close().await;
    res
}
