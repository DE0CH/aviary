//! `chaos` (platform 6.2 `e2e-chaos`, nightly): two people send turns continuously while a chaos loop rolls pigeon,
//! magpie, hermit and the gateway in random order every 2-5 min and kills a random supervisor every 10 min. Passes
//! when every turn completes (no `error` row, every reply's end in the timeline), every row is seen exactly once by
//! each person's socket (reopened and resumed whenever a drain closes it), and every send is accepted within 180 s (a
//! refusal while a service rolls is sent again under the same message id, and printed). `chaos` runs 60 min; `chaos:<minutes>` sets the
//! length.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail, ensure};
use serde_json::{Value, json};

use super::{TURN_WAIT, agent, create, marker, page, rollout, row_key, timeline};
use crate::ws::Ws;
use crate::{ALICE, Harness, PIGEON, TURN_MODEL};

/// The reply every chaos turn gets.
const DONE: &str = "Chaos turn done.";

/// A random number below `n` (uuid v4's random bits: the driver needs no other source).
fn below(n: u64) -> u64 {
    let b = uuid::Uuid::new_v4().as_u128();
    u64::try_from(b % u128::from(n.max(1))).unwrap_or(0)
}

/// One person's conversation and socket.
struct Rider {
    token: String,
    ws: Ws,
    id: String,
    epoch: Value,
    last: Option<u64>,
    /// (row, seq) of every row version the socket got (pushes, the first load, each resume's catch-up), counted.
    seen: BTreeMap<(String, u64), u32>,
    turns: u32,
    reopened: u32,
    retried: u32,
    refusals: Vec<String>,
}

impl Rider {
    async fn start(h: &Harness, email: &str, tag: &str) -> anyhow::Result<Self> {
        let token = h.mint(email, 4 * 3600, None).await?;
        let (mut ws, _) = Ws::open(PIGEON, &token).await?;
        let id = create(&mut ws, &format!("Chaos ({tag}) first turn")).await?;
        let mut r = Self {
            token,
            ws,
            id,
            epoch: Value::Null,
            last: None,
            seen: BTreeMap::new(),
            turns: 1,
            reopened: 0,
            retried: 0,
            refusals: Vec::new(),
        };
        let loaded = page(&mut r.ws, &r.id, "tail", None, 500).await?;
        r.epoch = loaded["epoch"].clone();
        r.take_entries(&loaded);
        r.wait_end().await?;
        Ok(r)
    }

    fn note(&mut self, key: String, seq: u64) {
        *self.seen.entry((key, seq)).or_default() += 1;
        self.last = Some(self.last.map_or(seq, |l| l.max(seq)));
    }

    fn take_entries(&mut self, page: &Value) {
        for e in page["entries"].as_array().into_iter().flatten() {
            self.note(row_key(&e["item"]), e["seqEnd"].as_u64().unwrap_or(0));
        }
    }

