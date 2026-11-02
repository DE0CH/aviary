// The harness's own acceptance (WP-10): the schema wiring is proven by a recorded frame log of the CloudCLI bridge
// (the server the shipped clients talk to today) validating for every client; a seeded invalid frame fails; clients.txt
// holds the live web build and the shipped iOS builds, sorted; the exported schema is current; a new client appended
// the way clients.yml does keeps the suite green.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { cpSync, mkdtempSync, readFileSync, readdirSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { before, describe, it } from "node:test";

import {
  clientForCommit,
  harnessRoot,
  installedVersion,
  newestClient,
  protocolForCommit,
  readClientLines,
  readClients,
  runtimeMismatches,
} from "../lib/protocol.mjs";
import { inboundProblem, outboundProblem, readFrameLog, validateRecords } from "../lib/validate.mjs";

const fixtures = path.join(harnessRoot, "fixtures");
const bridgeLogs = readdirSync(fixtures).filter((name) => name.startsWith("cloudcli-bridge-")).map((name) => path.join(fixtures, name));
const specFrames = path.join(fixtures, "pigeon-spec-frames.ndjson");
const seeded = path.join(fixtures, "seeded-invalid.ndjson");
const nested = process.env.PASEO_CONTRACT_NESTED === "1";

function node(script, args, cwd = harnessRoot) {
  return spawnSync(process.execPath, [path.join(cwd, script), ...args], { encoding: "utf8", cwd });
}

