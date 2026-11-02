#!/usr/bin/env node
// Adds a client build to clients.txt (pigeon spec 17.1 step 9; run by .github/workflows/clients.yml).
//
//   node append-client.mjs <claudecodeui commit, 40 hex> <web|ios> [--file CLIENTS]
//
// The commit is fetched from DE0CH/claudecodeui and must contain the Paseo protocol package. A commit already listed
// gains the kind if it lacks it; a new one is appended (the file stays oldest first). Prints added / updated /
// unchanged. Exit 2 on a bad argument or a commit that is not a Paseo client build.

import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";

import { commitDate, harnessRoot, protocolTree, readClients } from "./lib/protocol.mjs";

const [sha, kind, flag, other] = process.argv.slice(2);
try {
  if (!/^[0-9a-f]{40}$/.test(sha ?? "")) throw new Error(`not a full lower-case commit hash: ${JSON.stringify(sha)}`);
  if (kind !== "web" && kind !== "ios") throw new Error(`kind must be web or ios, not ${JSON.stringify(kind)}`);
  protocolTree(sha); // fetches it; throws when it has no paseo/packages/protocol
  if (flag !== undefined && (flag !== "--file" || !other)) throw new Error("the only option is --file CLIENTS");
  const file = other ?? path.join(harnessRoot, "clients.txt");
  const listed = readClients(file).find((client) => client.sha === sha);
  const lines = readFileSync(file, "utf8").split("\n");
  if (listed?.kinds.includes(kind)) {
    console.log(`unchanged: ${sha} is listed as ${listed.kinds.join(",")}`);
  } else if (listed) {
    const kinds = ["web", "ios"].filter((k) => k === kind || listed.kinds.includes(k)).join(",");
    lines[listed.line - 1] = lines[listed.line - 1].replace(/^(\S+)\s+\S+\s*/, (_, s) => `${s} ${kinds.padEnd(8)} `);
    writeFileSync(file, lines.join("\n"));
    console.log(`updated: ${sha} ${kinds}`);
  } else {
    const body = lines.join("\n").replace(/\n*$/, "\n");
    writeFileSync(file, `${body}${sha} ${kind.padEnd(8)} ${commitDate(sha)}\n`);
    console.log(`added: ${sha} ${kind}`);
  }
} catch (error) {
  console.error(`append-client: ${error.message ?? error}`);
  process.exit(2);
}
