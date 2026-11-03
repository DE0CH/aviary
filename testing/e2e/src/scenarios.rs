//! The scenarios of platform 7.8 the driver runs, by number (`e1` …).

use std::time::{Duration, Instant};

use anyhow::{Context as _, bail, ensure};
use reqwest::Method;
use serde_json::{Value, json};

use crate::ws::Ws;
use crate::{ADMIN, ALICE, Harness, JFS, PIGEON, STRANGER, TURN_MODEL, eventually};

/// The scenarios in the order `aviary-e2e` runs them when none is named.
pub const ALL: &[&str] = &[
    "e1", "e2", "e3", "e19", "e16", "e14", "cc", "e8", "e10", "e11", "e18", "e24", "e12", "e17",
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
        "e7" => e7(h).await,
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
    // close at the first row
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
