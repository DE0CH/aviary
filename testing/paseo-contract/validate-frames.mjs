#!/usr/bin/env node
// Validates Paseo frame logs against client builds (pigeon spec 18.1).
//
//   node validate-frames.mjs [--clients FILE] [--commit SHA]... [--protocol-dir DIR] [--outbound-only] FILE.ndjson...
//
// Clients: every commit of clients.txt by default; `--commit` names commits instead; `--protocol-dir` checks against
// a protocol package on disk (an app's checkout: `paseo/packages/protocol`, its own build's contract check, pigeon
// spec 17.1 step 5). Every server frame (`dir: out`) must pass every validator that client applies; every client
// frame (`dir: in`) must pass the client's inbound schema, unless `--outbound-only`.
//
// Exit 0: every frame valid for every client. 1: at least one invalid frame. 2: usage or setup error.

import path from "node:path";

import { harnessRoot, protocolForCommit, protocolForDirectory, readClients } from "./lib/protocol.mjs";
import { readFrameLog, validateRecords } from "./lib/validate.mjs";

function usage(message) {
  if (message) console.error(`validate-frames: ${message}`);
  console.error(
    "usage: node validate-frames.mjs [--clients FILE] [--commit SHA]... [--protocol-dir DIR] [--outbound-only] FILE.ndjson...",
  );
  process.exit(2);
}

const args = process.argv.slice(2);
let clientsFile = null;
const commits = [];
const dirs = [];
const logs = [];
let inbound = true;
for (let i = 0; i < args.length; i++) {
  const arg = args[i];
  if (arg === "--clients") clientsFile = args[++i] ?? usage("--clients needs a file");
  else if (arg === "--commit") commits.push(args[++i] ?? usage("--commit needs a hash"));
  else if (arg === "--protocol-dir") dirs.push(args[++i] ?? usage("--protocol-dir needs a directory"));
  else if (arg === "--outbound-only") inbound = false;
  else if (arg === "-h" || arg === "--help") usage();
  else if (arg.startsWith("--")) usage(`unknown option ${arg}`);
  else logs.push(arg);
}
if (logs.length === 0) usage("no frame log given");

let failed = false;
try {
  const records = logs.flatMap((file) => readFrameLog(file));
  const targets = [];
  for (const dir of dirs) targets.push(() => protocolForDirectory(dir));
  for (const sha of commits) targets.push(() => protocolForCommit(sha));
  if (targets.length === 0 || clientsFile) {
    for (const client of readClients(clientsFile ?? path.join(harnessRoot, "clients.txt"))) {
      targets.push(() => protocolForCommit(client.sha));
    }
  }
  // one validation per protocol build (commits sharing a package tree and its runtime install validate identically)
  const byTree = new Map();
  for (const target of targets) {
    const protocol = await target();
    let group = byTree.get(protocol.id);
    if (!group) byTree.set(protocol.id, (group = { protocol, clients: [] }));
    group.clients.push(protocol.commit ? protocol.commit.slice(0, 12) : protocol.label);
  }
  for (const { protocol, clients } of byTree.values()) {
    const { checked, failures } = validateRecords(protocol, records, { inbound });
    const tag = failures.length ? "FAIL" : "ok  ";
    console.log(
      `${tag} protocol ${protocol.tree.slice(0, 10)} (zod ${protocol.runtime?.zod ?? "?"}${protocol.aot ? ` + zod-aot ${protocol.runtime?.["zod-aot"] ?? "?"}` : ""}) of ${clients.join(" ")}: ${checked.out} server frames, ${checked.in} client frames, ${failures.length} invalid`,
    );
    for (const failure of failures.slice(0, 20)) {
      console.log(`     ${failure.file}:${failure.line} ${failure.dir} ${failure.type}${failure.case ? ` (${failure.case})` : ""}: ${failure.problem}`);
    }
    if (failures.length > 20) console.log(`     … ${failures.length - 20} more`);
    if (failures.length) failed = true;
  }
} catch (error) {
  console.error(`validate-frames: ${error.stack ?? error}`);
  process.exit(2);
}
process.exit(failed ? 1 : 0);
