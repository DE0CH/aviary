// tools' own configuration (aviary platform spec 17.2 tools item 1): `/etc/tools/tools.json` (ConfigMap `tools-config`),
// what the old shared config held under `tools`: the providers (exa, tikhub, anyapi), the instance's page capture and
// the upstream MCP servers. Secrets come from the environment: a provider names its variable (`keyEnv`, `tokenEnv`),
// an upstream header may name one as ${VAR} (expanded per call, checked once at start).
import { readFileSync } from "node:fs";
import { expandEnv } from "../common/config.ts";

export const TOOLS_CONFIG_PATH = "/etc/tools/tools.json";

export interface UpstreamMcp {
  prefix: string;
  url: string;
  headers: Record<string, string>;
  /**
   * The upstream's tools (their own names, without the prefix) the model may use: read-only ones only. Default deny: a
   * tool not named here, or one the upstream marks readOnlyHint false or destructiveHint true, is never offered.
   */
  tools?: string[];
  /** The identity the tool list is read as (`${USER_EMAIL}` while listing); each call goes as the sandbox's owner. */
  listAs: string;
}

export interface ToolsConfig {
  exa?: { keyEnv: string; baseUrl?: string };
  tikhub?: { keyEnv: string; baseUrl?: string; platforms?: string[] };
  /**
   * AnyAPI (getanyapi.com): one key for its catalog of read-only scraping and data APIs (Facebook, LinkedIn, Google,
   * commerce, …); the anyapi_* tools. `denySkus`: catalog ids never run here (default none: the catalog only reads).
   */
  anyapi?: { keyEnv: string; baseUrl?: string; denySkus?: string[] };
  /**
   * The instance's own page capture, asked first by web_fetch (unless the model asks for the page `original`): POST
   * {url} with Bearer $<tokenEnv> → {kind: "cache"|"capture"|"none", url, title, text, author, source, published,
   * captured_at, excerpt, dashboard_url}. YYC: its news monitor's copy of an article, or its capture of a site it reads.
   */
  capture?: { url: string; tokenEnv: string; name: string };
  upstreams: UpstreamMcp[];
}

const EMAIL = /^[^\s@]+@[^\s@]+$/;

export function parseToolsConfig(raw: unknown): ToolsConfig {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) throw new Error("tools.json: not an object");
  const c = raw as Record<string, any>;
  const str = (path: string, v: unknown) => {
    if (typeof v !== "string" || !v) throw new Error(`tools.json: ${path} must be a non-empty string`);
    return v;
  };
  const strList = (path: string, v: unknown): string[] | undefined => {
    if (v === undefined) return undefined;
    if (!Array.isArray(v) || v.some((x) => typeof x !== "string")) throw new Error(`tools.json: ${path} must be a list of strings`);
    return v as string[];
  };
  const out: ToolsConfig = { upstreams: [] };
  if (c.exa != null) out.exa = { keyEnv: str("exa.keyEnv", c.exa.keyEnv), ...(c.exa.baseUrl ? { baseUrl: str("exa.baseUrl", c.exa.baseUrl) } : {}) };
  if (c.tikhub != null) {
    const platforms = strList("tikhub.platforms", c.tikhub.platforms);
    out.tikhub = { keyEnv: str("tikhub.keyEnv", c.tikhub.keyEnv), ...(c.tikhub.baseUrl ? { baseUrl: str("tikhub.baseUrl", c.tikhub.baseUrl) } : {}), ...(platforms ? { platforms } : {}) };
  }
  if (c.anyapi != null) {
    const denySkus = strList("anyapi.denySkus", c.anyapi.denySkus);
    out.anyapi = { keyEnv: str("anyapi.keyEnv", c.anyapi.keyEnv), ...(c.anyapi.baseUrl ? { baseUrl: str("anyapi.baseUrl", c.anyapi.baseUrl) } : {}), ...(denySkus ? { denySkus } : {}) };
  }
  if (c.capture != null) out.capture = { url: str("capture.url", c.capture.url), tokenEnv: str("capture.tokenEnv", c.capture.tokenEnv), name: str("capture.name", c.capture.name) };
  if (c.upstreams !== undefined && !Array.isArray(c.upstreams)) throw new Error("tools.json: upstreams must be a list");
  const prefixes = new Set<string>();
  for (const [i, u] of ((c.upstreams ?? []) as any[]).entries()) {
    const at = `upstreams[${i}]`;
    if (!u || typeof u !== "object") throw new Error(`tools.json: ${at} must be an object`);
    const prefix = str(`${at}.prefix`, u.prefix);
    if (prefixes.has(prefix)) throw new Error(`tools.json: ${at}.prefix ${prefix} is listed twice`);
    prefixes.add(prefix);
    const listAs = str(`${at}.listAs`, u.listAs).trim().toLowerCase();
    if (!EMAIL.test(listAs)) throw new Error(`tools.json: ${at}.listAs must be an email`);
    const headers = u.headers ?? {};
    if (typeof headers !== "object" || Array.isArray(headers) || Object.values(headers).some((v) => typeof v !== "string"))
      throw new Error(`tools.json: ${at}.headers must map names to strings`);
    const tools = strList(`${at}.tools`, u.tools);
    out.upstreams.push({ prefix, url: str(`${at}.url`, u.url), headers, listAs, ...(tools ? { tools } : {}) });
  }
  return out;
}

export function loadToolsConfig(path = TOOLS_CONFIG_PATH): ToolsConfig {
  return parseToolsConfig(JSON.parse(readFileSync(path, "utf8")));
}

/** Every environment variable the config names, set (a start failure names the first missing one, never a value). */
export function checkConfigEnv(c: ToolsConfig, env: Record<string, string | undefined>): void {
  const need = (name: string, what: string) => {
    if (!env[name]) throw new Error(`environment variable ${name} is not set (${what})`);
  };
  if (c.exa) need(c.exa.keyEnv, "exa.keyEnv");
  if (c.tikhub) need(c.tikhub.keyEnv, "tikhub.keyEnv");
  if (c.anyapi) need(c.anyapi.keyEnv, "anyapi.keyEnv");
  if (c.capture) need(c.capture.tokenEnv, "capture.tokenEnv");
  for (const u of c.upstreams) for (const v of Object.values(u.headers)) expandEnv(v, { ...env, USER_EMAIL: u.listAs });
}
