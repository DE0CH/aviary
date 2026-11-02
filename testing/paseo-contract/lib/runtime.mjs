// The runtime packages of one client build, installed from that build's own lockfile.
//
// A Paseo client build runs its protocol / client / relay packages with the versions its `paseo/package-lock.json`
// resolves (zod, zod-aot, semver, ws, tweetnacl, …). The harness installs exactly those, per build: it walks the
// lockfile from the named workspaces (their `dependencies`, plus the build tools the harness runs, such as the
// protocol's zod-aot generator) through every transitive dependency the way Node resolves them, downloads each
// package's tarball from the URL the lockfile records, checks it against the lockfile's integrity hash, and unpacks it
// at the lockfile's own path (`node_modules/x`, `packages/protocol/node_modules/y`). No install scripts run.
//
// The result is a directory that mirrors the app's install for those workspaces. It is keyed by what was installed, so
// every client build that resolves the same versions shares one. Builds placed inside it resolve their imports there
// and nowhere else: the cache must not sit under any directory with a `node_modules` (checked), so a package missing
// from the lockfile is an error, never a silent fallback to some other copy.

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

/** Build tools the harness runs from a workspace's devDependencies (the protocol's zod-aot generator). */
export const BUILD_TOOLS = { protocol: ["zod-aot"] };

const installerId = createHash("sha256").update(readFileSync(fileURLToPath(import.meta.url))).digest("hex").slice(0, 8);

/** The parent install location of a lockfile path: `a/node_modules/b` -> `a`, `node_modules/b` / `packages/x` -> ``. */
function parentOf(location) {
  const at = location.lastIndexOf("/node_modules/");
  return at >= 0 ? location.slice(0, at) : "";
}

/** The lockfile path Node reaches for `name` required from the package at `from` (null when none). */
function resolveIn(entries, from, name) {
  for (let at = from; ; at = parentOf(at)) {
    const key = at ? `${at}/node_modules/${name}` : `node_modules/${name}`;
    if (entries[key]) return key;
    if (!at) return null;
  }
}

function platformMatches(entry) {
  const ok = (list, value) => {
    if (!list?.length) return true;
    if (list.includes(`!${value}`)) return false;
    const allowed = list.filter((v) => !v.startsWith("!"));
    return allowed.length === 0 || allowed.includes(value);
  };
  return ok(entry.os, process.platform) && ok(entry.cpu, process.arch);
}

/**
 * What to install for `workspaces` (names under `packages/`) from an npm lockfile (v2/v3 `packages` map): `{ key,
 * packages: { <lockfile path>: { version, resolved, integrity } }, versions: { <workspace>: { <dep>: <version> } } }`.
 * `versions` is what each workspace resolves for its own direct dependencies. Dependencies on other workspaces
 * (lockfile links) are left out: the harness bundles the sibling packages from source.
 */
export function runtimePlan(lock, workspaces) {
  const entries = lock?.packages;
  if (!entries) throw new Error("the lockfile has no packages map (lockfile v2 or later is needed)");
  const packages = {};
  const versions = {};
  const visit = (from, name, optional, chain) => {
    const key = resolveIn(entries, from, name);
    if (!key) {
      if (optional) return null;
      throw new Error(`the lockfile resolves no ${name} for ${chain.join(" > ")}`);
    }
    const entry = entries[key];
    if (entry.link) return null; // another workspace
    if (entry.inBundle) return entry.version; // shipped inside its parent's tarball
    if (entry.optional && !platformMatches(entry)) return null;
    if (packages[key]) return entry.version;
    if (!entry.resolved || !entry.integrity) throw new Error(`the lockfile has no resolved URL and integrity for ${key}`);
    packages[key] = { version: entry.version, resolved: entry.resolved, integrity: entry.integrity };
    for (const dep of Object.keys(entry.dependencies ?? {})) visit(key, dep, false, [...chain, dep]);
    for (const dep of Object.keys(entry.optionalDependencies ?? {})) visit(key, dep, true, [...chain, dep]);
    for (const dep of Object.keys(entry.peerDependencies ?? {})) {
      visit(key, dep, Boolean(entry.peerDependenciesMeta?.[dep]?.optional), [...chain, dep]);
    }
    return entry.version;
  };
  for (const workspace of workspaces) {
    const root = entries[`packages/${workspace}`];
    if (!root) throw new Error(`the lockfile has no workspace packages/${workspace}`);
    const names = new Set(Object.keys(root.dependencies ?? {}));
    for (const tool of BUILD_TOOLS[workspace] ?? []) if (root.devDependencies?.[tool]) names.add(tool);
    versions[workspace] = {};
    for (const name of [...names].sort()) {
      const version = visit(`packages/${workspace}`, name, false, [`packages/${workspace}`, name]);
      if (version) versions[workspace][name] = version;
    }
  }
  const sorted = Object.fromEntries(Object.entries(packages).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)));
  const key = createHash("sha256").update(installerId).update(JSON.stringify(sorted)).digest("hex").slice(0, 20);
  return { key, packages: sorted, versions };
}

