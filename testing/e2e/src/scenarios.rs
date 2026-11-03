//! The scenarios of platform 7.8 the driver runs, by number (`e1` …).

use std::time::{Duration, Instant};

use anyhow::{Context as _, bail, ensure};
use reqwest::Method;
use serde_json::{Value, json};

use crate::ws::Ws;
use crate::{ADMIN, ALICE, Harness, JFS, PIGEON, STRANGER, TURN_MODEL, eventually};

/// The scenarios in the order `aviary-e2e` runs them when none is named.
pub const ALL: &[&str] = &["e1", "e2", "e3", "e19", "e16", "e14", "cc"];

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
