// Checking frames against one client build's validators (lib/protocol.mjs).

import { readFileSync } from "node:fs";

function issues(error, limit = 5) {
  const list = error?.issues ?? [];
  const shown = list.slice(0, limit).map((issue) => {
    const message = String(issue.message ?? "");
    return `${(issue.path ?? []).join(".") || "(root)"}: ${message.length > 240 ? `${message.slice(0, 240)}…` : message}`;
  });
  if (list.length > limit) shown.push(`… ${list.length - limit} more`);
  return shown.join("; ") || String(error);
}

function knownStatuses(schema) {
  if (!schema?.options) return new Set();
  const out = new Set();
  for (const option of schema.options) {
    const tag = option.shape?.status;
    for (const value of tag?.values ?? []) out.add(value);
  }
  return out;
}

const statusCache = new WeakMap();

/** What the client would do with a frame the server sent: `null` when every validator accepts it, else why not. */
export function outboundProblem(protocol, frame) {
  const problems = [];
  const zod = protocol.outbound.safeParse(frame);
  if (!zod.success) problems.push(`WSOutboundMessageSchema: ${issues(zod.error)}`);
  if (protocol.aot) {
    const aot = protocol.aot(frame);
    if (!aot.success) problems.push(`validateWSOutboundMessage (zod-aot): ${issues(aot.error)}`);
    if (aot.success !== zod.success) problems.push("zod and zod-aot disagree on this frame");
  }
  const status = frame?.type === "session" && frame.message?.type === "status" ? frame.message.payload?.status : undefined;
  // the client reads server_info with its own schema (parseServerInfoStatusPayload); a failure there is a dead socket
  if (status === "server_info" && protocol.serverInfo) {
    const parsed = protocol.serverInfo.safeParse(frame.message.payload);
    if (!parsed.success) problems.push(`ServerInfoStatusPayloadSchema: ${issues(parsed.error)}`);
  }
  if (protocol.knownStatus && status !== undefined) {
    let statuses = statusCache.get(protocol.knownStatus);
    if (!statuses) statusCache.set(protocol.knownStatus, (statuses = knownStatuses(protocol.knownStatus)));
    if (statuses.has(status)) {
      const parsed = protocol.knownStatus.safeParse(frame.message.payload);
      if (!parsed.success) problems.push(`KnownStatusPayloadSchema (${status}): ${issues(parsed.error)}`);
    }
  }
  return problems.length ? problems.join(" | ") : null;
}

/** Whether a frame a driver sends is one the server-side schema accepts: `null`, or why not. */
export function inboundProblem(protocol, frame) {
  const parsed = protocol.inbound.safeParse(frame);
  return parsed.success ? null : `WSInboundMessageSchema: ${issues(parsed.error)}`;
}

/** The type a frame is reported under: `session/<message.type>` or its top-level type. */
export function frameType(frame) {
  if (frame?.type === "session") return `session/${frame.message?.type ?? "?"}`;
  return String(frame?.type ?? "?");
}

/**
 * Reads a frame log (NDJSON). A line is `{"dir": "out" | "in", "frame": {...}, "case"?: "...", "expect"?: "invalid"}`;
 * a line without `dir` is a bare frame the server sent. `expect: invalid` marks a frame that is outside the schema on
 * purpose (a test of the server's dispatch rules): it must fail validation.
 */
export function readFrameLog(file) {
  const records = [];
  for (const [index, raw] of readFileSync(file, "utf8").split("\n").entries()) {
    if (!raw.trim()) continue;
    let value;
    try {
      value = JSON.parse(raw);
    } catch (error) {
      throw new Error(`${file}:${index + 1}: not JSON (${error.message})`);
    }
    const record =
      value && typeof value === "object" && (value.dir === "in" || value.dir === "out") && "frame" in value
        ? { dir: value.dir, frame: value.frame, case: value.case ?? null, expectInvalid: value.expect === "invalid" }
        : { dir: "out", frame: value, case: null, expectInvalid: false };
    records.push({ ...record, file, line: index + 1 });
  }
  return records;
}

/** Validates every record against one protocol; returns `{checked: {out, in}, failures: [...]}`. */
export function validateRecords(protocol, records, { inbound = true } = {}) {
  const checked = { out: 0, in: 0 };
  const failures = [];
  for (const record of records) {
    if (record.dir === "in" && !inbound) continue;
    checked[record.dir] += 1;
    const found = record.dir === "out" ? outboundProblem(protocol, record.frame) : inboundProblem(protocol, record.frame);
    // a record marked `"expect": "invalid"` (a frame outside the schema on purpose) must be rejected
    const problem = record.expectInvalid ? (found ? null : "marked invalid, but every validator accepts it") : found;
    if (problem) failures.push({ ...record, type: frameType(record.frame), problem });
  }
  return { checked, failures };
}
