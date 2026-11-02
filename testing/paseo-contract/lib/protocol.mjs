// The Paseo protocol package of one client build, loaded the way that build validates frames.
//
// A client build is a DE0CH/claudecodeui commit. Its `paseo/packages/protocol` is fetched (a blobless, shallow fetch
// of just that commit), its zod-aot validators are generated with the commit's own generator script (what the shipped
// bundle runs), and the package is bundled with esbuild into one ESM module. Builds are cached by the protocol
// package's git tree hash, so the many commits that share one protocol cost one build.
//
// Every validator a client applies to a server frame is exposed:
//   - `outbound`: `WSOutboundMessageSchema` (zod), the schema of record;
//   - `aot`: `validateWSOutboundMessage` (zod-aot output), the validator the shipped bundle actually runs;
//   - `knownStatus`: `KnownStatusPayloadSchema`, applied by the client to `status` payloads it knows;
//   - `serverInfo`: `ServerInfoStatusPayloadSchema`, the client's parse of the hello reply;
//   - `inbound`: `WSInboundMessageSchema`, for checking frames a driver sends.
//
// `clientForCommit` bundles the same commit's `paseo/packages/client` (with its protocol and relay packages), the SDK
// the apps speak with, for drivers that talk to a server the way a shipped client does.
//
// A build runs under the harness's one installed version of each runtime package (zod, zod-aot, semver, ws, …). A
// client commit whose own `paseo/package-lock.json` resolves another version is refused (an error, exit 2 in the
// scripts) instead of being validated under the wrong runtime: bump the harness's pin to match.

import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdirSync, readFileSync, renameSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

export const harnessRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const pkg = JSON.parse(readFileSync(path.join(harnessRoot, "package.json"), "utf8"));

/** Where fetched commits and built protocol bundles live (inside node_modules: never committed, kept between runs). */
export const cacheRoot = process.env.PASEO_CONTRACT_CACHE
  ? path.resolve(process.env.PASEO_CONTRACT_CACHE)
  : path.join(harnessRoot, "node_modules", ".cache", "paseo-contract");

/** The claudecodeui repository the client commits come from (a URL, or a local clone for offline runs). */
export const repository = process.env.PASEO_CONTRACT_REPOSITORY || pkg.paseo.repository;

const SHA = /^[0-9a-f]{40}$/;

/** Built packages are keyed by this too: a change to the build (this file) or to the pinned tools rebuilds them. */
const buildId = (() => {
  const hash = createHash("sha256").update(readFileSync(fileURLToPath(import.meta.url)));
  const lock = path.join(harnessRoot, "package-lock.json");
  if (existsSync(lock)) hash.update(readFileSync(lock));
  return hash.digest("hex").slice(0, 8);
})();

function git(args, options = {}) {
  return execFileSync("git", args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], maxBuffer: 1 << 28, ...options }).trim();
}

/**
 * The cache directory. A protocol package built there resolves `zod`, `zod-aot` and `semver` by walking up to a
 * `node_modules`: inside the harness's own `node_modules` that is found; elsewhere (CI caches it under the runner's
 * home) a `node_modules` link to the harness's is made.
 */
function cache() {
  mkdirSync(cacheRoot, { recursive: true });
  const inside = path.relative(path.join(harnessRoot, "node_modules"), cacheRoot);
  const link = path.join(cacheRoot, "node_modules");
  if ((inside.startsWith("..") || path.isAbsolute(inside)) && !existsSync(link)) {
    symlinkSync(path.join(harnessRoot, "node_modules"), link, "dir");
  }
  return cacheRoot;
}

/** The bare, partial clone every commit is fetched into. */
function mirror() {
  const dir = path.join(cache(), "claudecodeui.git");
  if (!existsSync(path.join(dir, "HEAD"))) {
    mkdirSync(dir, { recursive: true });
    git(["init", "--bare", "--quiet", dir]);
    git(["-C", dir, "remote", "add", "origin", repository]);
    git(["-C", dir, "config", "remote.origin.promisor", "true"]);
    git(["-C", dir, "config", "remote.origin.partialclonefilter", "blob:none"]);
  }
  return dir;
}

function haveCommit(dir, sha) {
  return spawnSync("git", ["-C", dir, "cat-file", "-e", `${sha}^{commit}`], { stdio: "ignore" }).status === 0;
}

