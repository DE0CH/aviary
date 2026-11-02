// The Paseo protocol package of one client build, loaded the way that build validates frames.
//
// A client build is a DE0CH/claudecodeui commit. Its `paseo/packages/protocol` is fetched (a blobless, shallow fetch
// of just that commit), its runtime packages (zod, zod-aot, semver, …) are installed at the versions the commit's own
// `paseo/package-lock.json` resolves (lib/runtime.mjs), its zod-aot validators are generated with the commit's own
// generator script (what the shipped bundle runs), and the package is bundled with esbuild into one ESM module.
// Installs are shared by every build that resolves the same versions; builds are cached by the package's git tree.
//
// Every validator a client applies to a server frame is exposed:
//   - `outbound`: `WSOutboundMessageSchema` (zod), the schema of record;
//   - `aot`: `validateWSOutboundMessage` (zod-aot output), the validator the shipped bundle actually runs;
//   - `knownStatus`: `KnownStatusPayloadSchema`, applied by the client to `status` payloads it knows;
//   - `serverInfo`: `ServerInfoStatusPayloadSchema`, the client's parse of the hello reply;
//   - `inbound`: `WSInboundMessageSchema`, for checking frames a driver sends.
// plus `tree` (the package's git tree), `id` (tree and runtime install: builds with one id validate identically) and
// `runtime`: the versions the protocol package resolved for its own dependencies (`{ zod: "4.4.3", … }`).
//
// `clientForCommit` bundles the same commit's `paseo/packages/client` (with its protocol and relay packages, ws
// included), the SDK the apps speak with, for drivers that talk to a server the way a shipped client does.
//
// Concurrent processes may share the cache: every directory is built under a unique scratch name and renamed into
// place; when another process got there first, its copy is kept.

import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import { assertIsolated, installRuntime, publish, runtimePlan, scratch } from "./runtime.mjs";

export const harnessRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const pkg = JSON.parse(readFileSync(path.join(harnessRoot, "package.json"), "utf8"));

/**
 * Where fetched commits, runtime installs and built bundles live (kept between runs). Outside any tree with a
 * `node_modules` (checked), so a build can only resolve what its own install holds.
 */
export const cacheRoot = process.env.PASEO_CONTRACT_CACHE
  ? path.resolve(process.env.PASEO_CONTRACT_CACHE)
  : path.join(process.env.XDG_CACHE_HOME || path.join(os.homedir(), ".cache"), "paseo-contract");

/** The claudecodeui repository the client commits come from (a URL, or a local clone for offline runs). */
export const repository = process.env.PASEO_CONTRACT_REPOSITORY || pkg.paseo.repository;

const SHA = /^[0-9a-f]{40}$/;

/** Built packages are keyed by this too: a change to the build (these files) or to the pinned esbuild rebuilds them. */
const buildId = (() => {
  const hash = createHash("sha256").update(readFileSync(fileURLToPath(import.meta.url)));
  hash.update(readFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), "runtime.mjs")));
  const lock = path.join(harnessRoot, "package-lock.json");
  if (existsSync(lock)) hash.update(readFileSync(lock));
  return hash.digest("hex").slice(0, 8);
})();

function git(args, options = {}) {
  return execFileSync("git", args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], maxBuffer: 1 << 28, ...options }).trim();
}