describe("paseo contract harness", () => {
  const lines = readClientLines();
  const clients = readClients();
  const protocols = new Map(); // tree -> protocol

  before(async () => {
    for (const client of clients) {
      const protocol = await protocolForCommit(client.sha);
      if (!protocols.has(protocol.tree)) protocols.set(protocol.tree, protocol);
    }
  });

  it("clients.txt is sorted and holds the live web build, the shipped iOS builds and the pre-Paseo ui images", () => {
    // the live web build: the commit each bridge log was recorded at (fixtures/cloudcli-bridge-<commit>.ndjson)
    for (const log of bridgeLogs) {
      const prefix = path.basename(log).match(/^cloudcli-bridge-([0-9a-f]{8,40})\.ndjson$/)[1];
      const live = clients.find((client) => client.sha.startsWith(prefix));
      assert.ok(live?.kinds.includes("web"), `the live web build ${prefix} of ${path.basename(log)} is listed as web`);
    }
    const ios = clients.filter((client) => client.kinds.includes("ios"));
    assert.ok(ios.length >= 11, `the TestFlight builds since 2026-10-22 are listed (${ios.length})`);
    assert.ok(newestClient("web").kinds.includes("web"), "a Paseo web build is listed");
    for (const legacy of lines.filter((client) => client.legacy)) {
      assert.ok(legacy.time < "2026-10-26T16:54:50Z", `${legacy.sha} (web-legacy) is older than the first Paseo commit`);
    }
    const times = lines.map((client) => client.time);
    assert.deepEqual(times, [...times].sort(), "sorted by commit time, oldest first");
  });

  it("every client build's validators are the shipped ones (zod and the zod-aot validator)", () => {
    for (const protocol of protocols.values()) {
      assert.equal(typeof protocol.outbound.safeParse, "function");
      assert.equal(typeof protocol.aot, "function", `${protocol.label} has a generated validateWSOutboundMessage`);
    }
  });

  it("the recorded CloudCLI bridge log and pigeon's spec frames are valid for every client", () => {
    assert.ok(bridgeLogs.length > 0);
    const records = bridgeLogs.flatMap((file) => readFrameLog(file));
    const spec = readFrameLog(specFrames);
    assert.ok(spec.some((r) => r.dir === "out" && r.frame.type === "hello.rejected"));
    for (const protocol of protocols.values()) {
      const { checked, failures } = validateRecords(protocol, [...records, ...spec]);
      assert.deepEqual(failures.map((f) => `${f.file}:${f.line} ${f.type}: ${f.problem}`), [], protocol.label);
      assert.ok(checked.out >= 100, `server frames checked: ${checked.out}`);
      assert.ok(checked.in >= 50, `client frames checked: ${checked.in}`);
    }
    // what the log covers: a user row with an image and a file, a hello of a newer protocol
    const text = JSON.stringify(records.map((r) => r.frame));
    assert.match(text, /"images":\[\{"url":"data:image\/png/);
    assert.match(text, /"attachments":\[\{"type":"uploaded_file"/);
    assert.ok(records.some((r) => r.dir === "in" && r.frame.type === "hello" && r.frame.protocolVersion === 2));
  });

  it("every seeded invalid frame is rejected by every client", () => {
    const records = readFrameLog(seeded);
    assert.ok(records.length >= 10);
    for (const protocol of protocols.values()) {
      for (const record of records) {
        assert.ok(outboundProblem(protocol, record.frame), `${protocol.label} rejects: ${record.case}`);
      }
    }
  });

  it("a frame marked invalid that every validator accepts is a failure", () => {
    const [protocol] = protocols.values();
    const records = [{ dir: "out", frame: { type: "pong" }, expectInvalid: true, case: "marked", file: "-", line: 1 }];
    assert.equal(validateRecords(protocol, records).failures.length, 1);
  });

  it("validate-frames exits 0 on the recorded log and 1 on a seeded invalid frame", () => {
    const ok = node("validate-frames.mjs", [...bridgeLogs, specFrames]);
    assert.equal(ok.status, 0, ok.stdout + ok.stderr);
    const bad = node("validate-frames.mjs", ["--commit", newestClient("web").sha, seeded]);
    assert.equal(bad.status, 1, bad.stdout + bad.stderr);
    assert.match(bad.stdout, /FAIL protocol/);
  });

  it("the exported JSON Schema of the newest client is committed and current", () => {
    const result = node("export-schema.mjs", ["--check"]);
    assert.equal(result.status, 0, result.stdout + result.stderr);
  });

  it("a client commit whose lockfile resolves another runtime than the harness's is refused", () => {
    assert.equal(installedVersion("zod"), "4.4.3");
    const lock = {
      packages: {
        "node_modules/zod": { version: "4.5.0" },
        "node_modules/zod-aot": { version: installedVersion("zod-aot") },
        "node_modules/semver": { version: "7.7.4" },
        "packages/protocol/node_modules/semver": { version: installedVersion("semver") }, // the workspace's own wins
      },
    };
    assert.deepEqual(runtimeMismatches(lock, ["protocol"]), ["zod: packages/protocol resolves 4.5.0, the harness has 4.4.3"]);
    delete lock.packages["packages/protocol/node_modules/semver"];
    assert.equal(runtimeMismatches(lock, ["protocol"]).length, 2);
    assert.match(runtimeMismatches({ packages: {} }, ["relay"])[0], /resolves nothing/);
  });

  it("append-client inserts by commit time, merges a kind, lists pre-Paseo ui images as web-legacy, refuses the rest", () => {
    const dir = mkdtempSync(path.join(tmpdir(), "paseo-clients-"));
    const file = path.join(dir, "clients.txt");
    const older = "01d7db996cc2a12a70bd82194e895428325638dc"; // 2026-10-28, an iOS run cancelled before its upload
    const legacy = "12397ad3b9389bf8357cb72bfc5bf6c2d6e77dda"; // 2026-10-21, before paseo/
    const start = readFileSync(path.join(harnessRoot, "clients.txt"), "utf8").split("\n").filter((l) => !l.startsWith(older) && !l.startsWith(legacy));
    writeFileSync(file, start.join("\n"));
    const newest = readClientLines(file).at(-1).sha;
    let result = node("append-client.mjs", [older, "ios", "--file", file]);
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /^added/);
    let listed = readClientLines(file);
    assert.equal(listed.at(-1).sha, newest, "a late build of an older commit does not become the newest");
    const entry = listed.find((c) => c.sha === older);
    assert.deepEqual([entry.kinds, entry.time], [["ios"], "2026-10-28T03:49:44Z"]);
    assert.ok(listed[listed.indexOf(entry) + 1].time >= entry.time && listed[listed.indexOf(entry) - 1].time <= entry.time);
    result = node("append-client.mjs", [older, "ios", "--file", file]);
    assert.match(result.stdout, /^unchanged/);
    result = node("append-client.mjs", [older, "web", "--file", file]);
    assert.match(result.stdout, /^updated/);
    assert.deepEqual(readClientLines(file).find((c) => c.sha === older).kinds, ["web", "ios"]);
    // a ui image of the CloudCLI web UI (before paseo/), dispatched as web: listed, as web-legacy
    result = node("append-client.mjs", [legacy, "web", "--file", file]);
    assert.equal(result.status, 0, result.stderr);
    listed = readClientLines(file);
    assert.deepEqual(listed.find((c) => c.sha === legacy).kinds, ["web-legacy"]);
    assert.ok(!readClients(file).some((c) => c.sha === legacy), "the validators skip it");
    result = node("append-client.mjs", [legacy, "web", "--file", file]);
    assert.match(result.stdout, /^unchanged/);
    // the same commit as an iOS build, a short hash, a bad kind
    for (const args of [[legacy, "ios"], ["01d7db996cc2", "ios"], [older, "android"]]) {
      result = node("append-client.mjs", [...args, "--file", file]);
      assert.equal(result.status, 2, `${args.join(" ")}: ${result.stdout}`);
    }
    // a file out of order is refused
    const unsorted = path.join(dir, "unsorted.txt");
    writeFileSync(unsorted, `${newest} web 2026-11-02T08:29:13Z\n${older} ios 2026-10-28T03:49:44Z\n`);
    assert.throws(() => readClientLines(unsorted), /out of order/);
    assert.equal(node("append-client.mjs", [legacy, "web", "--file", unsorted]).status, 2);
  });

  it("clientForCommit loads the newest web build's client SDK, which connects to a server that speaks the bridge's handshake", async () => {
    const commit = newestClient("web").sha;
    const { sdk } = await clientForCommit(commit);
    const protocol = await protocolForCommit(commit);
    const { WebSocketServer } = await import("ws");
    const serverInfo = readFrameLog(bridgeLogs[0]).find(
      (r) => r.dir === "out" && r.frame.type === "session" && r.frame.message?.payload?.status === "server_info",
    ).frame;
    const server = new WebSocketServer({ host: "127.0.0.1", port: 0, path: "/ws" });
    await new Promise((resolve) => server.once("listening", resolve));
    const sent = [];
    server.on("connection", (socket) => {
      socket.on("message", (data) => {
        const frame = JSON.parse(String(data));
        sent.push(frame);
        if (frame.type === "hello") socket.send(JSON.stringify(serverInfo));
        else if (frame.type === "ping") socket.send(JSON.stringify({ type: "pong" }));
      });
    });
    const client = sdk.createPaseoClient({ url: `ws://127.0.0.1:${server.address().port}/ws` });
    try {
      await client.connect();
      assert.ok(sent.some((frame) => frame.type === "hello"), "the client said hello");
      for (const frame of sent) assert.equal(inboundProblem(protocol, frame), null, JSON.stringify(frame));
    } finally {
      await client.close();
      await new Promise((resolve) => server.close(resolve));
    }
  });

  it("appending a new client the way clients.yml does keeps the suite green", { skip: nested && "inside the simulation" }, () => {
    // a copy of the harness (its node_modules shared), with a newer web build and a late older build appended
    const copy = mkdtempSync(path.join(tmpdir(), "paseo-contract-copy-"));
    cpSync(harnessRoot, copy, { recursive: true, filter: (p) => !p.startsWith(path.join(harnessRoot, "node_modules")) });
    symlinkSync(path.join(harnessRoot, "node_modules"), path.join(copy, "node_modules"), "dir");
    const newer = "a57656af3076b3d8c69ceac8486777233fc83fe7"; // claudecodeui main, 2026-11-02, after the live web build
    for (const [sha, kind] of [[newer, "web"], ["01d7db996cc2a12a70bd82194e895428325638dc", "ios"]]) {
      const result = node("append-client.mjs", [sha, kind], copy);
      assert.equal(result.status, 0, result.stdout + result.stderr);
    }
    const listed = readClientLines(path.join(copy, "clients.txt"));
    assert.ok(listed.some((c) => c.sha === newer && c.kinds.includes("web")));
    const exported = node("export-schema.mjs", [], copy);
    assert.equal(exported.status, 0, exported.stdout + exported.stderr);
    const newestSha = newestClient(null, path.join(copy, "clients.txt")).sha;
    assert.deepEqual(readdirSync(path.join(copy, "schema")), [`${newestSha}.json`], "the newest client's schema, by commit time");
    const env = { ...process.env, PASEO_CONTRACT_NESTED: "1" };
    delete env.NODE_TEST_CONTEXT; // a run of its own, not a child of this one
    const suite = spawnSync(process.execPath, ["--test", "--test-reporter=tap", "--test-concurrency=1", ...readdirSync(path.join(copy, "test")).filter((n) => n.endsWith(".test.mjs")).map((n) => path.join("test", n))], {
      cwd: copy,
      encoding: "utf8",
      env,
    });
    assert.equal(suite.status, 0, suite.stdout.slice(-4000) + suite.stderr.slice(-2000));
    assert.match(suite.stdout, /^# fail 0$/m);
    assert.match(suite.stdout, /^# skipped 1$/m, "the simulation itself is skipped inside");
    assert.ok(Number(suite.stdout.match(/^# pass (\d+)$/m)?.[1]) >= 10, suite.stdout.slice(-2000));
  });
});
