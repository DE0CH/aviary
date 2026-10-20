// What every tool of the tools server is (docs/CHAT.md, "tools"): an MCP tool definition plus the function that runs it
// for one turn. Results are MCP content; costUsd (when the provider reports one) goes into tool_calls.
import type { ChatConfig } from "../common/config.ts";
import type { Db, Listener } from "../common/db.ts";

export interface TurnRef {
  id: string;
  conversation_id: string;
  owner: string;
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
  config: ChatConfig;
  db: Db;
  listener: Listener;
  /** Base URL of the web role inside the cluster (POST /internal/events). */
  webUrl: string;
  env: Record<string, string | undefined>;
  fetch: typeof fetch;
  /** How long a confirmation waits for the staff member (default 15 minutes). */
  confirmTimeoutMs: number;
  /** Address check for web_fetch's direct path (tests reach local fakes through it). */
  resolve?: (host: string) => Promise<{ address: string; family: number }>;
  /** Ports web_fetch may reach (default 80, 443, 8080, 8443). */
  fetchPorts?: string[] | "any";
}

export interface ToolCtx {
  turn: TurnRef;
  /** The turn token, to post events to web as this turn. */
  token: string;
  deps: Deps;
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
