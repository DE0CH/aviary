// What every tool of the tools server is (docs/CHAT.md, "tools"): a tool definition (name, description, JSON Schema of
// its arguments) plus the function that runs it for one call. A result is content (text the reader reads, pictures)
// and/or JSON (structuredContent), which server.ts `answer` turns into the HTTP answer; costUsd (when the provider
// reports one) goes into tool_calls.
import type { ToolsConfig } from "./config.ts";

/** Who is calling: Claude Code in a sandbox, as the gateway verified it (its pair bearer plus the `Aviary-Sandbox-*`
 * headers, aviary platform spec 3.11.3). */
export interface Caller {
  sandboxId: string;
  conversationId: string;
  /** the owner's trimmed, lowercased email (`Aviary-Sandbox-Person`): ${USER_EMAIL} in upstream headers */
  person: string;
  /** `Aviary-Request` */
  request: string | null;
}

export type Content = { type: "text"; text: string } | { type: "image"; data: string; mimeType: string } | Record<string, unknown>;

export interface ToolResult {
  content: Content[];
  isError?: boolean;
  structuredContent?: unknown;
  costUsd?: number;
}

export interface Annotations {
  title?: string;
  readOnlyHint?: boolean;
  destructiveHint?: boolean;
  idempotentHint?: boolean;
  openWorldHint?: boolean;
}

export interface ToolDef {
  name: string;
  title?: string;
  description: string;
  inputSchema: Record<string, unknown>;
  outputSchema?: Record<string, unknown>;
  annotations?: Annotations;
  run(args: Record<string, unknown>, ctx: ToolCtx): Promise<ToolResult>;
}

export interface Deps {
  config: ToolsConfig;
  env: Record<string, string | undefined>;
  fetch: typeof fetch;
  /** Address check for web_fetch's direct path (tests reach local fakes through it). */
  resolve?: (host: string) => Promise<{ address: string; family: number }>;
  /** Ports web_fetch may reach (default 80, 443, 8080, 8443). */
  fetchPorts?: string[] | "any";
}

export interface ToolCtx {
  caller: Caller;
  deps: Deps;
  /** Aborted when the drain cuts the call (upstream calls stop waiting on their upstream). */
  signal?: AbortSignal;
}

export const text = (t: string, isError = false): ToolResult => ({ content: [{ type: "text", text: t }], ...(isError ? { isError } : {}) });

/** A tool's input: a string argument, trimmed; throws a readable error for a missing required one. */
export function str(args: Record<string, unknown>, key: string, required = false): string {
  const v = args[key];
  if (v === undefined || v === null || v === "") {
    if (required) throw new ToolInputError(`"${key}" is required`);
    return "";
  }
  if (typeof v !== "string") throw new ToolInputError(`"${key}" must be a string`);
  return v.trim();
}

export function int(args: Record<string, unknown>, key: string, def: number, min: number, max: number): number {
  const v = args[key];
  if (v === undefined || v === null || v === "") return def;
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new ToolInputError(`"${key}" must be a number`);
  return Math.min(max, Math.max(min, Math.round(n)));
}

export class ToolInputError extends Error {}

/** The most of one outside answer (a provider's, an upstream tool's, the capture's) that tools takes: an answer is
 * copied several times while it is parsed and written out again, and the pod has 128 Mi. */
export const ANSWER_MAX = 8 * 1024 * 1024;

/** A fetch answer's text, read piece by piece and given up at `max` bytes (never `await r.text()`: that takes whatever
 * the other side sends). Over the bound is the caller's to fix, in words the agent can act on. */
export async function readCapped(r: Response, what: string, max = ANSWER_MAX): Promise<string> {
  if (!r.body) return "";
  const pieces: Uint8Array[] = [];
  let n = 0;
  const reader = r.body.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    n += value.length;
    if (n > max) {
      await reader.cancel().catch(() => {});
      throw new ToolInputError(`${what} answered more than ${Math.round(max / (1024 * 1024))} MB, which is too much to pass on: ask for less (fewer rows, a narrower search).`);
    }
    pieces.push(value);
  }
  return Buffer.concat(pieces).toString("utf8");
}