    /// Reopens the socket on whichever pigeon answers and resumes after the last row seen.
    async fn reopen(&mut self) -> anyhow::Result<()> {
        self.reopened += 1;
        let until = Instant::now() + Duration::from_secs(120);
        let ws = loop {
            match Ws::open(PIGEON, &self.token).await {
                Ok((w, _)) => break w,
                Err(e) => {
                    ensure!(Instant::now() < until, "no pigeon took the socket within 120 s: {e:#}");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        };
        self.ws = ws;
        let (_, s) = self
            .ws
            .rpc(
                "agent.timeline.set_subscription.request",
                json!({"agentIds": [self.id]}),
            )
            .await?;
        ensure!(s["agentIds"][0] == self.id.as_str(), "resubscribe: {s}");
        if let Some(last) = self.last {
            let c = page(
                &mut self.ws,
                &self.id,
                "after",
                Some(&json!({"epoch": self.epoch, "seq": last})),
                500,
            )
            .await?;
            ensure!(
                c["reset"] != true && c["staleCursor"] != true,
                "the resume reset the timeline: {c}"
            );
            self.take_entries(&c);
        }
        Ok(())
    }

    /// Reads frames until the running turn ends (its end push, or the conversation idle after a reopen).
    async fn wait_end(&mut self) -> anyhow::Result<()> {
        let until = Instant::now() + TURN_WAIT;
        loop {
            ensure!(Instant::now() < until, "turn {} of {} did not end", self.turns, self.id);
            let frames = self.ws.drain(Duration::from_millis(500)).await;
            let mut ended = None;
            for v in frames {
                let p = &v["message"]["payload"];
                if p["agentId"] != self.id.as_str() {
                    continue;
                }
                match (v["message"]["type"].as_str(), p["event"]["type"].as_str()) {
                    (Some("agent.timeline.replacement"), _) => bail!("a timeline replacement in a chaos run: {p}"),
                    (Some("agent_stream"), Some("timeline")) => {
                        ensure!(
                            p["epoch"] == self.epoch,
                            "a row of epoch {} in {}",
                            p["epoch"],
                            self.epoch
                        );
                        self.note(row_key(&p["event"]["item"]), p["seq"].as_u64().unwrap_or(0));
                    }
                    (Some("agent_stream"), Some(k @ ("turn_completed" | "turn_failed" | "turn_canceled"))) => {
                        ended = Some(k.to_owned());
                    }
                    _ => {}
                }
            }
            if let Some(k) = ended {
                ensure!(k == "turn_completed", "turn {} of {} ended {k}", self.turns, self.id);
                return Ok(());
            }
            if self.ws.closed.is_some() {
                self.reopen().await?;
                let a = agent(&mut self.ws, &self.id).await?;
                if a["status"] == "idle" {
                    // the end push went with the old socket; the timeline says how it ended (checked at the end)
                    return Ok(());
                }
            }
        }
    }

    /// Sends the next turn, retrying `unavailable` (and a socket a drain closed) under the same message id.
    async fn send(&mut self, text: &str) -> anyhow::Result<()> {
        let message_id = uuid::Uuid::new_v4().to_string();
        let until = Instant::now() + Duration::from_secs(180);
        loop {
            ensure!(Instant::now() < until, "the send was not accepted within 180 s");
            if self.ws.closed.is_some() {
                self.reopen().await?;
            }
            let r = self
                .ws
                .rpc(
                    "send_agent_message_request",
                    json!({"agentId": self.id, "text": text, "messageId": message_id}),
                )
                .await;
            match r {
                Ok((_, p)) if p["accepted"] == true => break,
                Ok((_, p)) => {
                    // pigeon answers a refusal as the app's sentence (magpie unreachable during its rollout, or the
                    // last turn's end not yet recorded): sent again under the same message id, so at most once
                    self.retried += 1;
                    self.refusals.push(p["error"].as_str().unwrap_or_default().to_owned());
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                Err(e) => {
                    ensure!(self.ws.closed.is_some(), "the send failed on an open socket: {e:#}");
                }
            }
        }
        self.turns += 1;
        Ok(())
    }

    /// Every row once, every turn completed.
    async fn check(&mut self) -> anyhow::Result<String> {
        if self.ws.closed.is_some() {
            self.reopen().await?;
        }
        let twice: Vec<_> = self
            .seen
            .iter()
            .filter(|(_, n)| **n > 1)
            .map(|(k, n)| (k.clone(), *n))
            .collect();
        ensure!(twice.is_empty(), "{}: rows seen more than once: {twice:?}", self.id);
        let all = timeline(&mut self.ws, &self.id).await?;
        for r in &all {
            let k = (row_key(&r["item"]), r["seqEnd"].as_u64().unwrap_or(0));
            ensure!(self.seen.contains_key(&k), "{}: row {k:?} never seen", self.id);
        }
        let errors: Vec<&Value> = all.iter().filter(|r| r["item"]["type"] == "error").collect();
        ensure!(errors.is_empty(), "{}: failed turns: {errors:?}", self.id);
        let done = all
            .iter()
            .filter(|r| r["item"]["type"] == "assistant_message" && r["item"]["text"] == DONE)
            .count();
        ensure!(
            u32::try_from(done).unwrap_or(0) == self.turns,
            "{}: {} turns sent, {done} replies ended",
            self.id,
            self.turns
        );
        Ok(format!(
            "{}: {} turns, {} rows, socket reopened {} times, {} sends retried ({:?})",
            self.id,
            self.turns,
            all.len(),
            self.reopened,
            self.retried,
            self.refusals
        ))
    }
}

/// Turns, one after the other, until `stop`.
async fn rider_loop(h: &Harness, email: &str, m: &str, stop: Instant) -> anyhow::Result<String> {
    let mut r = Rider::start(h, email, m)
        .await
        .with_context(|| format!("{email}'s first turn"))?;
    while Instant::now() < stop {
        let n = r.turns + 1;
        r.send(&format!("Chaos ({m}) turn {n}"))
            .await
            .with_context(|| format!("{email}: turn {n}"))?;
        r.wait_end().await.with_context(|| format!("{email}: turn {n}"))?;
    }
    let said = r.check().await?;
    r.ws.close().await;
    Ok(said)
}

/// The chaos loop: a rollout every 2-5 min (the four Deployments in a random order, then again), a supervisor
/// killed every 10 min.
async fn chaos_loop(h: &Harness, stop: Instant) -> anyhow::Result<Vec<String>> {
    let mut did = Vec::new();
    let mut order: Vec<&str> = Vec::new();
    let mut next_kill = Instant::now() + Duration::from_mins(10);
    loop {
        let pause = Duration::from_secs(120 + below(181));
        let at = Instant::now() + pause;
        while Instant::now() < at {
            if Instant::now() >= stop {
                return Ok(did);
            }
            if Instant::now() >= next_kill {
                next_kill += Duration::from_mins(10);
                let pods = kube::Api::<k8s_openapi::api::core::v1::Pod>::namespaced(h.kube.clone(), crate::NS)
                    .list(&kube::api::ListParams::default().labels("app=sandbox"))
                    .await?;
                let live: Vec<String> = pods
                    .items
                    .iter()
                    .filter(|p| p.metadata.deletion_timestamp.is_none())
                    .filter_map(|p| p.metadata.name.clone())
                    .collect();
                if !live.is_empty() {
                    let pod = &live[usize::try_from(below(live.len() as u64)).unwrap_or(0)];
                    let (ok, out) = h.exec(pod, &["sh", "-c", "kill -9 $(pidof supervisor)"]).await?;
                    ensure!(ok, "kill -9 the supervisor of {pod}: {out}");
                    did.push(format!("killed the supervisor of {pod}"));
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        if order.is_empty() {
            order = vec!["pigeon", "magpie", "hermit", "test-gateway"];
            for i in (1..order.len()).rev() {
                order.swap(i, usize::try_from(below(i as u64 + 1)).unwrap_or(0));
            }
        }
        let d = order.pop().unwrap_or("pigeon");
        let t = Instant::now();
        rollout(h, d).await?;
        did.push(format!("rolled {d} ({:.0} s)", t.elapsed().as_secs_f64()));
    }
}

/// Runs the chaos scenario for `minutes`.
pub async fn run(h: &Harness, minutes: u64) -> anyhow::Result<()> {
    let m = marker("CHAOS");
    h.script(json!({"match": {"textContains": m, "model": TURN_MODEL},
                    "reply": [{"text": "Chaos turn."}, {"sleepMs": 15_000}, {"text": DONE}],
                    "times": 0}))
        .await?;
    let stop = Instant::now() + Duration::from_mins(minutes);
    let (alice, bob, did) = tokio::try_join!(
        rider_loop(h, ALICE, &m, stop),
        rider_loop(h, super::BOB, &m, stop),
        chaos_loop(h, stop),
    )?;
    for line in did {
        eprintln!("chaos: {line}");
    }
    eprintln!("chaos: {alice}");
    eprintln!("chaos: {bob}");
    Ok(())
}
