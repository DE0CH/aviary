#!/usr/bin/env node
// Adds client builds to clients.txt (pigeon spec 17.1 step 9, cutover 13.1; run by .github/workflows/clients.yml).
//
//   node append-client.mjs <claudecodeui commit, 40 hex> <web|ios> [<commit> <kind>]... [--file CLIENTS]
//
// Each commit is fetched from DE0CH/claudecodeui. A commit already listed gains the kind if it lacks it; a new one is
// inserted at its place by commit time (the file stays sorted oldest first, whatever order builds finish in), so the
// last line is always the newest build. Prints added / updated / unchanged for each build. Every build is checked
// before the file is written: one bad build leaves the file as it was.
//
// A web commit without the Paseo protocol package from before the Paseo apps (a ui image of the old CloudCLI web UI,
// still served within the Access session lifetime) is listed as `web-legacy`: it has no Paseo client to validate, and
// the validators skip it. Any other commit without the protocol package is refused.
//
// Exit 2 on a bad argument or a commit that cannot be a client build.

import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { commitTime, harnessRoot, hasPaseo, readClientLines } from "./lib/protocol.mjs";

/** The first claudecodeui commit with `paseo/` (2026-10-26): web builds before it served the CloudCLI web UI. */
const FIRST_PASEO_TIME = "2026-10-26T16:54:50Z"; // 3968b730261d9dee137933f2837678bb6c07a672

function format(sha, kinds, time) {
  return `${sha} ${kinds.join(",").padEnd(10)} ${time}`;
}

/** Adds one build to the text of a clients file; returns the new text and what was done. */
function addOne(text, sha, requested, scratchFile) {
  if (!/^[0-9a-f]{40}$/.test(sha ?? "")) throw new Error(`not a full lower-case commit hash: ${JSON.stringify(sha)}`);
  if (requested !== "web" && requested !== "ios") throw new Error(`kind must be web or ios, not ${JSON.stringify(requested)}`);
  const time = commitTime(sha); // fetches the commit
  let kind = requested;
  if (!hasPaseo(sha)) {
    if (requested !== "web" || time >= FIRST_PASEO_TIME) {
      throw new Error(`claudecodeui ${sha} has no paseo/packages/protocol: it is not a Paseo client build`);
    }
    kind = "web-legacy";
  }
  writeFileSync(scratchFile, text);
  const clients = readClientLines(scratchFile); // also checks the file is well-formed and sorted
  const lines = text.replace(/\n*$/, "\n").split("\n");
  lines.pop(); // the empty string after the final newline
  const listed = clients.find((client) => client.sha === sha);
  if (listed?.kinds.includes(kind)) return { text, done: `unchanged: ${sha} is listed as ${listed.kinds.join(",")}` };
  if (listed) {
    if (listed.legacy || kind === "web-legacy") throw new Error(`${sha} is listed as ${listed.kinds.join(",")}; it cannot also be ${kind}`);
    const kinds = ["web", "ios"].filter((k) => k === kind || listed.kinds.includes(k));
    lines[listed.line - 1] = format(sha, kinds, listed.time);
    return { text: `${lines.join("\n")}\n`, done: `updated: ${sha} ${kinds.join(",")}` };
  }
  // after every line not newer than it (ties keep arrival order), before the first newer one
  const next = clients.find((client) => client.time > time);
  const at = next ? next.line - 1 : lines.length;
  lines.splice(at, 0, format(sha, [kind], time));
  return { text: `${lines.join("\n")}\n`, done: `added: ${sha} ${kind} ${time}${next ? ` (before ${next.sha.slice(0, 12)})` : " (newest)"}` };
}

const args = process.argv.slice(2);
const at = args.indexOf("--file");
let file = path.join(harnessRoot, "clients.txt");
const scratchDir = mkdtempSync(path.join(tmpdir(), "append-client-"));
try {
  if (at >= 0) {
    if (!args[at + 1] || args[at + 1].startsWith("--")) throw new Error("--file needs a file");
    file = args[at + 1];
    args.splice(at, 2);
  }
  if (args.some((arg) => arg.startsWith("--"))) throw new Error("the only option is --file CLIENTS");
  if (args.length === 0 || args.length % 2 !== 0) throw new Error("give one or more <commit> <web|ios> pairs");
  const scratchFile = path.join(scratchDir, "clients.txt");
  let text = readFileSync(file, "utf8");
  const done = [];
  for (let i = 0; i < args.length; i += 2) {
    const result = addOne(text, args[i], args[i + 1], scratchFile);
    text = result.text;
    done.push(result.done);
  }
  writeFileSync(scratchFile, text);
  readClientLines(scratchFile); // the result is well-formed and sorted
  writeFileSync(file, text);
  for (const line of done) console.log(line);
} catch (error) {
  console.error(`append-client: ${error.message ?? error}`);
  process.exitCode = 2;
} finally {
  rmSync(scratchDir, { recursive: true, force: true });
}