/** Fetches one commit (no history, blobs on demand) into the mirror; returns the mirror's directory. */
function fetchCommit(sha) {
  if (!SHA.test(sha)) throw new Error(`not a full commit hash: ${sha}`);
  const dir = mirror();
  if (!haveCommit(dir, sha)) {
    git(["-C", dir, "fetch", "--quiet", "--depth=1", "--filter=blob:none", "origin", sha], { stdio: ["ignore", "pipe", "inherit"] });
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

/**
 * The runtime packages each Paseo package is built and run with. The harness installs one version of each
 * (package.json); a client commit must resolve exactly that version for each package that uses it.
 */
export const RUNTIME = {
  protocol: ["zod", "zod-aot", "semver"],
  client: ["ws", "zod"],
  relay: ["ws", "tweetnacl", "base64-js", "@noble/ciphers", "@noble/curves", "@noble/hashes"],
};

/** The version of a package installed in the harness's node_modules. */
export function installedVersion(name) {
  const file = path.join(harnessRoot, "node_modules", name, "package.json");
  return existsSync(file) ? JSON.parse(readFileSync(file, "utf8")).version : null;
}

/**
 * Where an app lockfile (`paseo/package-lock.json`, npm workspaces) and the harness disagree: for each named Paseo
 * package, each runtime package it uses resolves (as Node would: the workspace's own node_modules first, then the
 * root's) to a version other than the one the harness has installed.
 */
export function runtimeMismatches(lock, packages, installed = installedVersion) {
  const entries = lock?.packages ?? {};
  const problems = [];
  for (const pkg of packages) {
    for (const name of RUNTIME[pkg]) {
      const want = (entries[`packages/${pkg}/node_modules/${name}`] ?? entries[`node_modules/${name}`])?.version ?? null;
      const have = installed(name);
      if (want !== have) problems.push(`${name}: packages/${pkg} resolves ${want ?? "nothing"}, the harness has ${have ?? "nothing"}`);
    }
  }
  return problems;
}

/** Throws when the commit's lockfile resolves another runtime than the harness's (see `runtimeMismatches`). */
function checkRuntime(sha, packages) {
  const dir = fetchCommit(sha);
  let lock;
  try {
    lock = JSON.parse(git(["-C", dir, "show", `${sha}:paseo/package-lock.json`]));
  } catch (error) {
    throw new Error(`claudecodeui ${sha}: cannot read paseo/package-lock.json (${error.message})`);
  }
  const problems = runtimeMismatches(lock, packages);
  if (problems.length) {
    throw new Error(
      `claudecodeui ${sha.slice(0, 12)} runs other package versions than the harness pins (update testing/paseo-contract/package.json and its lockfile): ${problems.join("; ")}`,
    );
  }
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
 * Builds a protocol package directory (generated validators + one bundle) into `out`. The package keeps the layout
 * its generator script expects (`src/`, `scripts/`, `codegen/`); `node_modules` resolution walks up to the
 * harness's own pinned zod / zod-aot / semver.
 */
/**
 * Generates a protocol package's zod-aot validators with the package's own generator script; returns whether it has
 * them. The generator patches zod-aot in place (as the app's build does), so the package gets its own copy of the
 * harness's zod-aot to patch: the harness's node_modules stay as installed.
 */
function generateAot(dir) {
  if (!existsSync(path.join(dir, "src/validation/ws-outbound.ts"))) return false;
  const own = path.join(dir, "node_modules", "zod-aot");
  rmSync(own, { recursive: true, force: true });
  cpSync(path.join(harnessRoot, "node_modules", "zod-aot"), own, { recursive: true, dereference: true });
  run(process.execPath, ["scripts/generate-validation-aot.mjs"], dir);
  return true;
}

async function build(work, out) {
  const hasAot = generateAot(work);
  const entry = path.join(work, "__contract_entry.ts");
  writeFileSync(
    entry,
    [
      'export * as messages from "./src/messages.ts";',
      'export { z } from "zod";',
      hasAot ? 'export { validateWSOutboundMessage } from "./src/validation/ws-outbound.ts";' : "export const validateWSOutboundMessage = null;",
      "",
    ].join("\n"),
  );
  const esbuild = await import("esbuild");
  await esbuild.build({
    entryPoints: [entry],
    outfile: path.join(work, "bundle.mjs"),
    bundle: true,
    platform: "node",
    format: "esm",
    target: "node22",
    alias: { "@getpaseo/protocol": path.join(work, "src") },
    nodePaths: [path.join(harnessRoot, "node_modules")],
    logLevel: "error",
    legalComments: "none",
  });
  writeFileSync(path.join(work, "meta.json"), JSON.stringify({ hasAot }, null, 2));
  rmSync(out, { recursive: true, force: true });
  renameSync(work, out);
}

const loaded = new Map();

async function load(dir, label) {
  const mod = await import(pathToFileURL(path.join(dir, "bundle.mjs")).href);
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
  };
}

/** The validators of client commit `sha` (fetched and built on first use). */
export async function protocolForCommit(sha) {
  const tree = protocolTree(sha);
  checkRuntime(sha, ["protocol"]);
  if (loaded.has(tree)) return { ...loaded.get(tree), commit: sha, tree };
  const dir = path.join(cache(), `protocol-${tree}-${buildId}`);
  if (!existsSync(path.join(dir, "bundle.mjs"))) {
    const work = `${dir}.work-${process.pid}`;
    rmSync(work, { recursive: true, force: true });
    extract(sha, ["protocol"], `${work}.x`);
    renameSync(path.join(`${work}.x`, "protocol"), work);
    rmSync(`${work}.x`, { recursive: true, force: true });
    await build(work, dir);
  }
  const protocol = await load(dir, `claudecodeui ${sha.slice(0, 12)} (protocol tree ${tree.slice(0, 10)})`);
  loaded.set(tree, protocol);
  return { ...protocol, commit: sha, tree };
}

/**
 * The validators of a protocol package on disk (an app's own checkout, `paseo/packages/protocol`). It is copied, so
 * the checkout is left as it was; the copy is rebuilt when the package's files change.
 */
export async function protocolForDirectory(source) {
  const abs = path.resolve(source);
  if (!existsSync(path.join(abs, "src/messages.ts"))) throw new Error(`${abs} is not a Paseo protocol package (no src/messages.ts)`);
  const hash = createHash("sha256");
  const files = execFileSync("find", [abs, "-type", "f", "-not", "-path", "*/node_modules/*", "-not", "-path", "*/generated/*"], {
    encoding: "utf8",
  })
    .split("\n")
    .filter(Boolean)
    .sort();
  for (const file of files) hash.update(file.slice(abs.length)).update(readFileSync(file));
  const key = `${hash.digest("hex").slice(0, 20)}-${buildId}`;
  const dir = path.join(cache(), `local-${key}`);
  if (!existsSync(path.join(dir, "bundle.mjs"))) {
    const work = `${dir}.work-${process.pid}`;
    rmSync(work, { recursive: true, force: true });
    cpSync(abs, work, { recursive: true, filter: (p) => !p.includes(`${path.sep}node_modules`) });
    await build(work, dir);
  }
  return { ...(await load(dir, `local ${abs}`)), commit: null, tree: key };
}

const clientsLoaded = new Map();

/**
 * The client SDK of client commit `sha`: its `paseo/packages/client` bundled (with the same commit's protocol and relay
 * packages, the protocol's zod-aot validators generated) into one ESM module, fetched and built on first use. Returns
 * `{ commit, sdk, node }`: `sdk` is the package root (`createPaseoClient`, …), `node` its `./node` entry. `ws` is the
 * harness's own (the version the commit's lockfile resolves, checked).
 */
export async function clientForCommit(sha) {
  const trees = ["protocol", "client", "relay"].map((name) => {
    const tree = packageTree(sha, name);
    if (!tree) throw new Error(`claudecodeui ${sha} has no paseo/packages/${name}: it is not a Paseo client build`);
    return tree;
  });
  checkRuntime(sha, ["protocol", "client", "relay"]);
  const key = createHash("sha256").update(trees.join(" ")).digest("hex").slice(0, 20);
  if (clientsLoaded.has(key)) return { ...clientsLoaded.get(key), commit: sha };
  const dir = path.join(cache(), `client-${key}-${buildId}`);
  if (!existsSync(path.join(dir, "client.mjs"))) {
    const work = `${dir}.work-${process.pid}`;
    rmSync(work, { recursive: true, force: true });
    extract(sha, ["protocol", "client", "relay"], work);
    generateAot(path.join(work, "protocol"));
    const entry = path.join(work, "__contract_client.ts");
    writeFileSync(entry, ['export * as sdk from "./client/src/index.ts";', 'export * as node from "./client/src/node/index.ts";', ""].join("\n"));
    const esbuild = await import("esbuild");
    await esbuild.build({
      entryPoints: [entry],
      outfile: path.join(work, "client.mjs"),
      bundle: true,
      platform: "node",
      format: "esm",
      target: "node22",
      alias: { "@getpaseo/protocol": path.join(work, "protocol/src"), "@getpaseo/relay": path.join(work, "relay/src") },
      // ws (with its optional native add-ons) is loaded from the harness at run time, not bundled
      external: ["ws", "bufferutil", "utf-8-validate"],
      // CommonJS dependencies (tweetnacl) require Node built-ins
      banner: { js: 'import { createRequire as __contractRequire } from "node:module"; const require = __contractRequire(import.meta.url);' },
      nodePaths: [path.join(harnessRoot, "node_modules")],
      logLevel: "error",
      legalComments: "none",
    });
    rmSync(dir, { recursive: true, force: true });
    renameSync(work, dir);
  }
  const mod = await import(pathToFileURL(path.join(dir, "client.mjs")).href);
  if (typeof mod.sdk.createPaseoClient !== "function") throw new Error(`claudecodeui ${sha.slice(0, 12)}: the client package exports no createPaseoClient`);
  const client = { sdk: mod.sdk, node: mod.node, key };
  clientsLoaded.set(key, client);
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

/** The newest Paseo client of `kind` (any kind when null): the live web build is `newestClient("web")`. */
export function newestClient(kind = null, file = path.join(harnessRoot, "clients.txt")) {
  const clients = readClients(file).filter((client) => kind === null || client.kinds.includes(kind));
  if (clients.length === 0) throw new Error(`${file} lists no ${kind} client`);
  return clients.at(-1);
}
