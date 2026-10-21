// The MCP server side, by hand (MCP 2025-06-18, Streamable HTTP, stateless, JSON responses): JSON-RPC 2.0 requests
// in one POST (a single message or a batch), answers as one JSON body; notifications get 202 and no body.
import type { ToolCtx, ToolDef, ToolResult } from "./types.ts";
import { ToolInputError } from "./types.ts";

export const PROTOCOL_VERSIONS = ["2025-06-18", "2025-03-26", "2024-11-05"];

export interface JsonRpcRequest {
  jsonrpc: "2.0";
  id?: string | number | null;
  method: string;
  params?: Record<string, unknown>;
}

export type JsonRpcResponse =
  | { jsonrpc: "2.0"; id: string | number | null; result: unknown }
  | { jsonrpc: "2.0"; id: string | number | null; error: { code: number; message: string; data?: unknown } };

export const ERR = { parse: -32700, invalidRequest: -32600, method: -32601, params: -32602, internal: -32603 };

export interface McpHandlers {
  serverName: string;
  serverVersion: string;
  instructions?: string;
  /** the tools on offer */
  tools(): ToolDef[];
  /** Runs a tool and logs it; returns the MCP result (tool errors are results with isError, not JSON-RPC errors). */
  call(tool: ToolDef, args: Record<string, unknown>, ctx: ToolCtx): Promise<ToolResult>;
}

export function toolListEntry(t: ToolDef): Record<string, unknown> {
  return {
    name: t.name,
    ...(t.title ? { title: t.title } : {}),
    description: t.description,
    inputSchema: t.inputSchema,
    ...(t.outputSchema ? { outputSchema: t.outputSchema } : {}),
    ...(t.annotations ? { annotations: t.annotations } : {}),
  };
}

export async function handleMessage(h: McpHandlers, msg: unknown, ctx: ToolCtx): Promise<JsonRpcResponse | null> {
  if (!msg || typeof msg !== "object" || (msg as JsonRpcRequest).jsonrpc !== "2.0" || typeof (msg as JsonRpcRequest).method !== "string") {
    // a response or garbage: a response from the client (to a request we never send) is ignored
    if (msg && typeof msg === "object" && ("result" in msg || "error" in msg)) return null;
    return { jsonrpc: "2.0", id: null, error: { code: ERR.invalidRequest, message: "invalid JSON-RPC request" } };
  }
  const m = msg as JsonRpcRequest;
  const isNotification = m.id === undefined;
  const ok = (result: unknown): JsonRpcResponse => ({ jsonrpc: "2.0", id: m.id ?? null, result });
  const fail = (code: number, message: string): JsonRpcResponse => ({ jsonrpc: "2.0", id: m.id ?? null, error: { code, message } });
  if (isNotification) return null; // notifications/initialized, notifications/cancelled: nothing to do (stateless)
  switch (m.method) {
    case "initialize": {
      const asked = String(m.params?.protocolVersion ?? "");
      return ok({
        protocolVersion: PROTOCOL_VERSIONS.includes(asked) ? asked : PROTOCOL_VERSIONS[0],
        capabilities: { tools: { listChanged: false } },
        serverInfo: { name: h.serverName, version: h.serverVersion },
        ...(h.instructions ? { instructions: h.instructions } : {}),
      });
    }
    case "ping":
      return ok({});
    case "tools/list":
      return ok({ tools: h.tools().map(toolListEntry) });
    case "tools/call": {
      const name = m.params?.name;
      const args = (m.params?.arguments ?? {}) as Record<string, unknown>;
      if (typeof name !== "string") return fail(ERR.params, "tools/call needs a tool name");
      if (typeof args !== "object" || Array.isArray(args)) return fail(ERR.params, "arguments must be an object");
      const tool = h.tools().find((t) => t.name === name);
      if (!tool) return fail(ERR.params, `unknown tool ${name}`);
      try {
        const r = await h.call(tool, args, ctx);
        const { costUsd: _cost, ...result } = r;
        return ok(result);
      } catch (e) {
        if (e instanceof ToolInputError) return ok({ content: [{ type: "text", text: `Invalid input: ${e.message}` }], isError: true });
        return ok({ content: [{ type: "text", text: `The tool failed: ${(e as Error).message}` }], isError: true });
      }
    }
    case "resources/list":
      return ok({ resources: [] });
    case "prompts/list":
      return ok({ prompts: [] });
    default:
      return fail(ERR.method, `method ${m.method} is not supported`);
  }
}