/** Moves a finished `work` directory to `out`; when another process published `out` first, keeps theirs. */
export function publish(work, out) {
  try {
    renameSync(work, out);
  } catch (error) {
    if (!["EEXIST", "ENOTEMPTY", "EISDIR", "EPERM"].includes(error.code) || !existsSync(out)) throw error;
    rmSync(work, { recursive: true, force: true });
  }
}

/** A fresh, unique scratch directory name beside `out`. */
export function scratch(out) {
  return `${out}.work-${process.pid}-${Math.random().toString(36).slice(2, 10)}`;
}

/** Throws when Node resolution from inside `dir` could reach a `node_modules` above it. */
export function assertIsolated(dir) {
  for (let at = path.dirname(path.resolve(dir)); ; at = path.dirname(at)) {
    if (path.basename(at) !== "node_modules" && existsSync(path.join(at, "node_modules"))) {
      throw new Error(
        `the build cache ${dir} is inside ${at}, which has a node_modules: a package missing from a client's lockfile would silently resolve there. Set PASEO_CONTRACT_CACHE elsewhere.`,
      );
    }
    if (path.dirname(at) === at) return;
  }
}

async function download(url, integrity) {
  const [algorithm, expected] = integrity.split(/\s+/)[0].split(/-(.*)/s);
  if (!/^sha(256|384|512)$/.test(algorithm)) throw new Error(`unsupported integrity ${integrity} for ${url}`);
  if (!/^https:\/\//.test(url)) throw new Error(`not an https tarball URL: ${url}`);
  let lastError;
  for (let attempt = 1; attempt <= 4; attempt++) {
    try {
      const response = await fetch(url);
      if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
      const body = Buffer.from(await response.arrayBuffer());
      const actual = createHash(algorithm).update(body).digest("base64");
      if (actual !== expected) throw new Error(`${url}: integrity mismatch (lockfile ${integrity}, got ${algorithm}-${actual})`);
      return body;
    } catch (error) {
      if (/integrity mismatch/.test(error.message)) throw error;
      lastError = error;
      await new Promise((resolve) => setTimeout(resolve, 500 * attempt));
    }
  }
  throw lastError;
}

/** Installs `plan` under `root` (once per plan key); returns the install directory. */
export async function installRuntime(plan, root) {
  const dir = path.join(root, `runtime-${plan.key}`);
  if (existsSync(path.join(dir, "runtime.json"))) return dir;
  mkdirSync(root, { recursive: true });
  const work = scratch(dir);
  try {
    const list = Object.entries(plan.packages);
    const tarballs = new Map();
    for (let i = 0; i < list.length; i += 16) {
      const batch = list.slice(i, i + 16);
      const bodies = await Promise.all(batch.map(([, p]) => download(p.resolved, p.integrity)));
      batch.forEach(([location], j) => tarballs.set(location, bodies[j]));
    }
    // parents before children: a package's own node_modules is unpacked into it afterwards
    for (const [location] of list.sort(([a], [b]) => a.split("/").length - b.split("/").length)) {
      const target = path.join(work, location);
      mkdirSync(target, { recursive: true });
      const untar = spawnSync("tar", ["-xz", "-C", target, "--strip-components=1", "--no-same-owner"], { input: tarballs.get(location) });
      if (untar.status !== 0) throw new Error(`unpacking ${location} failed: ${untar.stderr}`);
    }
    writeFileSync(path.join(work, "runtime.json"), `${JSON.stringify({ versions: plan.versions, packages: plan.packages }, null, 1)}\n`);
    publish(work, dir);
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
  return dir;
}
