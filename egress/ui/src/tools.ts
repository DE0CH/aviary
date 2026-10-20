// Friendly names for tool steps. MCP tools arrive as mcp__tools__<name>; claw's own tools by their names.
import { t } from "./i18n.ts";

export function bareToolName(name: string): string {
  const m = /^mcp__[^_]+(?:_[^_]+)*?__(.+)$/.exec(name);
  return m ? m[1]! : name;
}

const str = (v: unknown): string => (typeof v === "string" ? v : "");

function host(u: string): string {
  try {
    return new URL(u).hostname.replace(/^www\./, "");
  } catch {
    return u;
  }
}

function baseName(p: string): string {
  const parts = p.split("/").filter(Boolean);
  return parts[parts.length - 1] ?? p;
}

/** [label, detail] for a tool step chip. */
export function describeTool(name: string, input: any): [string, string] {
  const n = bareToolName(name);
  const s = t().tool;
  const i = input && typeof input === "object" ? input : {};
  switch (n) {
    case "web_search":
      return [s.web_search, str(i.query)];
    case "news_search":
      return [s.news_search, str(i.query)];
    case "social_search":
      return [s.social_search, [str(i.platform), str(i.query)].filter(Boolean).join(" · ")];
    case "web_fetch":
      return [s.web_fetch, host(str(i.url))];
    case "bash":
      return [s.bash, firstLine(str(i.description) || str(i.command))];
    case "read_file":
    case "read":
      return [s.read_file, baseName(str(i.path) || str(i.file_path))];
    case "write_file":
    case "write":
      return [s.write_file, baseName(str(i.path) || str(i.file_path))];
    case "edit_file":
    case "edit":
      return [s.edit_file, baseName(str(i.path) || str(i.file_path))];
    case "glob_search":
    case "glob":
      return [s.glob_search, str(i.pattern)];
    case "grep_search":
    case "grep":
      return [s.grep_search, str(i.pattern)];
    case "request_network_access":
      return [s.request_network_access, str(i.host)];
  }
  if (n.startsWith("yyc_")) return [s.yyc, str(i.query) || str(i.stream) || str(i.profile)];
  return [s.other, n];
}

function firstLine(s: string): string {
  const l = s.split("\n")[0] ?? "";
  return l.length > 80 ? l.slice(0, 79) + "…" : l;
}

export type ToolIcon = "search" | "news" | "social" | "page" | "terminal" | "file" | "data" | "globe" | "tool";

export function toolIcon(name: string): ToolIcon {
  const n = bareToolName(name);
  if (n === "web_search") return "search";
  if (n === "news_search") return "news";
  if (n === "social_search") return "social";
  if (n === "web_fetch") return "page";
  if (n === "bash") return "terminal";
  if (/^(read|write|edit|glob|grep)/.test(n)) return "file";
  if (n.startsWith("yyc_")) return "data";
  if (n === "request_network_access") return "globe";
  return "tool";
}
