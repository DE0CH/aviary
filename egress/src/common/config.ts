// Instance configuration for the chat runtime (docs/CHAT.md, "Config"): a JSON file (JARVIS_CONFIG, default
// /etc/jarvis/config.json) for everything that is not a secret. Secrets come from environment variables, and a
// string in the config may name one as ${VAR} (expanded where it is used, never at load: expandEnv()).
import { readFileSync } from "node:fs";

export interface ModelEntry {
  id: string;
  label: string;
}

export interface EgressRoute {
  name: string;
  upstream: string;
  auth: "turn" | "sandbox";
  inject: Record<string, string>;
  allowPaths?: string[];
}

export interface UpstreamMcp {
  prefix: string;
  url: string;
  headers: Record<string, string>;
}

export interface ChatConfig {
  product: { name: string; locale: string; logo?: string };
  publicUrl: string;
  access: { teamDomain: string; audience: string };
  users: string[];
  admins: string[];
  models: ModelEntry[];
  defaultModel: string;
  titleModel: string;
  systemPromptFile: string;
  sandbox: {
    image: string;
    namespace: string;
    runtimeClass: string;
    idleMinutes: number;
    maxLifetimeHours: number;
    maxActive: number;
    cpu: string;
    memory: string;
  };
  egress: { defaultAllowedHosts: string[]; routes: EgressRoute[]; cacheGiB: number };
  tools: {
    exa?: { keyEnv: string };
    tikhub?: { keyEnv: string };
    wisers?: { url: string; accessClientIdEnv: string; accessClientSecretEnv: string };
    upstreams: UpstreamMcp[];
    confirm: string[];
  };
  s3: { endpoint: string; region: string; bucket: string; prefix: string };
}

const DEFAULTS = {
  sandbox: { namespace: "jarvis-sandbox", runtimeClass: "gvisor", idleMinutes: 30, maxLifetimeHours: 8, maxActive: 6, cpu: "1", memory: "1Gi" },
  egress: { defaultAllowedHosts: [] as string[], routes: [] as EgressRoute[], cacheGiB: 5 },
  tools: { upstreams: [] as UpstreamMcp[], confirm: [] as string[] },
};

export function parseConfig(raw: unknown): ChatConfig {
  if (!raw || typeof raw !== "object") throw new Error("config: not an object");
  const c = raw as Record<string, any>;
  const need = (path: string, v: unknown, type: string) => {
    if (typeof v !== type || (type === "string" && !v)) throw new Error(`config: ${path} must be a non-empty ${type}`);
  };
  need("product.name", c.product?.name, "string");
  need("publicUrl", c.publicUrl, "string");
  need("access.teamDomain", c.access?.teamDomain, "string");
  need("access.audience", c.access?.audience, "string");
  need("defaultModel", c.defaultModel, "string");
  need("systemPromptFile", c.systemPromptFile, "string");
  need("sandbox.image", c.sandbox?.image, "string");
  if (!Array.isArray(c.users) || !c.users.length) throw new Error("config: users must list at least one email");
  if (!Array.isArray(c.models) || !c.models.length) throw new Error("config: models must not be empty");
  if (!c.models.some((m: ModelEntry) => m.id === c.defaultModel)) throw new Error("config: defaultModel must be one of models");
  const lower = (xs: unknown[]) => xs.map((x) => String(x).trim().toLowerCase());
  return {
    product: { locale: "en", ...c.product },
    publicUrl: String(c.publicUrl).replace(/\/$/, ""),
    access: c.access,
    users: lower(c.users),
    admins: lower(c.admins ?? []),
    models: c.models,
    defaultModel: c.defaultModel,
    titleModel: c.titleModel ?? c.defaultModel,
    systemPromptFile: c.systemPromptFile,
    sandbox: { ...DEFAULTS.sandbox, ...c.sandbox },
    egress: { ...DEFAULTS.egress, ...c.egress },
    tools: { ...DEFAULTS.tools, ...c.tools },
    s3: { prefix: "", ...c.s3 },
  };
}

export function loadConfig(path = process.env.JARVIS_CONFIG || "/etc/jarvis/config.json"): ChatConfig {
  return parseConfig(JSON.parse(readFileSync(path, "utf8")));
}

/** Expand ${VAR} from `vars` (default process.env); an unset variable is an error, never an empty string. */
export function expandEnv(s: string, vars: Record<string, string | undefined> = process.env): string {
  return s.replace(/\$\{([A-Z0-9_]+)\}/g, (_, name: string) => {
    const v = vars[name];
    if (v === undefined || v === "") throw new Error(`environment variable ${name} is not set`);
    return v;
  });
}

export function requireEnv(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`environment variable ${name} is not set`);
  return v;
}
