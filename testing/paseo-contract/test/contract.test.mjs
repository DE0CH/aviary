// The harness's own acceptance (WP-10): the schema wiring is proven by a recorded frame log of the CloudCLI bridge
// (the server the shipped clients talk to today) validating for every client; a seeded invalid frame fails; clients.txt
// holds the web build the bridge log was recorded at and the shipped iOS builds, sorted; the exported schema is current;
// each build runs the runtime its own lockfile resolves; the build cache is safe for concurrent processes; a new client
// appended the way clients.yml does keeps the suite green, and a burst of parallel appends loses none.
import assert from "node:assert/strict";
import { execFile, spawn, spawnSync } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { before, describe, it } from "node:test";
import { promisify } from "node:util";

import {
  clientForCommit,
  exportCommit,
  harnessRoot,
  newestClient,
  protocolForCommit,
  protocolForDirectory,
  readClientLines,
  readClients,
} from "../lib/protocol.mjs";
import { runtimePlan } from "../lib/runtime.mjs";
import { inboundProblem, outboundProblem, readFrameLog, validateRecords } from "../lib/validate.mjs";

const fixtures = path.join(harnessRoot, "fixtures");
const bridgeLogs = readdirSync(fixtures).filter((name) => name.startsWith("cloudcli-bridge-")).map((name) => path.join(fixtures, name));
const specFrames = path.join(fixtures, "pigeon-spec-frames.ndjson");
const seeded = path.join(fixtures, "seeded-invalid.ndjson");
const nested = process.env.PASEO_CONTRACT_NESTED === "1";

function node(script, args, cwd = harnessRoot, env = process.env) {
  return spawnSync(process.execPath, [path.join(cwd, script), ...args], { encoding: "utf8", cwd, env });
}

