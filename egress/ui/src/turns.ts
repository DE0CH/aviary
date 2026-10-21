// Turns a turn's event list (claw's NDJSON events plus the server's own) into what the reply shows: text, tool
// steps, notices. Pure, so the same function serves the history load and the live stream.
import type { ChatEvent, PresentedFile, Turn } from "./api.ts";
import { budgetOf } from "./rules.ts";

export type Block =
  | { kind: "text"; text: string }
  | { kind: "tool"; id: string; name: string; input: unknown; output?: string; isError?: boolean; done: boolean; images?: ViewedImage[] }
  | { kind: "notice"; notice: "sandbox_reset" }
  | { kind: "error"; message: string };

/** An image the model looked at (read_file; claw's tool_result `images`): the step shows it small. */
export interface ViewedImage {
  path: string;
  sha256: string;
  page?: number;
}

export interface TurnView {
  id: string;
  status: Turn["status"];
  blocks: Block[];
  thinking: boolean;
  startedAt?: string;
  finishedAt?: string | null;
  error?: string | null;
  cost?: number;
  sandboxStarting: boolean;
  /** the files the agent presented (present_files), in its order: the only file cards under the answer */
  presented: PresentedFile[];
  /** the shared budget ran out (before or during the turn): its limit and who set it, for the UI's own wording */
  budget?: { limitUsd: number; ownerName: string | null };
}

export const isActive = (s: Turn["status"]) => s === "queued" || s === "running" || s === "waiting";

export function viewTurn(turn: Turn): TurnView {
  const v: TurnView = {
    id: turn.id,
    status: turn.status,
    blocks: [],
    thinking: false,
    startedAt: turn.startedAt,
    finishedAt: turn.finishedAt,
    error: turn.error,
    sandboxStarting: false,
    presented: Array.isArray(turn.presented) ? turn.presented : [],
  };
  const tools = new Map<string, Extract<Block, { kind: "tool" }>>();
  let text: Extract<Block, { kind: "text" }> | null = null;
  const events = [...turn.events].sort((a, b) => a.seq - b.seq || a.id - b.id);
  for (const e of events) {
    const d = e.data ?? {};
    switch (e.type) {
      case "turn_started":
        v.startedAt ??= e.createdAt;
        if (v.status === "queued") v.status = "running";
        break;
      case "thinking_delta":
        v.thinking = true;
        break;
      case "text_delta":
        v.thinking = false;
        if (!text) {
          text = { kind: "text", text: "" };
          v.blocks.push(text);
        }
        text.text += String(d.text ?? "");
        break;
      case "message_stop":
        v.thinking = false;
        break;
      case "tool_use": {
        v.thinking = false;
        text = null;
        const b = { kind: "tool" as const, id: String(d.id), name: String(d.name ?? ""), input: d.input, done: false };
        tools.set(b.id, b);
        v.blocks.push(b);
        break;
      }
      case "tool_result": {
        const b = tools.get(String(d.id));
        const output = typeof d.output === "string" ? d.output : JSON.stringify(d.output, null, 2);
        const images = viewedImages(d.images);
        if (b) {
          b.output = output;
          b.isError = !!d.is_error;
          b.done = true;
          if (images.length) b.images = images;
        } else {
          v.blocks.push({ kind: "tool", id: String(d.id), name: String(d.name ?? ""), input: undefined, output, isError: !!d.is_error, done: true, ...(images.length ? { images } : {}) });
        }
        text = null;
        break;
      }
      // confirmation_request / confirmation_resolved: stored by older versions (nothing asks the user anything now);
      // shown as nothing, like any other event type this view doesn't know
      case "sandbox":
        if (d.status === "starting") v.sandboxStarting = true;
        if (d.status === "ready") v.sandboxStarting = false;
        if (d.status === "reset") {
          v.sandboxStarting = false;
          v.blocks.push({ kind: "notice", notice: "sandbox_reset" });
        }
        break;
      case "error":
        text = null;
        if (budgetOf(d)) {
          v.budget = budgetOf(d);
          break;
        }
        v.blocks.push({ kind: "error", message: String(d.message ?? "") });
        break;
      case "files_presented":
        // each event carries the whole list so far; the last one wins
        if (Array.isArray(d.files)) v.presented = d.files.filter((f: any) => f && typeof f.path === "string");
        break;
      case "result":
        if (typeof d.estimated_cost === "number") v.cost = d.estimated_cost;
        break;
      case "turn_finished":
        v.status = d.status ?? "done";
        v.finishedAt = e.createdAt;
        if (d.error) v.error = d.error;
        if (budgetOf(d)) v.budget = budgetOf(d);
        v.thinking = false;
        v.sandboxStarting = false;
        break;
    }
  }
  if (!isActive(v.status)) v.thinking = false;
  return v;
}

function viewedImages(v: unknown): ViewedImage[] {
  if (!Array.isArray(v)) return [];
  return v
    .filter((x: any) => x && typeof x.path === "string" && typeof x.sha256 === "string")
    .map((x: any) => ({ path: x.path, sha256: x.sha256, ...(typeof x.page === "number" ? { page: x.page } : {}) }));
}

/** Plain text of a reply (for the copy button): the text blocks only. */
export const replyText = (v: TurnView): string =>
  v.blocks
    .filter((b): b is Extract<Block, { kind: "text" }> => b.kind === "text")
    .map((b) => b.text)
    .join("\n\n")
    .trim();

/** Merge streamed events into a turn list; ignores duplicates; creates a turn the client hasn't seen. */
export function mergeEvent(turns: Turn[], e: ChatEvent): Turn[] {
  const i = turns.findIndex((t) => t.id === e.turnId);
  if (i < 0) return [...turns, { id: e.turnId, status: "running", startedAt: e.createdAt, events: [e] }];
  const t = turns[i]!;
  if (t.events.some((x) => x.id === e.id)) return turns;
  const next = { ...t, events: [...t.events, e] };
  if (e.type === "turn_finished") next.status = e.data?.status ?? "done";
  else if (e.type === "turn_started" && next.status === "queued") next.status = "running";
  const out = turns.slice();
  out[i] = next;
  return out;
}
