#!/usr/bin/env node
// Writes record/cloudcli-bridge-488a7736.tools.json: the tool calls the recorder (record-cloudcli-bridge.ts) fed the
// bridge, as the transcript had them (tool_use name and input, tool_result text and error flag), by call id. pigeon's
// render-parity test (crates/pigeon/tests/parity.rs) builds magpie's rows from these and compares its projection with
// the bridge's frames in fixtures/cloudcli-bridge-488a7736.ndjson.
//
//   node record/extract-tools.mjs [--check]

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const source = path.join(here, "record-cloudcli-bridge.ts");
const target = path.join(here, "cloudcli-bridge-488a7736.tools.json");

const calls = new Map();
for (const line of fs.readFileSync(source, "utf8").split("\n")) {
  // the recorder writes each tool message as one object literal on one line
  const m =
    line.match(/^\s*message\((\{ kind: 'tool_(?:use|result)'.*\})\),\s*$/) ??
    line.match(/^live\(\w+, (\{ kind: 'tool_(?:use|result)'.*\}), \w+\);\s*$/);
  if (!m) continue;
  const o = new Function(`return (${m[1]});`)();
  const call = calls.get(o.toolId) ?? { callId: o.toolId };
  calls.set(o.toolId, call);
  if (o.kind === "tool_use") {
    call.name = o.toolName;
    call.input = o.toolInput;
  } else {
    call.result = { text: o.toolResult.content, isError: Boolean(o.toolResult.isError) };
  }
}
const text = `${JSON.stringify({ source: "record/record-cloudcli-bridge.ts (node record/extract-tools.mjs)", calls: [...calls.values()] }, null, 1)}\n`;
if (process.argv.includes("--check")) {
  if (fs.readFileSync(target, "utf8") !== text) {
    console.error(`${target} is stale: run node record/extract-tools.mjs`);
    process.exit(1);
  }
} else {
  fs.writeFileSync(target, text);
}
console.log(`${calls.size} tool calls`);
