#!/usr/bin/env node
// Adds a client build to clients.txt (pigeon spec 17.1 step 9, cutover 13.1; run by .github/workflows/clients.yml).
//
//   node append-client.mjs <claudecodeui commit, 40 hex> <web|ios> [--file CLIENTS]
//
// The commit is fetched from DE0CH/claudecodeui. A commit already listed gains the kind if it lacks it; a new one is
// inserted at its place by commit time (the file stays sorted oldest first, whatever order builds finish in), so the
// last line is always the newest build. Prints added / updated / unchanged.
//
// A web commit without the Paseo protocol package from before the Paseo apps (a ui image of the old CloudCLI web UI,
// still served within the Access session lifetime) is listed as `web-legacy`: it has no Paseo client to validate, and
// the validators skip it. Any other commit without the protocol package is refused.
//
// Exit 2 on a bad argument or a commit that cannot be a client build.

import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";

import { commitTime, harnessRoot, hasPaseo, readClientLines } from "./lib/protocol.mjs";

/** The first claudecodeui commit with `paseo/` (2026-10-26): web builds before it served the CloudCLI web UI. */
const FIRST_PASEO_TIME = "2026-10-26T16:54:50Z"; // 3968b730261d9dee137933f2837678bb6c07a672

function format(sha, kinds, time) {
  return `${sha} ${kinds.join(",").padEnd(10)} ${time}`;
}

const [sha, requested, flag, other] = process.argv.slice(2);
try {
  if (!/^[0-9a-f]{40}$/.test(sha ?? "")) throw new Error(`not a full lower-case commit hash: ${JSON.stringify(sha)}`);
  if (requested !== "web" && requested !== "ios") throw new Error(`kind must be web or ios, not ${JSON.stringify(requested)}`);
  if (flag !== undefined && (flag !== "--file" || !other)) throw new Error("the only option is --file CLIENTS");
  const file = other ?? path.join(harnessRoot, "clients.txt");
  const time = commitTime(sha); // fetches the commit
  let kind = requested;
  if (!hasPaseo(sha)) {
    if (requested !== "web" || time >= FIRST_PASEO_TIME) {
      throw new Error(`claudecodeui ${sha} has no paseo/packages/protocol: it is not a Paseo client build`);
    }
    kind = "web-legacy";
  }
  const clients = readClientLines(file); // also checks the file is well-formed and sorted
  const lines = readFileSync(file, "utf8").replace(/\n*$/, "\n").split("\n");
  lines.pop(); // the empty string after the final newline
  const listed = clients.find((client) => client.sha === sha);
  if (listed?.kinds.includes(kind)) {
    console.log(`unchanged: ${sha} is listed as ${listed.kinds.join(",")}`);
  } else if (listed) {
    if (listed.legacy || kind === "web-legacy") throw new Error(`${sha} is listed as ${listed.kinds.join(",")}; it cannot also be ${kind}`);
    const kinds = ["web", "ios"].filter((k) => k === kind || listed.kinds.includes(k));
    lines[listed.line - 1] = format(sha, kinds, listed.time);
    writeFileSync(file, `${lines.join("\n")}\n`);
    console.log(`updated: ${sha} ${kinds.join(",")}`);
  } else {
    // after every line not newer than it (ties keep arrival order), before the first newer one
    const next = clients.find((client) => client.time > time);
    const at = next ? next.line - 1 : lines.length;
    lines.splice(at, 0, format(sha, [kind], time));
    writeFileSync(file, `${lines.join("\n")}\n`);
    console.log(`added: ${sha} ${kind} ${time}${next ? ` (before ${next.sha.slice(0, 12)})` : " (newest)"}`);
  }
  readClientLines(file); // the result is well-formed and sorted
} catch (error) {
  console.error(`append-client: ${error.message ?? error}`);
  process.exit(2);
}
