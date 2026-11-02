// pigeon driven by the apps' own code (pigeon spec 18.1): a running pigeon (crates/pigeon/tests/contract_server.rs,
// the real router and socket in front of an in-memory magpie and a local Access team) driven by
//   - each listed client build's own client package (`paseo/packages/client`), every frame it receives and sends
//     checked as received (no normalisation) against every client build in clients.txt;
//   - the app's real timeline reducers (`paseo/packages/app/src/timeline`: processAgentStreamEvent,
//     processTimelineResponse and the fetch plans), fed by the app's own socket driver (DaemonClient's
//     observeTimeline and fetchAgentTimeline, as session-context wires them), whose rendered rows must equal those of
//     an uninterrupted run with no catch-up fetch when non-row events interleave with rows, and with no duplicate or
//     lost row when the socket closes mid-turn across a history rewrite.
//
// The server is `cargo test -p pigeon --test contract_server -- --ignored --exact --nocapture serve`, run from the
// repository root, or the command in $PIGEON_CONTRACT_SERVER (a shell command printing the same `PIGEON_CONTRACT {…}`
// line). PIGEON_CONTRACT_SERVER=none skips this file (a run without Rust); CI runs it.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import path from "node:path";
import { after, before, describe, it } from "node:test";

import { WebSocket } from "ws";

import { appTimelineForCommit, clientForCommit, harnessRoot, newestClient, protocolForCommit, readClients } from "../lib/protocol.mjs";
import { frameType, validateRecords } from "../lib/validate.mjs";

// inside contract.test.mjs's simulation (a copy of the harness outside the repository) there is no pigeon to build
const skip =
  (process.env.PIGEON_CONTRACT_SERVER === "none" && "PIGEON_CONTRACT_SERVER=none") ||
  (process.env.PASEO_CONTRACT_NESTED === "1" && "inside the simulation");
const repoRoot = path.resolve(harnessRoot, "..", "..");

let server;
let pigeon; // { url, control, token }

function startServer() {
  const command = process.env.PIGEON_CONTRACT_SERVER;
  const child = command
    ? spawn("sh", ["-c", command], { cwd: repoRoot, stdio: ["pipe", "pipe", "inherit"] })
    : spawn(
        "cargo",
        ["test", "--locked", "-q", "-p", "pigeon", "--test", "contract_server", "--", "--ignored", "--exact", "--nocapture", "serve"],
        { cwd: repoRoot, stdio: ["pipe", "pipe", "inherit"] },
      );
  return new Promise((resolve, reject) => {
    let buffered = "";
    child.stdout.on("data", (data) => {
      buffered += data;
      const line = buffered.split("\n").find((l) => l.startsWith("PIGEON_CONTRACT "));
      if (line) resolve({ child, info: JSON.parse(line.slice("PIGEON_CONTRACT ".length)) });
    });
    child.on("exit", (code) => reject(new Error(`the contract server exited (${code}) before it was ready`)));
  });
}

async function ctl(op, fields = {}) {
  const response = await fetch(pigeon.control, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ op, ...fields }),
  });
  const body = await response.json();
  if (body.error) throw new Error(body.error);
  return body;
}

const wsUrl = () => `${pigeon.url.replace(/^http/, "ws")}/paseo-ws`;

/**
 * A WebSocket factory for the client package that sends the Access assertion (the browser sends it as a cookie) and
 * records every frame both ways as it is on the wire: `{dir: "out"}` from the server, `{dir: "in"}` from the client.
 */
function recordingFactory(log, sockets = []) {
  return (url, options) => {
    const ws = new WebSocket(url, options?.protocols, {
      headers: { ...(options?.headers ?? {}), "cf-access-jwt-assertion": pigeon.token },
    });
    ws.on("message", (data, isBinary) => {
      if (!isBinary) log.push({ dir: "out", frame: JSON.parse(String(data)), at: Date.now() });
    });
    const send = ws.send.bind(ws);
    ws.send = (data, ...rest) => {
      if (typeof data === "string") log.push({ dir: "in", frame: JSON.parse(data), at: Date.now() });
      return send(data, ...rest);
    };
    sockets.push(ws);
    return ws;
  };
}