function sleep(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

let checked = false;
function cache() {
  mkdirSync(cacheRoot, { recursive: true });
  if (!checked) assertIsolated(cacheRoot);
  checked = true;
  return cacheRoot;
}

/** The bare, partial clone every commit is fetched into (created under a scratch name, then renamed into place). */
function mirror() {
  const dir = path.join(cache(), "claudecodeui.git");
  if (!existsSync(path.join(dir, "HEAD"))) {
    const work = scratch(dir);
    git(["init", "--bare", "--quiet", work]);
    git(["-C", work, "remote", "add", "origin", repository]);
    git(["-C", work, "config", "remote.origin.promisor", "true"]);
    git(["-C", work, "config", "remote.origin.partialclonefilter", "blob:none"]);
    publish(work, dir);
  }
  return dir;
}

function haveCommit(dir, sha) {
  return spawnSync("git", ["-C", dir, "cat-file", "-e", `${sha}^{commit}`], { stdio: "ignore" }).status === 0;
}

/**
 * Fetches one commit (no history, blobs on demand) into the mirror; returns the mirror's directory. Another process
 * fetching at the same time holds the mirror's shallow.lock, so a failed fetch is retried a few times.
 */
function fetchCommit(sha) {
  if (!SHA.test(sha)) throw new Error(`not a full commit hash: ${sha}`);
  const dir = mirror();
  for (let attempt = 1; !haveCommit(dir, sha); attempt++) {
    try {
      git(["-C", dir, "fetch", "--quiet", "--depth=1", "--filter=blob:none", "origin", sha]);
    } catch (error) {
      if (haveCommit(dir, sha)) break;
      if (attempt >= 6) throw new Error(`fetching claudecodeui ${sha} failed: ${error.stderr || error.message}`);
      sleep(250 * attempt);
    }
  }
  return dir;
}

/** The committer time of a commit, UTC, to the second (`2026-11-02T08:29:13Z`): the order of clients.txt. */
export function commitTime(sha) {
  const dir = fetchCommit(sha);
  return git(["-C", dir, "show", "-s", "--date=format-local:%Y-%m-%dT%H:%M:%SZ", "--format=%cd", sha], {
    env: { ...process.env, TZ: "UTC" },
  });
}

/** The git tree hash of `paseo/packages/<name>` at `sha`, or null when the commit has none. */
function packageTree(sha, name) {
  const dir = fetchCommit(sha);
  const result = spawnSync("git", ["-C", dir, "rev-parse", "--verify", "--quiet", `${sha}:paseo/packages/${name}`], { encoding: "utf8" });
  return result.status === 0 ? result.stdout.trim() : null;
}

/** Whether the commit has a Paseo client (the protocol package). */
export function hasPaseo(sha) {
  return packageTree(sha, "protocol") !== null;
}

/** The git tree hash of the commit's protocol package (throws when it is not a Paseo client build). */
export function protocolTree(sha) {
  const tree = packageTree(sha, "protocol");
  if (!tree) throw new Error(`claudecodeui ${sha} has no paseo/packages/protocol: it is not a Paseo client build`);
  return tree;
}

/** The commit's `paseo/package-lock.json`. */
function commitLock(sha) {
  const dir = fetchCommit(sha);
  try {
    return JSON.parse(git(["-C", dir, "show", `${sha}:paseo/package-lock.json`]));
  } catch (error) {
    throw new Error(`claudecodeui ${sha}: cannot read paseo/package-lock.json (${error.message})`);
  }
}

/** Installs what `lock` resolves for `workspaces` (lib/runtime.mjs); returns `{ dir, plan }`. */
async function runtimeFor(lock, workspaces, label) {
  let plan;
  try {
    plan = runtimePlan(lock, workspaces);
  } catch (error) {
    throw new Error(`${label}: ${error.message}`);
  }
  return { dir: await installRuntime(plan, cache()), plan };
}

/** Writes `paseo/packages/<name>` of `sha` for each name into `target/<name>`. */
function extract(sha, names, target) {
  const dir = fetchCommit(sha);
  mkdirSync(target, { recursive: true });
  const archive = spawnSync("git", ["-C", dir, "archive", "--format=tar", sha, ...names.map((n) => `paseo/packages/${n}`)], {
    maxBuffer: 1 << 30,
  });
  if (archive.status !== 0) throw new Error(`git archive ${sha} failed: ${archive.stderr}`);
  const untar = spawnSync("tar", ["-x", "-C", target, "--strip-components=2"], { input: archive.stdout });
  if (untar.status !== 0) throw new Error(`untar ${sha} failed: ${untar.stderr}`);
}

function run(cmd, args, cwd) {
  const result = spawnSync(cmd, args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
  if (result.status !== 0) {
    throw new Error(`${cmd} ${args.join(" ")} (in ${cwd}) failed:\n${result.stdout}\n${result.stderr}`);
  }
}

/**
 * Generates a protocol package's zod-aot validators with the package's own generator script; returns whether it has
 * them. The generator patches zod-aot in place (as the app's build does), so the package gets its own copy of the
 * runtime's zod-aot to patch: the shared install stays as unpacked.
 */
function generateAot(dir, runtimeDir) {
  if (!existsSync(path.join(dir, "src/validation/ws-outbound.ts"))) return false;
  const own = path.join(dir, "node_modules", "zod-aot");
  if (!existsSync(own)) {
    const shared = path.join(runtimeDir, "node_modules", "zod-aot");
    if (!existsSync(shared)) throw new Error(`${dir}: the package generates zod-aot validators but its lockfile installs no zod-aot`);
    cpSync(shared, own, { recursive: true, dereference: true });
  }
  run(process.execPath, ["scripts/generate-validation-aot.mjs"], dir);
  return true;
}

/**
 * Builds one bundle inside a runtime install: `populate(work)` writes the workspace packages to `work/<name>`; each
 * gets its own nested node_modules from the install (`packages/<name>/node_modules`), so it resolves exactly what it
 * resolves in the app; the protocol's validators are generated; `entry` (a file name and its text, written inside
 * `work/<entryWorkspace>` so its imports resolve from that package) is bundled into `work/<outfile>`. Returns the
 * directory.
 */
async function buildIn(runtimeDir, name, { populate, workspaces, entryWorkspace, entry, outfile, alias = {}, external = [], banner }) {
  const dir = path.join(runtimeDir, name);
  if (existsSync(path.join(dir, outfile))) return dir;
  const work = scratch(dir);
  try {
    populate(work);
    for (const workspace of workspaces) {
      const nested = path.join(runtimeDir, "packages", workspace, "node_modules");
      if (existsSync(nested)) cpSync(nested, path.join(work, workspace, "node_modules"), { recursive: true });
    }
    const hasAot = workspaces.includes("protocol") ? generateAot(path.join(work, "protocol"), runtimeDir) : false;
    const entryFile = path.join(work, entryWorkspace, entry.name);
    writeFileSync(entryFile, entry.text(hasAot));
    const esbuild = await import("esbuild");
    await esbuild.build({
      entryPoints: [entryFile],
      outfile: path.join(work, outfile),
      bundle: true,
      platform: "node",
      format: "esm",
      target: "node22",
      alias: Object.fromEntries(Object.entries(alias).map(([from, to]) => [from, path.join(work, to)])),
      external,
      banner: banner ? { js: banner } : undefined,
      logLevel: "error",
      legalComments: "none",
    });
    writeFileSync(path.join(work, "meta.json"), JSON.stringify({ hasAot }, null, 2));
    publish(work, dir);
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
  return dir;
}

const PROTOCOL_ENTRY = {
  name: "__contract_entry.ts",
  text: (hasAot) =>
    [
      'export * as messages from "./src/messages.ts";',
      'export { z } from "zod";',
      hasAot ? 'export { validateWSOutboundMessage } from "./src/validation/ws-outbound.ts";' : "export const validateWSOutboundMessage = null;",
      "",
    ].join("\n"),
};

async function buildProtocol(runtime, name, populate) {
  return buildIn(runtime.dir, name, {
    populate,
    workspaces: ["protocol"],
    entryWorkspace: "protocol",
    entry: PROTOCOL_ENTRY,
    outfile: "bundle.mjs",
    alias: { "@getpaseo/protocol": "protocol/src" },
  });
}

/**
 * Writes the commit's `paseo/packages/<name>` for each name to `target/packages/<name>` and its
 * `paseo/package-lock.json` to `target/package-lock.json`: the layout `protocolForDirectory` reads.
 */
export function exportCommit(sha, names, target) {
  extract(sha, names, path.join(target, "packages"));
  writeFileSync(path.join(target, "package-lock.json"), `${JSON.stringify(commitLock(sha), null, 2)}\n`);
}

const loaded = new Map();

async function load(dir, label, runtime) {
  const mod = await import(pathToFileURL(path.join(dir, "bundle.mjs")).href);
  const bundled = mod.z?.core?.version;
  if (bundled && runtime?.zod && `${bundled.major}.${bundled.minor}.${bundled.patch}` !== runtime.zod) {
    throw new Error(`${label}: the bundle runs zod ${bundled.major}.${bundled.minor}.${bundled.patch}, the lockfile resolves ${runtime.zod}`);
  }
  const m = mod.messages;
  for (const name of ["WSOutboundMessageSchema", "WSInboundMessageSchema"]) {
    if (!m[name]) throw new Error(`${label}: messages.ts exports no ${name}`);
  }
  return {
    label,
    outbound: m.WSOutboundMessageSchema,
    inbound: m.WSInboundMessageSchema,
    knownStatus: m.KnownStatusPayloadSchema ?? null,
    serverInfo: m.ServerInfoStatusPayloadSchema ?? null,
    aot: mod.validateWSOutboundMessage ?? null,
    messages: m,
    z: mod.z,
    runtime,
  };
}

/** The validators of client commit `sha` (fetched, installed and built on first use). */
export async function protocolForCommit(sha) {
  const tree = protocolTree(sha);
  const label = `claudecodeui ${sha.slice(0, 12)}`;
  const runtime = await runtimeFor(commitLock(sha), ["protocol"], label);
  const id = `${tree}-${runtime.plan.key}`;
  if (loaded.has(id)) return { ...loaded.get(id), commit: sha, tree, id };
  const dir = await buildProtocol(runtime, `protocol-${tree}-${buildId}`, (work) => extract(sha, ["protocol"], work));
  const protocol = await load(dir, `${label} (protocol tree ${tree.slice(0, 10)})`, runtime.plan.versions.protocol);
  loaded.set(id, protocol);
  return { ...protocol, commit: sha, tree, id };
}

/**
 * The validators of a protocol package on disk (an app's own checkout, `paseo/packages/protocol`), installed from the
 * checkout's own lockfile (`paseo/package-lock.json`, two levels up). The package is copied, so the checkout is left as
 * it was; the copy is rebuilt when the package's files change.
 */
export async function protocolForDirectory(source) {
  const abs = path.resolve(source);
  if (!existsSync(path.join(abs, "src/messages.ts"))) throw new Error(`${abs} is not a Paseo protocol package (no src/messages.ts)`);
  const lockFile = path.resolve(abs, "..", "..", "package-lock.json");
  if (!existsSync(lockFile)) throw new Error(`${abs}: no lockfile at ${lockFile} (the package must sit at <app>/packages/protocol)`);
  const runtime = await runtimeFor(JSON.parse(readFileSync(lockFile, "utf8")), ["protocol"], `local ${abs}`);
  const hash = createHash("sha256");
  const files = execFileSync("find", [abs, "-type", "f", "-not", "-path", "*/node_modules/*", "-not", "-path", "*/generated/*"], {
    encoding: "utf8",
  })
    .split("\n")
    .filter(Boolean)
    .sort();
  for (const file of files) hash.update(file.slice(abs.length)).update(readFileSync(file));
  const key = `${hash.digest("hex").slice(0, 20)}-${buildId}`;
  const dir = await buildProtocol(runtime, `local-${key}`, (work) =>
    cpSync(abs, path.join(work, "protocol"), { recursive: true, filter: (p) => !p.slice(abs.length).split(path.sep).includes("node_modules") }),
  );
  return { ...(await load(dir, `local ${abs}`, runtime.plan.versions.protocol)), commit: null, tree: key, id: `${key}-${runtime.plan.key}` };
}

const clientsLoaded = new Map();

/**
 * The client SDK of client commit `sha`: its `paseo/packages/client` bundled (with the same commit's protocol and relay
 * packages, the protocol's zod-aot validators generated, and the ws, zod, tweetnacl, … its lockfile resolves) into one
 * ESM module, fetched, installed and built on first use. Returns `{ commit, sdk, node, runtime }`: `sdk` is the package
 * root (`createPaseoClient`, …), `node` its `./node` entry, `runtime` what each package resolved.
 */
export async function clientForCommit(sha) {
  const workspaces = ["protocol", "client", "relay"];
  const trees = workspaces.map((name) => {
    const tree = packageTree(sha, name);
    if (!tree) throw new Error(`claudecodeui ${sha} has no paseo/packages/${name}: it is not a Paseo client build`);
    return tree;
  });
  const runtime = await runtimeFor(commitLock(sha), workspaces, `claudecodeui ${sha.slice(0, 12)}`);
  const key = createHash("sha256").update(trees.join(" ")).digest("hex").slice(0, 20);
  const id = `${key}-${runtime.plan.key}`;
  if (clientsLoaded.has(id)) return { ...clientsLoaded.get(id), commit: sha };
  const dir = await buildIn(runtime.dir, `client-${key}-${buildId}`, {
    populate: (work) => extract(sha, workspaces, work),
    workspaces,
    entryWorkspace: "client",
    entry: {
      name: "__contract_client.ts",
      text: () => ['export * as sdk from "./src/index.ts";', 'export * as node from "./src/node/index.ts";', ""].join("\n"),
    },
    outfile: "client.mjs",
    alias: { "@getpaseo/protocol": "protocol/src", "@getpaseo/relay": "relay/src" },
    // ws's optional native add-ons, required inside a try
    external: ["bufferutil", "utf-8-validate"],
    // CommonJS dependencies (ws, tweetnacl) require Node built-ins
    banner: 'import { createRequire as __contractRequire } from "node:module"; const require = __contractRequire(import.meta.url);',
  });
  const mod = await import(pathToFileURL(path.join(dir, "client.mjs")).href);
  if (typeof mod.sdk.createPaseoClient !== "function") throw new Error(`claudecodeui ${sha.slice(0, 12)}: the client package exports no createPaseoClient`);
  const client = { sdk: mod.sdk, node: mod.node, key: id, runtime: runtime.plan.versions };
  clientsLoaded.set(id, client);
  return { ...client, commit: sha };
}

/** The kinds of client a clients.txt line may name. `web-legacy`: a ui image from before the Paseo apps (no protocol). */
export const KINDS = ["web", "ios", "web-legacy"];
const TIME = /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/;

/**
 * Every line of `clients.txt`, in file order: `{ sha, kinds, time, legacy, line }`. The file must be sorted by commit
 * time (oldest first), so the last line is the newest build; `legacy` lines (only `web-legacy`) have no Paseo client and
 * are skipped by the validators.
 */
export function readClientLines(file = path.join(harnessRoot, "clients.txt")) {
  const commits = [];
  for (const [index, raw] of readFileSync(file, "utf8").split("\n").entries()) {
    const line = raw.replace(/#.*/, "").trim();
    if (!line) continue;
    const [sha, kinds, time, extra] = line.split(/\s+/);
    const where = `${file}:${index + 1}`;
    if (!SHA.test(sha)) throw new Error(`${where}: not a full commit hash: ${sha}`);
    const list = (kinds ?? "").split(",");
    if (!kinds || !list.every((k) => KINDS.includes(k)) || new Set(list).size !== list.length) {
      throw new Error(`${where}: kinds must be a comma-separated set of ${KINDS.join(", ")}`);
    }
    if (list.includes("web-legacy") && list.length > 1) throw new Error(`${where}: web-legacy is a kind of its own`);
    if (!TIME.test(time ?? "")) throw new Error(`${where}: the commit time must be YYYY-MM-DDTHH:MM:SSZ, not ${time}`);
    if (extra !== undefined) throw new Error(`${where}: unexpected ${extra}`);
    if (commits.some((c) => c.sha === sha)) throw new Error(`${where}: ${sha} is listed twice`);
    const previous = commits.at(-1);
    if (previous && previous.time > time) throw new Error(`${where}: out of order (${time} is older than the line before, ${previous.time})`);
    commits.push({ sha, kinds: list, time, legacy: list[0] === "web-legacy", line: index + 1 });
  }
  return commits;
}

/** The Paseo client builds of `clients.txt` (legacy lines left out), oldest first. */
export function readClients(file = path.join(harnessRoot, "clients.txt")) {
  const commits = readClientLines(file).filter((client) => !client.legacy);
  if (commits.length === 0) throw new Error(`${file} lists no Paseo client`);
  return commits;
}

/**
 * The newest listed Paseo client of `kind` (any kind when null), by commit time. It is not necessarily the build being
 * served: a build is listed when it is built, before (or without) its deploy, and a rollback serves an older one. A
 * driver that needs the deployed build takes its commit from the deploy pin (outside this repository) as an input.
 */
export function newestClient(kind = null, file = path.join(harnessRoot, "clients.txt")) {
  const clients = readClients(file).filter((client) => kind === null || client.kinds.includes(kind));
  if (clients.length === 0) throw new Error(`${file} lists no ${kind} client`);
  return clients.at(-1);
}
