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
  return execFileSync("git", args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], ...options }).trim();
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

/** The committer date (YYYY-MM-DD) of a fetched commit. */
export function commitDate(sha) {
  protocolTree(sha);
  return git(["-C", mirror(), "show", "-s", "--format=%cs", sha]);
}

/** Fetches one commit (no history, blobs on demand); returns the git tree hash of its protocol package. */
export function protocolTree(sha) {
  if (!SHA.test(sha)) throw new Error(`not a full commit hash: ${sha}`);
  const dir = mirror();
  if (!haveCommit(dir, sha)) {
    git(["-C", dir, "fetch", "--quiet", "--depth=1", "--filter=blob:none", "origin", sha], { stdio: ["ignore", "pipe", "inherit"] });
  }
  try {
    return git(["-C", dir, "rev-parse", `${sha}:paseo/packages/protocol`]);
  } catch {
    throw new Error(`claudecodeui ${sha} has no paseo/packages/protocol: it is not a Paseo client build`);
  }
}

/** Writes `paseo/packages/protocol` of `sha` into `target`. */
function extract(sha, target) {
  const dir = mirror();
  mkdirSync(target, { recursive: true });
  const archive = spawnSync("git", ["-C", dir, "archive", "--format=tar", sha, "paseo/packages/protocol"], {
    maxBuffer: 1 << 30,
  });
  if (archive.status !== 0) throw new Error(`git archive ${sha} failed: ${archive.stderr}`);
  const untar = spawnSync("tar", ["-x", "-C", target, "--strip-components=3"], { input: archive.stdout });
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
async function build(work, out) {
  const hasAot = existsSync(path.join(work, "src/validation/ws-outbound.ts"));
  if (hasAot) {
    // the commit's own generator (it patches zod-aot in place, idempotently, as the app's build does)
    run(process.execPath, ["scripts/generate-validation-aot.mjs"], work);
  }
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
  if (loaded.has(tree)) return { ...loaded.get(tree), commit: sha, tree };
  const dir = path.join(cache(), `protocol-${tree}-${buildId}`);
  if (!existsSync(path.join(dir, "bundle.mjs"))) {
    const work = `${dir}.work-${process.pid}`;
    rmSync(work, { recursive: true, force: true });
    extract(sha, work);
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

/** The client commits listed in `clients.txt` (first token of each non-comment line), in file order (oldest first). */
export function readClients(file = path.join(harnessRoot, "clients.txt")) {
  const commits = [];
  for (const [index, raw] of readFileSync(file, "utf8").split("\n").entries()) {
    const line = raw.replace(/#.*/, "").trim();
    if (!line) continue;
    const [sha, kinds] = line.split(/\s+/);
    if (!SHA.test(sha)) throw new Error(`${file}:${index + 1}: not a full commit hash: ${sha}`);
    if (!kinds || !/^(web|ios)(,(web|ios))*$/.test(kinds)) throw new Error(`${file}:${index + 1}: kinds must be web, ios or web,ios`);
    if (commits.some((c) => c.sha === sha)) throw new Error(`${file}:${index + 1}: ${sha} is listed twice`);
    commits.push({ sha, kinds: kinds.split(","), line: index + 1 });
  }
  if (commits.length === 0) throw new Error(`${file} lists no client`);
  return commits;
}