async function until(what, check, ms = 10_000) {
  const deadline = Date.now() + ms;
  for (;;) {
    const value = check();
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

const sessionOut = (log, type) => log.filter((r) => r.dir === "out" && r.frame.type === "session" && r.frame.message?.type === type);
const streamEvents = (log, agentId, type) =>
  sessionOut(log, "agent_stream").filter((r) => r.frame.message.payload.agentId === agentId && r.frame.message.payload.event?.type === type);

const assistant = (text, messageId = `msg-${text}`) => ({ kind: "assistant", messageId, blockIndex: 0, text });

/** Every listed client's validators, built once. */
let protocols;
async function everyProtocol() {
  if (!protocols) protocols = await Promise.all(readClients().map((client) => protocolForCommit(client.sha)));
  return protocols;
}

function assertValidForEveryClient(log, all) {
  assert.ok(log.length > 0, "no frames were recorded");
  for (const protocol of all) {
    const { checked, failures } = validateRecords(protocol, log);
    assert.deepEqual(
      failures.map((f) => `${f.dir} ${frameType(f.frame)}: ${f.problem}`),
      [],
      `${protocol.label}: ${failures.length} of ${checked.out} server and ${checked.in} client frames invalid`,
    );
  }
}

describe("pigeon driven by the apps' own code", { skip }, () => {
  before(async () => {
    const started = await startServer();
    server = started.child;
    pigeon = started.info;
  });

  after(() => {
    server?.stdin.end();
    server?.kill();
  });

  it("each newest web and iOS client package drives pigeon; every frame both ways, as sent, is valid for every listed client", async () => {
    const all = await everyProtocol();
    const commits = [...new Set([newestClient("web").sha, newestClient("ios").sha])];
    for (const commit of commits) {
      const { sdk } = await clientForCommit(commit);
      const log = [];
      const client = sdk.createPaseoClient({
        url: wsUrl(),
        webSocketFactory: recordingFactory(log),
        reconnect: { enabled: false },
        appVersion: "0.13.2",
      });
      await client.connect();
      try {
        const { id } = await ctl("conversation", { title: "Quarterly figures" });
        await ctl("row", { id, item: { kind: "user", messageId: "u1", clientMessageId: null, text: "Summarise", images: [], files: [], viewer: null } });
        await ctl("row", { id, item: assistant("Q3 revenue rose 4%.") });
        const agents = await client.agents.list({ subscribe: {} });
        assert.ok(agents.subscriptionId, "the agents list subscription");
        const workspaces = await client.workspaces.list({ subscribe: {} });
        assert.ok(workspaces.subscriptionId, "the workspaces list subscription");
        const handle = client.agents.ref(id);
        await handle.refresh();
        const page = await handle.timeline.refetch({ direction: "tail", limit: 50, projection: "projected" });
        assert.equal(page.entries.length, 2, "the two history rows");
        const events = [];
        const subscription = handle.timeline.subscribe((event) => events.push(event));
        await subscription.ready;
        await handle.send("And in Chinese?");
        const turn = await until("the turn the send started", () => {
          const started = streamEvents(log, id, "turn_started").at(-1);
          return started?.frame.message.payload.event.turnId;
        });
        await ctl("title", { id, title: "Q3 figures" });
        await ctl("sandbox", { id, phase: "preparing" });
        await ctl("row", { id, turn, item: assistant("第三季收入上升 4%。") });
        await ctl("sandbox", { id, phase: "ready" });
        await ctl("turn_end", { id });
        await until("turn_completed", () => streamEvents(log, id, "turn_completed").length > 0);
        await until("the rename's list upsert", () =>
          sessionOut(log, "agent_update").some((r) => r.frame.message.payload.agent?.title === "Q3 figures"),
        );
        assert.ok(events.length > 0, "the client package delivered live timeline events");
        assert.deepEqual(sessionOut(log, "rpc_error").map((r) => r.frame.message.payload), [], "no request failed");
      } finally {
        await client.close();
      }
      assertValidForEveryClient(log, all);
    }
  });

  /** The app's view of one conversation's timeline: the reducers' state, fed by DaemonClient as the app feeds it. */
  class AppTimeline {
    constructor(app, internal, agentId, log) {
      Object.assign(this, { app, internal, agentId, log });
      this.state = { tail: [], head: [], cursor: undefined };
      this.fetches = [];
      this.catchUps = 0;
      this.pending = Promise.resolve();
    }

    async connect() {
      this.sockets = [];
      this.client = new this.internal.DaemonClient({
        url: wsUrl(),
        clientId: `contract-app-${Math.random().toString(36).slice(2)}`,
        clientType: "browser",
        appVersion: "0.13.2",
        webSocketFactory: recordingFactory(this.log, this.sockets),
        reconnect: { enabled: false },
      });
      await this.client.connect();
    }

    /** One fetch, through the app's plan, its response applied by processTimelineResponse. */
    async fetch(plan, initializing = false) {
      this.fetches.push(plan.direction);
      const payload = await this.client.fetchAgentTimeline(this.agentId, plan);
      const out = this.app.reducers.processTimelineResponse({
        payload,
        currentTail: this.state.tail,
        currentHead: this.state.head,
        currentCursor: this.state.cursor,
        isInitializing: initializing,
        hasActiveInitDeferred: false,
        initRequestDirection: plan.direction === "after" ? "after" : "tail",
        sendingClientMessageIds: [],
      });
      if (out.commit === "apply") {
        this.state = { tail: out.tail, head: out.head, cursor: out.cursor === null ? undefined : out.cursor ?? this.state.cursor };
      }
      for (const effect of out.sideEffects) if (effect.type === "catch_up") this.catchUp(effect.cursor);
      return payload;
    }

    catchUp(cursor) {
      this.catchUps += 1;
      this.pending = this.pending.then(() => this.fetch(this.app.plan.planTimelineCatchUpAfter({ epoch: cursor.epoch, seq: cursor.endSeq })));
    }

    /** The live feed, as session-context observes it (agent_stream through processAgentStreamEvent). */
    async observe() {
      this.observation = this.client.observeTimeline([this.agentId]);
      this.observation.subscribe({
        snapshot: () => {},
        update: (message) => {
          if (message.type === "agent_stream" && message.payload.agentId === this.agentId) this.stream(message.payload);
          else if (message.type === "agent.timeline.replacement" && message.payload.agentId === this.agentId) {
            this.pending = this.pending.then(() => this.fetch(this.app.plan.planTimelineTailFetch()));
          }
        },
      });
      await this.observation.ready;
    }

    stream(payload) {
      const out = this.app.reducers.processAgentStreamEvent({
        event: payload.event,
        seq: payload.seq,
        epoch: payload.epoch,
        currentTail: this.state.tail,
        currentHead: this.state.head,
        currentCursor: this.state.cursor,
        timestamp: new Date(payload.timestamp),
      });
      this.state = { tail: out.tail, head: out.head, cursor: out.cursorChanged ? out.cursor ?? undefined : this.state.cursor };
      for (const effect of out.sideEffects) if (effect.type === "catch_up") this.catchUp(effect.cursor);
    }

    async settle() {
      for (let previous; previous !== this.pending; ) {
        previous = this.pending;
        await previous;
      }
    }

    async close() {
      await this.client.close();
    }

    rendered() {
      return render([...this.state.tail, ...this.state.head]);
    }
  }

  /** How often `text` appears in the rendered rows (consecutive assistant blocks may show as one item). */
  function occurrences(shown, text) {
    return shown.join("\n").split(text).length - 1;
  }

  /** What the app shows of each stream item, in order (ids and times aside). */
  function render(items) {
    return items.map((item) => {
      switch (item.kind) {
        case "user_message":
        case "assistant_message":
        case "thought":
          return `${item.kind}: ${item.text}`;
        case "tool_call":
          return `tool_call: ${item.payload?.data?.name ?? ""} ${item.payload?.data?.callId ?? ""} ${item.payload?.data?.status ?? ""}`;
        case "notification":
          return `notification: ${item.message}`;
        default:
          return item.kind;
      }
    });
  }

  /** The rows as a fresh tab (no live history) renders them: one tail fetch. */
  async function freshRender(app, internal, agentId) {
    const fresh = new AppTimeline(app, internal, agentId, []);
    await fresh.connect();
    try {
      await fresh.fetch(app.plan.planTimelineTailFetch(), true);
      return fresh.rendered();
    } finally {
      await fresh.close();
    }
  }

  async function newestApp() {
    const commit = newestClient("web").sha;
    const [app, { internal }] = await Promise.all([appTimelineForCommit(commit), clientForCommit(commit)]);
    assert.equal(typeof internal.DaemonClient, "function", "the client package's DaemonClient");
    return { app, internal };
  }

  it("the app's reducers render a turn whose rows interleave with title, task, sandbox and ask events like an uninterrupted run, with no catch-up fetch", async () => {
    const { app, internal } = await newestApp();
    const log = [];
    const { id } = await ctl("conversation", { title: "Interleaved" });
    await ctl("row", { id, item: assistant("history") });
    const tab = new AppTimeline(app, internal, id, log);
    await tab.connect();
    try {
      // as the app does: the lists subscribed (so the title's list upserts reach this socket too), then the timeline
      await tab.client.fetchAgents({ subscribe: {} });
      await tab.client.fetchWorkspaces({ subscribe: {} });
      await tab.fetch(app.plan.planTimelineTailFetch(), true);
      await tab.observe();
      const { turn } = await ctl("turn_start", { id });
      await ctl("row", { id, turn, item: { kind: "user", messageId: "u-2", clientMessageId: null, text: "go", images: [], files: [], viewer: null } });
      await ctl("title", { id, title: "Interleaved, renamed" });
      await ctl("row", { id, turn, item: assistant("one") });
      await ctl("sandbox", { id, phase: "preparing" });
      const tool = {
        kind: "tool",
        callId: "t-task",
        messageId: "msg-tool",
        blockIndex: 0,
        name: "TodoWrite",
        input: { todos: [{ content: "Check", status: "in_progress", activeForm: "Checking" }] },
        status: "running",
      };
      const toolSeq = (await ctl("row", { id, turn, item: tool })).seq;
      await ctl("ask", {
        id,
        ask: {
          id: "ask-1",
          toolName: "AskUserQuestion",
          toolUseId: "t-ask",
          input: { questions: [{ question: "Which quarter?", header: "Quarter", options: [{ label: "Q3", description: "" }], multiSelect: false }] },
          createdAt: new Date().toISOString(),
        },
      });
      await ctl("update", { id, seqStart: toolSeq, item: { ...tool, status: "completed", result: { text: "ok", bytes: 2, truncated: false, isError: false } } });
      await ctl("sandbox", { id, phase: "ready" });
      await ctl("row", { id, turn, item: assistant("two") });
      await ctl("turn_end", { id });
      await until("turn_completed", () => streamEvents(log, id, "turn_completed").length > 0);
      await tab.settle();
      // the events that take no row seq did come between the rows on this socket
      const firstRow = log.findIndex((r) => r.dir === "out" && r.frame.message?.type === "agent_stream" && r.frame.message.payload.event?.type === "timeline");
      const lastRow = log.findLastIndex((r) => r.dir === "out" && r.frame.message?.type === "agent_stream" && r.frame.message.payload.event?.type === "timeline");
      const between = new Set(log.slice(firstRow, lastRow).filter((r) => r.dir === "out").map((r) => frameType(r.frame)));
      for (const type of ["session/agent_update", "session/workspace_update", "session/agent_permission_request"]) {
        assert.ok(between.has(type), `${type} between the rows: ${[...between].join(", ")}`);
      }
      assert.ok([...between].some((t) => /sandbox_status/.test(t)), `the sandbox chip between the rows: ${[...between].join(", ")}`);
      assert.equal(tab.catchUps, 0, `catch-up fetches: ${tab.fetches.join(", ")}`);
      assert.deepEqual(tab.fetches, ["tail"], "only the opening fetch");
      const shown = tab.rendered();
      assert.deepEqual(shown, await freshRender(app, internal, id));
      assert.equal(occurrences(shown, "one"), 1, shown.join(" | "));
      assert.equal(occurrences(shown, "two"), 1, shown.join(" | "));
    } finally {
      await tab.close();
    }
    assertValidForEveryClient(log, await everyProtocol());
  });

  it("the app's reducers keep every row exactly once when the socket closes mid-turn across a history rewrite", async () => {
    const { app, internal } = await newestApp();
    const log = [];
    const { id } = await ctl("conversation", { title: "Rewritten" });
    await ctl("row", { id, item: assistant("«before»", "msg-before") });
    const tab = new AppTimeline(app, internal, id, log);
    await tab.connect();
    await tab.fetch(app.plan.planTimelineTailFetch(), true);
    await tab.observe();
    // a rewrite while connected: one replacement, the tail fetched again in the new epoch
    const { turn } = await ctl("turn_start", { id });
    await ctl("row", { id, turn, item: assistant("«kept»", "msg-kept") });
    await ctl("row", { id, turn, item: assistant("«retracted-1»", "msg-retracted-1") });
    await until("the live rows", () => streamEvents(log, id, "timeline").length >= 2);
    await ctl("rewrite", { id, drop: 1 });
    await until("agent.timeline.replacement", () => sessionOut(log, "agent.timeline.replacement").length === 1);
    await tab.settle();
    await ctl("row", { id, turn, item: assistant("«after-replacement»", "msg-after-replacement") });
    await until("the row after the replacement", () => occurrences(tab.rendered(), "«after-replacement»") === 1);
    // the socket closes mid-turn; meanwhile a retract rewrites the history again and the turn goes on
    await ctl("row", { id, turn, item: assistant("«retracted-2»", "msg-retracted-2") });
    await until("the row about to be retracted", () => occurrences(tab.rendered(), "«retracted-2»") === 1);
    await tab.close();
    await ctl("rewrite", { id, drop: 1 });
    await ctl("row", { id, turn, item: assistant("«while-away»", "msg-while-away") });
    // back: the app resumes from its cursor (the old epoch: pigeon answers with a reset), then observes again
    await tab.connect();
    try {
      const resumed = await tab.fetch(app.plan.planTimelineResumeFetch(tab.state.cursor ?? null));
      assert.equal(resumed.reset, true, "a stale epoch resumes with a reset");
      await tab.observe();
      await ctl("row", { id, turn, item: assistant("«last»", "msg-last") });
      await ctl("turn_end", { id });
      await until("turn_completed", () => streamEvents(log, id, "turn_completed").length > 0);
      await tab.settle();
      const shown = tab.rendered();
      assert.deepEqual(shown, await freshRender(app, internal, id));
      for (const text of ["before", "kept", "after-replacement", "while-away", "last"]) {
        assert.equal(occurrences(shown, `«${text}»`), 1, `${text} once: ${shown.join(" | ")}`);
      }
      for (const text of ["retracted-1", "retracted-2"]) assert.equal(occurrences(shown, `«${text}»`), 0, `${text} gone: ${shown.join(" | ")}`);
    } finally {
      await tab.close();
    }
    assertValidForEveryClient(log, await everyProtocol());
  });
});