/** Runs a command; resolves to { status, stdout, stderr } (never rejects). */
function exec(cmd, args, options = {}) {
  return new Promise((resolve) => {
    const child = spawn(cmd, args, { ...options, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (d) => (stdout += d));
    child.stderr.on("data", (d) => (stderr += d));
    child.on("close", (status) => resolve({ status, stdout, stderr }));
  });
}

/** The registry's dist record of an npm package version ({ tarball, integrity }). */
async function registryDist(name, version) {
  const response = await fetch(`https://registry.npmjs.org/${name}/${version}`);
  assert.ok(response.ok, `registry ${name}@${version}: ${response.status}`);
  return (await response.json()).dist;
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

  it("clients.txt is sorted and holds the recorded web build, the shipped iOS builds and the pre-Paseo ui images", () => {
    // the web build each bridge log was recorded at (fixtures/cloudcli-bridge-<commit>.ndjson; the deployed build then)
    for (const log of bridgeLogs) {
      const prefix = path.basename(log).match(/^cloudcli-bridge-([0-9a-f]{8,40})\.ndjson$/)[1];
      const recorded = clients.find((client) => client.sha.startsWith(prefix));
      assert.ok(recorded?.kinds.includes("web"), `the web build ${prefix} of ${path.basename(log)} is listed as web`);
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

  it("runtime plans follow Node's resolution in the client's lockfile", () => {
    const lock = {
      packages: {
        "packages/protocol": { dependencies: { semver: "^7", zod: "^4" }, devDependencies: { "zod-aot": "0.20.4", vitest: "4" } },
        "packages/protocol/node_modules/semver": { version: "7.8.5", resolved: "https://r/semver-7.8.5.tgz", integrity: "sha512-a" },
        "node_modules/semver": { version: "7.7.4", resolved: "https://r/semver-7.7.4.tgz", integrity: "sha512-b" },
        "node_modules/zod": { version: "4.4.3", resolved: "https://r/zod-4.4.3.tgz", integrity: "sha512-c" },
        "node_modules/zod-aot": {
          version: "0.20.4",
          resolved: "https://r/zod-aot-0.20.4.tgz",
          integrity: "sha512-d",
          dependencies: { acorn: "^8" },
          peerDependencies: { zod: "^4", missing: "1" },
          peerDependenciesMeta: { missing: { optional: true } },
        },
        "node_modules/zod-aot/node_modules/acorn": { version: "8.16.0", resolved: "https://r/acorn.tgz", integrity: "sha512-e" },
        "node_modules/vitest": { version: "4.1.6", resolved: "https://r/vitest.tgz", integrity: "sha512-f" },
      },
    };
    const plan = runtimePlan(lock, ["protocol"]);
    // the workspace's own semver wins over the root's; zod-aot (a build tool) is in, vitest (a dev tool) is not
    assert.deepEqual(plan.versions.protocol, { semver: "7.8.5", zod: "4.4.3", "zod-aot": "0.20.4" });
    assert.deepEqual(Object.keys(plan.packages), [
      "node_modules/zod",
      "node_modules/zod-aot",
      "node_modules/zod-aot/node_modules/acorn",
      "packages/protocol/node_modules/semver",
    ]);
    lock.packages["node_modules/zod"].version = "4.4.2";
    assert.notEqual(runtimePlan(lock, ["protocol"]).key, plan.key, "another version is another install");
    delete lock.packages["node_modules/zod"];
    assert.throws(() => runtimePlan(lock, ["protocol"]), /resolves no zod for packages\/protocol > zod/);
  });

  it("two clients whose lockfiles resolve different zod versions are validated in one run, each under its own", async () => {
    const commit = newestClient("web").sha;
    const base = mkdtempSync(path.join(tmpdir(), "paseo-two-runtimes-"));
    const dirs = {};
    for (const [name, zod] of [["current", null], ["older-zod", "4.4.2"]]) {
      const app = path.join(base, name);
      exportCommit(commit, ["protocol"], app);
      if (zod) {
        const lockFile = path.join(app, "package-lock.json");
        const lock = JSON.parse(readFileSync(lockFile, "utf8"));
        const dist = await registryDist("zod", zod);
        lock.packages["node_modules/zod"] = { ...lock.packages["node_modules/zod"], version: zod, resolved: dist.tarball, integrity: dist.integrity };
        writeFileSync(lockFile, JSON.stringify(lock, null, 2));
      }
      dirs[name] = path.join(app, "packages", "protocol");
    }
    const current = await protocolForDirectory(dirs.current);
    const older = await protocolForDirectory(dirs["older-zod"]);
    const fromCommit = await protocolForCommit(commit);
    assert.equal(current.runtime.zod, fromCommit.runtime.zod);
    assert.equal(older.runtime.zod, "4.4.2");
    assert.notEqual(current.id, older.id);
    const version = (p) => Object.values(p.z.core.version).join(".");
    assert.equal(version(current), current.runtime.zod, "the bundle runs the zod its lockfile resolves");
    assert.equal(version(older), "4.4.2");
    const records = [...bridgeLogs.flatMap((file) => readFrameLog(file)), ...readFrameLog(specFrames)];
    for (const protocol of [current, older]) assert.deepEqual(validateRecords(protocol, records).failures, [], protocol.label);
    // and through the script: one run, two protocol builds, each named with its zod
    const result = node("validate-frames.mjs", ["--protocol-dir", dirs.current, "--protocol-dir", dirs["older-zod"], ...bridgeLogs]);
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.match(result.stdout, new RegExp(`^ok .*\\(zod ${current.runtime.zod.replaceAll(".", "\\.")} \\+ zod-aot`, "m"));
    assert.match(result.stdout, /^ok .*\(zod 4\.4\.2 \+ zod-aot/m);
  });

  it("processes building the same client into one fresh cache at the same time all succeed", { skip: nested && "inside the simulation" }, async () => {
    const cache = mkdtempSync(path.join(tmpdir(), "paseo-cache-race-"));
    const env = { ...process.env, PASEO_CONTRACT_CACHE: cache };
    const commit = newestClient("web").sha;
    const runs = await Promise.all(
      [1, 2, 3].map(() => exec(process.execPath, ["validate-frames.mjs", "--commit", commit, ...bridgeLogs], { cwd: harnessRoot, env })),
    );
    for (const run of runs) assert.equal(run.status, 0, run.stdout + run.stderr);
    const left = readdirSync(cache);
    assert.ok(!left.some((name) => name.includes(".work-")), `no scratch directory is left: ${left.join(" ")}`);
    assert.equal(left.filter((name) => name.startsWith("runtime-")).length, 1, "one shared runtime install");
  });

  it("append-client inserts by commit time, merges a kind, lists pre-Paseo ui images as web-legacy, refuses the rest", () => {
    const dir = mkdtempSync(path.join(tmpdir(), "paseo-clients-"));
    const file = path.join(dir, "clients.txt");
    const older = "01d7db996cc2a12a70bd82194e895428325638dc"; // 2026-10-28, an iOS run cancelled before its upload
    const legacy = "12397ad3b9389bf8357cb72bfc5bf6c2d6e77dda"; // 2026-10-21, before paseo/
    const newer = "a57656af3076b3d8c69ceac8486777233fc83fe7"; // 2026-11-02, a web build
    const start = readFileSync(path.join(harnessRoot, "clients.txt"), "utf8").split("\n").filter((l) => ![older, legacy, newer].some((sha) => l.startsWith(sha)));
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
    // several builds in one call; one bad build leaves the file as it was
    const before = readFileSync(file, "utf8");
    result = node("append-client.mjs", [newer, "web", older, "android", "--file", file]);
    assert.equal(result.status, 2, result.stdout);
    assert.equal(readFileSync(file, "utf8"), before);
    result = node("append-client.mjs", [newer, "web", older, "ios", "--file", file]);
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(result.stdout.trim().split("\n").map((l) => l.split(":")[0]), ["added", "unchanged"]);
    assert.equal(readClientLines(file).at(-1).sha, newer);
    // a file out of order is refused
    const unsorted = path.join(dir, "unsorted.txt");
    writeFileSync(unsorted, `${newest} web 2026-11-02T08:29:13Z\n${older} ios 2026-10-28T03:49:44Z\n`);
    assert.throws(() => readClientLines(unsorted), /out of order/);
    assert.equal(node("append-client.mjs", [legacy, "web", "--file", unsorted]).status, 2);
  });

  it("clientForCommit loads the newest web build's client SDK, which connects to a server that speaks the bridge's handshake", async () => {
    const commit = newestClient("web").sha;
    const { sdk, runtime } = await clientForCommit(commit);
    const protocol = await protocolForCommit(commit);
    assert.match(runtime.client.ws, /^8\./, "ws from the commit's lockfile");
    assert.equal(runtime.client.zod, protocol.runtime.zod);
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
    const newer = "a57656af3076b3d8c69ceac8486777233fc83fe7"; // claudecodeui main, 2026-11-02, after the recorded web build
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
    assert.match(suite.stdout, /^# skipped 3$/m, "the simulation, the cache race and the burst are skipped inside");
    assert.ok(Number(suite.stdout.match(/^# pass (\d+)$/m)?.[1]) >= 10, suite.stdout.slice(-2000));
  });

  it("a burst of parallel appends, landed the way clients.yml does, ends with every build listed", { skip: nested && "inside the simulation" }, async () => {
    // origin: a bare repository holding the harness's clients.txt; each worker is one clients.yml run: a build job on a
    // fresh clone of main (append, make the patch), then the land job (ci/land.sh on another fresh clone); a stale
    // list (exit 3) is a re-dispatch: the worker starts again on the new main. An unrelated push lands meanwhile.
    const run = promisify(execFile);
    const root = mkdtempSync(path.join(tmpdir(), "paseo-burst-"));
    const origin = path.join(root, "origin.git");
    const seed = path.join(root, "seed");
    const builds = [
      ["01d7db996cc2a12a70bd82194e895428325638dc", "ios"],
      ["12397ad3b9389bf8357cb72bfc5bf6c2d6e77dda", "web"],
      ["a57656af3076b3d8c69ceac8486777233fc83fe7", "web"],
      ["4bbd14270fb46a948cc1c1b60cb7f01ffc0a63f1", "web"],
      ["3968b730261d9dee137933f2837678bb6c07a672", "web"],
    ];
    const git = (cwd, ...args) => run("git", ["-c", "user.name=t", "-c", "user.email=t@t", "-c", "core.hooksPath=/dev/null", ...args], { cwd });
    await run("git", ["init", "-q", "--bare", "-b", "main", origin]);
    await run("git", ["clone", "-q", origin, seed]);
    mkdirSync(path.join(seed, "testing/paseo-contract"), { recursive: true });
    const start = readFileSync(path.join(harnessRoot, "clients.txt"), "utf8")
      .split("\n")
      .filter((line) => !builds.some(([sha]) => line.startsWith(sha)));
    writeFileSync(path.join(seed, "testing/paseo-contract/clients.txt"), start.join("\n"));
    writeFileSync(path.join(seed, "other.txt"), "0\n");
    await git(seed, "add", "-A");
    await git(seed, "commit", "-q", "-m", "seed");
    await git(seed, "push", "-q", "origin", "HEAD:main");
    const land = path.join(harnessRoot, "ci/land.sh");
    const rounds = [];
    const worker = async (index, sha, kind) => {
      for (let attempt = 1; attempt <= 20; attempt++) {
        const build = path.join(root, `build-${index}-${attempt}`);
        await run("git", ["clone", "-q", origin, build]);
        const added = node("append-client.mjs", [sha, kind, "--file", path.join(build, "testing/paseo-contract/clients.txt")]);
        assert.equal(added.status, 0, added.stderr);
        await git(build, "add", "-A", "testing/paseo-contract");
        const { stdout: patch } = await git(build, "diff", "--cached", "--binary");
        const { stdout: base } = await git(build, "rev-parse", "HEAD");
        writeFileSync(path.join(root, `patch-${index}-${attempt}`), patch);
        const landing = path.join(root, `land-${index}-${attempt}`);
        await run("git", ["clone", "-q", origin, landing]);
        const result = await exec("bash", [land, path.join(root, `patch-${index}-${attempt}`), base.trim(), `client ${sha.slice(0, 12)}`], { cwd: landing });
        if (result.status === 0) return rounds.push(attempt);
        assert.equal(result.status, 3, result.stdout + result.stderr);
      }
      throw new Error(`${sha} did not land in 20 attempts`);
    };
    const unrelated = (async () => {
      const other = path.join(root, "other");
      await run("git", ["clone", "-q", origin, other]);
      writeFileSync(path.join(other, "other.txt"), "1\n");
      await git(other, "commit", "-q", "-am", "unrelated");
      for (let i = 0; i < 10; i++) {
        try {
          return await git(other, "push", "-q", "origin", "HEAD:main");
        } catch {
          await git(other, "pull", "-q", "--rebase", "origin", "main");
        }
      }
    })();
    await Promise.all([...builds.map(([sha, kind], index) => worker(index, sha, kind)), unrelated]);
    const final = path.join(root, "final");
    await run("git", ["clone", "-q", origin, final]);
    const listed = readClientLines(path.join(final, "testing/paseo-contract/clients.txt"));
    for (const [sha] of builds) assert.ok(listed.some((c) => c.sha === sha), `${sha} is listed`);
    assert.equal(listed.length, start.filter((l) => /^[0-9a-f]{40} /.test(l)).length + builds.length);
    assert.equal(readFileSync(path.join(final, "other.txt"), "utf8"), "1\n", "the unrelated push is kept");
    assert.ok(Math.max(...rounds) > 1, `some runs went stale and were redone (attempts: ${rounds.join(" ")})`);
  });
});
