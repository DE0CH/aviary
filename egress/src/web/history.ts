// The conversation's model-facing history (claw's session JSONL, table claw_sessions; docs/CHAT.md "History"):
// durable at every step, valid to resume whatever way the last turn ended.
//
// claw (host mode) appends each completed step to its session file as it happens (the user's prompt, each assistant
// message with its signed thinking and tool_use blocks, each tool_result); the turn runner copies the file to
// Postgres after every step. A turn that did not end with a final reply (cancelled, failed, the server restarted)
// leaves an "open" history: closeHistory() makes it valid and tells the model what happened, the same way for all
// three (at the end of the turn, or at the start of the next one when the end never ran):
//   - any tool_use without a tool_result gets a synthetic one ("interrupted before this finished");
//   - the reply streamed so far that claw never stored (partial text) is kept as an assistant message (partial
//     thinking is dropped: it has no signature);
//   - a user-side note says the previous response was interrupted.

export const NOTES = {
  cancelled: "[The user interrupted the previous response]",
  error: "[The previous response stopped early because of an error]",
  restarted: "[The previous response was interrupted because the server restarted]",
} as const;
export type InterruptKind = keyof typeof NOTES;

export const INTERRUPTED_TOOL_RESULT = "interrupted by the user before this finished";
const INTERRUPTED_TOOL_RESULT_OTHER = "interrupted before this finished";

type Block = Record<string, any>;
interface Msg {
  role: string;
  blocks: Block[];
  [k: string]: unknown;
}
interface Line {
  raw: string;
  rec: Record<string, any> | null;
}

function parse(content: string): Line[] {
  const out: Line[] = [];
  for (const raw of content.split("\n")) {
    if (!raw.trim()) continue;
    try {
      const rec = JSON.parse(raw);
      out.push({ raw, rec: rec && typeof rec === "object" ? rec : null });
    } catch {
      // a line claw was writing when it was stopped: incomplete, never stored
    }
  }
  return out;
}

const msgOf = (l: Line): Msg | null => (l.rec?.type === "message" && l.rec.message && Array.isArray(l.rec.message.blocks) ? (l.rec.message as Msg) : null);
const isNote = (m: Msg) => m.role === "user" && m.blocks.length === 1 && m.blocks[0]?.type === "text" && Object.values(NOTES).includes(m.blocks[0].text);
/** A message after which the history is complete: a final reply (assistant, no tool_use) or an interruption note. */
const closes = (m: Msg) => isNote(m) || (m.role === "assistant" && !m.blocks.some((b) => b.type === "tool_use") && m.blocks.some((b) => b.type === "text"));

/** Whether the history ends in the middle of a turn. */
export function isOpen(content: string): boolean {
  const msgs = parse(content).map(msgOf).filter((m): m is Msg => !!m);
  const last = msgs.at(-1);
  return !!last && !closes(last);
}

const record = (message: Msg) => JSON.stringify({ type: "message", message });

/**
 * Closes an open history (see the top of this file); a closed one comes back unchanged except for dropping an
 * incomplete last line. Idempotent.
 */
export function closeHistory(content: string, o: { kind: InterruptKind; partialText?: string }): string {
  const lines = parse(content);
  const out = lines.map((l) => l.raw);
  const msgs = lines.map(msgOf).filter((m): m is Msg => !!m);
  const last = msgs.at(-1);
  if (!last || closes(last)) return out.length ? out.join("\n") + "\n" : "";

  // tool_use blocks with no tool_result (claw stores an assistant message before running its tools)
  const answered = new Set<string>();
  for (const m of msgs) for (const b of m.blocks) if (b.type === "tool_result") answered.add(String(b.tool_use_id));
  const why = o.kind === "cancelled" ? INTERRUPTED_TOOL_RESULT : INTERRUPTED_TOOL_RESULT_OTHER;
  for (const m of msgs) {
    for (const b of m.blocks) {
      if (b.type !== "tool_use" || answered.has(String(b.id))) continue;
      answered.add(String(b.id));
      out.push(record({ role: "tool", blocks: [{ type: "tool_result", tool_use_id: String(b.id), tool_name: String(b.name ?? ""), output: why, is_error: true }] }));
    }
  }

  // the reply streamed so far, unless claw stored it already (it is the last assistant message's text then)
  const partial = (o.partialText ?? "").trim();
  if (partial) {
    const tail = msgs.at(-1)!;
    const stored = tail.role === "assistant" ? tail.blocks.filter((b) => b.type === "text").map((b) => String(b.text ?? "")).join("").trim() : "";
    if (stored !== partial) out.push(record({ role: "assistant", blocks: [{ type: "text", text: o.partialText!.trimEnd() }] }));
  }
  out.push(record({ role: "user", blocks: [{ type: "text", text: NOTES[o.kind] }] }));
  return out.join("\n") + "\n";
}

/**
 * The history up to its last complete point (a final reply or an interruption note): what a share snapshot keeps
 * while a turn is still running. Lines that are not messages (session_meta, compaction, prompt history) are kept.
 */
export function completedPrefix(content: string): string {
  const lines = parse(content);
  let cut = -1;
  lines.forEach((l, i) => {
    const m = msgOf(l);
    if (m && closes(m)) cut = i;
  });
  const kept = lines.filter((l, i) => i <= cut || !msgOf(l)).map((l) => l.raw);
  return kept.length ? kept.join("\n") + "\n" : "";
}
