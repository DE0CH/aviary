#!/usr/bin/env node
// Writes record/cloudcli-bridge-488a7736.tools.json: what the recorder (record-cloudcli-bridge.ts) fed the bridge, as
// the transcript had it: the tool calls (tool_use name and input, tool_result text and error flag) by call id, and
// conversation A's history in order (`history`: every message with the row seq the bridge gave it, its kind, role,
// text, images and files). pigeon's render-parity test (crates/pigeon/tests/parity.rs) builds magpie's rows from these
// and compares its projection with the bridge's frames in fixtures/cloudcli-bridge-488a7736.ndjson.
//
//   node record/extract-tools.mjs [--check]

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const source = path.join(here, "record-cloudcli-bridge.ts");
const target = path.join(here, "cloudcli-bridge-488a7736.tools.json");

const calls = new Map();
const lines = fs.readFileSync(source, "utf8").split("\n");
// conversation A's history: the `message({...}),` elements of the `history` array, in order; the recorder numbers its
// messages m1, m2, … and the bridge gives each its own row seq in the same order (a tool result updates its call's row)
const history = [];
const start = lines.findIndex((l) => /^const history: AnyRecord\[\] = \[$/.test(l));
if (start < 0) throw new Error("no history array in the recorder");
for (let i = start + 1; lines[i] !== "];"; i++) {
  const m = lines[i].match(/^\s*message\((\{.*\})\),\s*$/);
  if (!m) throw new Error(`history line ${i + 1} is not one message({...}) literal`);
  const o = new Function(`return (${m[1]});`)();
  const seq = history.length + 1;
  if (o.kind === "tool_use" || o.kind === "tool_result") {
    history.push({ seq, messageId: `m${seq}`, kind: o.kind, callId: o.toolId });
    continue;
  }
  history.push({
    seq,
    messageId: `m${seq}`,
    kind: o.kind,
    ...(o.role ? { role: o.role } : {}),
    content: o.content,
    ...(o.images ? { images: o.images } : {}),
    ...(o.files ? { files: o.files } : {}),
  });
}
for (const line of lines) {
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
const text = `${JSON.stringify({ source: "record/record-cloudcli-bridge.ts (node record/extract-tools.mjs)", calls: [...calls.values()], history }, null, 1)}\n`;
if (process.argv.includes("--check")) {
  if (fs.readFileSync(target, "utf8") !== text) {
    console.error(`${target} is stale: run node record/extract-tools.mjs`);
    process.exit(1);
  }
} else {
  fs.writeFileSync(target, text);
}
console.log(`${calls.size} tool calls, ${history.length} history messages`);
