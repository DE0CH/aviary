// The harness's own acceptance (WP-10): the schema wiring is proven by a recorded frame log of the CloudCLI bridge
// (the server the shipped clients talk to today) validating for every client; a seeded invalid frame fails; clients.txt
// holds the live web build and the shipped iOS builds; the exported schema is current.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdtempSync, readFileSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { before, describe, it } from "node:test";

import { harnessRoot, protocolForCommit, readClients } from "../lib/protocol.mjs";
import { outboundProblem, readFrameLog, validateRecords } from "../lib/validate.mjs";

const pkg = JSON.parse(readFileSync(path.join(harnessRoot, "package.json"), "utf8"));
const fixtures = path.join(harnessRoot, "fixtures");
const bridgeLogs = readdirSync(fixtures).filter((name) => name.startsWith("cloudcli-bridge-")).map((name) => path.join(fixtures, name));
const seeded = path.join(fixtures, "seeded-invalid.ndjson");

function node(script, ...args) {
  return spawnSync(process.execPath, [path.join(harnessRoot, script), ...args], { encoding: "utf8", cwd: harnessRoot });
}

describe("paseo contract harness", () => {
  const clients = readClients();
  const protocols = new Map(); // tree -> protocol

  before(async () => {
    for (const client of clients) {
      const protocol = await protocolForCommit(client.sha);
      if (!protocols.has(protocol.tree)) protocols.set(protocol.tree, protocol);
    }
  });

  it("clients.txt holds the live web build and the shipped iOS builds", () => {
    const live = clients.find((client) => client.sha === pkg.paseo.client);
    assert.ok(live, `the live web build ${pkg.paseo.client} (package.json paseo.client) is listed`);
    assert.ok(live.kinds.includes("web"));
    const ios = clients.filter((client) => client.kinds.includes("ios"));
    assert.ok(ios.length >= 11, `the TestFlight builds since 2026-10-22 are listed (${ios.length})`);
    assert.equal(clients.at(-1).sha, pkg.paseo.client, "the newest client is the live web build");
  });

  it("every client build's validators are the shipped ones (zod and the zod-aot validator)", () => {
    for (const protocol of protocols.values()) {
      assert.equal(typeof protocol.outbound.safeParse, "function");
      assert.equal(typeof protocol.aot, "function", `${protocol.label} has a generated validateWSOutboundMessage`);
    }
  });

  it("the recorded CloudCLI bridge log is valid for every client", () => {
    assert.ok(bridgeLogs.length > 0);
    const records = bridgeLogs.flatMap((file) => readFrameLog(file));
    for (const protocol of protocols.values()) {
      const { checked, failures } = validateRecords(protocol, records);
      assert.deepEqual(failures.map((f) => `${f.line} ${f.type}: ${f.problem}`), [], protocol.label);
      assert.ok(checked.out >= 100, `server frames checked: ${checked.out}`);
      assert.ok(checked.in >= 50, `client frames checked: ${checked.in}`);
    }
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
    const ok = node("validate-frames.mjs", ...bridgeLogs);
    assert.equal(ok.status, 0, ok.stdout + ok.stderr);
    const bad = node("validate-frames.mjs", "--commit", pkg.paseo.client, seeded);
    assert.equal(bad.status, 1, bad.stdout + bad.stderr);
    assert.match(bad.stdout, /FAIL protocol/);
  });

  it("the exported JSON Schema of the newest client is committed and current", () => {
    const result = node("export-schema.mjs", "--check");
    assert.equal(result.status, 0, result.stdout + result.stderr);
  });

  it("append-client adds a Paseo build, merges a kind, and refuses what is not a client", () => {
    const dir = mkdtempSync(path.join(tmpdir(), "paseo-clients-"));
    const file = path.join(dir, "clients.txt");
    copyFileSync(path.join(harnessRoot, "clients.txt"), file);
    const added = "01d7db996cc2a12a70bd82194e895428325638dc"; // an iOS run that was cancelled before its upload
    let result = node("append-client.mjs", added, "ios", "--file", file);
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /^added/);
    assert.equal(readClients(file).at(-1).sha, added);
    result = node("append-client.mjs", added, "ios", "--file", file);
    assert.match(result.stdout, /^unchanged/);
    result = node("append-client.mjs", added, "web", "--file", file);
    assert.match(result.stdout, /^updated/);
    assert.deepEqual(readClients(file).at(-1).kinds, ["web", "ios"]);
    // a CloudCLI-UI build from before the Paseo apps, a short hash, a bad kind
    for (const args of [["f3f56c112a9d17c8a138a1748223930252b44b6d", "web"], ["01d7db996cc2", "ios"], [added, "android"]]) {
      result = node("append-client.mjs", ...args, "--file", file);
      assert.equal(result.status, 2, `${args.join(" ")}: ${result.stdout}`);
    }
  });
});
