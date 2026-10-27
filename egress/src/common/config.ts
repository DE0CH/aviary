// Instance configuration for the chat runtime (docs/CHAT.md, "Config"): a JSON file (JARVIS_CONFIG, default
// /etc/jarvis/config.json) for everything that is not a secret. Secrets come from environment variables, and a
// string in the config may name one as ${VAR} (expanded where it is used, never at load: expandEnv()).
import { readFileSync } from "node:fs";

/** OpenRouter's reasoning levels; "none" turns thinking off. */
export type ReasoningEffort = "none" | "low" | "medium" | "high" | "max";
const REASONING_EFFORTS: ReadonlySet<string> = new Set(["none", "low", "medium", "high", "max"]);

export interface ModelEntry {
  id: string;
  label: string;
  /** Thinking level for this model's chat turns (requests that carry tools); absent = the model's own default. */
  reasoning?: ReasoningEffort;
}

export interface EgressRoute {
  name: string;
  upstream: string;
  /** a model route: zero data retention forced, model + budget checked, cost logged as kind "model" (Claude Code in
   * the sandbox calls the model through one, docs/CLAUDE-CODE.md). Every route authenticates the caller by its
   * sandbox's egress token. */
  model?: boolean;
  /** pass the caller's Authorization on (an upstream that identifies the sandbox by its own egress token: our tools
   * role's MCP endpoint for Claude Code, docs/CLAUDE-CODE.md); otherwise caller credentials never go upstream */
  keepAuth?: boolean;
  inject: Record<string, string>;
  allowPaths?: string[];
}

export interface UpstreamMcp {
  prefix: string;
  url: string;
  headers: Record<string, string>;
  /**
   * The upstream's tools (their own names, without the prefix) the model may use: read-only ones only. Default deny: a
   * tool not named here, or one the upstream marks readOnlyHint false or destructiveHint true, is never offered.
   */
  tools?: string[];
}

/** sandbox.e2b: E2B's hosted sandboxes, built from chat/sandbox by the chat workflow. */
export interface E2bConfig {
  /** The template the sandboxes start from: its id or name, pinned per release (`<name>:<tag>` or a build's id). */
  template: string;
  /** The egress wire every sandbox's local proxy connects to: wss://<public egress host>/wire. */
  egressUrl: string;
  /** Extra headers on the wire's WebSocket (e.g. an Access service token), values may name ${VAR}s of the web env. */
  egressHeaders: Record<string, string>;
  /** Env var holding the API key (default E2B_API_KEY). */
  apiKeyEnv: string;
  /** Default https://api.e2b.app. */
  apiUrl: string;
  /** Sandbox hosts are <port>-<sandbox id>.<domain>; default e2b.app. */
  domain: string;
  /** Tags this instance's sandboxes (metadata `jarvis`), so the janitor never touches another instance's. */
  instance: string;
}

/**
 * sandbox.vm: our own sandbox hosts (docs/SANDBOX-VM.md): the always-on Hetzner hub plus AWS burst hosts, gVisor with
 * pre-copy live migration. The controller and scaler run in the web role.
 */
export interface VmConfig {
  /** the hub (Hetzner, always on): its sandboxd URL (overlay address) and the WireGuard endpoint AWS hosts dial */
  hub: { name: string; url: string; endpoint: string };
  sandboxCidr: string;
  hostCidr: string;
  /** the egress proxy's address as sandboxes see it (translated by the hub to an egress pod) */
  egressVip: string;
  /** where the controller's connections come from (the cluster's private network; the web pods are masqueraded) */
  controlCidr: string;
  /** comma-separated CPU features every sandbox sees: those all host types have (`runsc cpu-features`, intersected) */
  cpuFeatures: string;
  /** free sandbox slots the scaler keeps (an AWS host takes ~1 min to boot) */
  spareSlots: number;
  cpuOvercommit: number;
  /**
   * No new sandbox is placed on a host whose available memory (MemAvailable) is below this many bytes, whatever its
   * slots say (a backstop: slot sizes already keep every sandbox's full limit within RAM minus the host's reserve)
   */
  minAvailableMemory: number;
  /** an AWS host is terminated once it has no sandbox and is at least this old (from launch) */
  minHostLifetimeSeconds: number;
  /** env vars with PEMs: the CA, the controller's client certificate/key, and the burst hosts' server certificate/key */
  tls: { caEnv: string; certEnv: string; keyEnv: string; hostCertEnv: string; hostKeyEnv: string };
  /**
   * downtimeMs: pre-copy's target for the final stop; maxDowntimeMs: across clouds, a move whose final stop would be
   * longer is aborted before the switch (the sandbox keeps running); idleCpu: cores a sandbox may use and still move
   * across clouds
   */
  migration: { downtimeMs: number; maxRounds: number; maxDowntimeMs: number; idleCpu: number };
  /**
   * The provider sandboxes came from before the switch ("kubernetes"): its sandboxes keep running until released
   * (with their files saved); remove once none is left.
   */
  legacyProvider?: "kubernetes";
  /** AWS burst hosts; absent = the hub only */
  aws?: {
    region: string;
    launchTemplate: string;
    /** steady-state maximum (the quota: 3 x m7i.xlarge = 12 of 16 vCPU); one more only while a host is replaced */
    maxHosts: number;
    accessKeyIdEnv: string;
    secretAccessKeyEnv: string;
    /** unused (an empty AWS host goes once past sandbox.vm.minHostLifetimeSeconds); still accepted in configs */
    emptyMinutes: number;
    /** tag `jarvis` on every instance: the scaler only ever touches instances with its own value */
    instance: string;
  };
}

/**
 * budget: one shared spending limit for everyone (docs/CHAT.md, "Budget"): model cost (egress_log, kind model)
 * summed from `since`; once it reaches `limitUsd` egress refuses every model call (402 budget_exceeded).
 */
export interface BudgetConfig {
  limitUsd: number;
  /** ISO timestamp: spend counts from here (a reset = a newer `since`) */
  since: string;
  /** who set it, named in the message staff see when it is used up */
  ownerName?: string;
}

/**
 * A person of this instance: an email in `users`, or one at a domain listed there as "@domain" (e.g. "@yyc.hk": everyone
 * with a company address, signing in with Access's emailed code). Access checks the same list.
 */
export function isUser(config: Pick<ChatConfig, "users">, email: string | null | undefined): boolean {
  const e = String(email ?? "").trim().toLowerCase();
  if (!e || e.startsWith("@")) return false;
  if (config.users.includes(e)) return true;
  const at = e.lastIndexOf("@");
  return at > 0 && config.users.includes(e.slice(at));
}

export interface ChatConfig {
  product: { name: string; locale: string; logo?: string };
  publicUrl: string;
  access: { teamDomain: string; audience: string };
  /** emails, or "@domain" for everyone at that email domain (isUser) */
  users: string[];
  admins: string[];
  models: ModelEntry[];
  defaultModel: string;
  titleModel: string;
  /** Thinking level for the title model's tool-less calls (titles, Claude Code's quick calls); absent = its default. */
  titleReasoning?: ReasoningEffort;
  systemPromptFile: string;
  sandbox: {
    /** Where sandboxes run (docs/CHAT.md, "Sandbox providers"). */
    provider: "kubernetes" | "e2b" | "vm";
    /** kubernetes: the sandbox image. */
    image: string;
    /** kubernetes: the proxy sandboxes use (default http://egress.<web namespace>.svc.cluster.local:3128); e2b uses e2b.egressUrl. */
    egressProxyUrl?: string;
    namespace: string;
    runtimeClass: string;
    idleMinutes: number;
    maxLifetimeHours: number;
    maxActive: number;
    cpu: string;
    memory: string;
    /** PriorityClass of sandbox pods (above the pool's placeholder pods, so a sandbox evicts one at once). */
    priorityClass?: string;
    /** A docker-registry Secret in the sandbox namespace, when the sandbox image is private. */
    imagePullSecret?: string;
    e2b?: E2bConfig;
    vm?: VmConfig;
  };
  egress: {
    /** Hosts sandboxes may reach on top of the generated allowlist (docs/CHAT.md, "The sandbox allowlist"). */
    defaultAllowedHosts: string[];
    /** Hosts sandboxes may never reach; wins over every allow entry. */
    denyHosts: string[];
    /** Use the generated allowlist shipped with the image (chat/egress-allowlist/); false = defaultAllowedHosts only. */
    generatedAllowlist: boolean;
    /** Bytes a sandbox may send through one connection (default 8 MiB). */
    maxUploadBytes: number;
    routes: EgressRoute[];
    cacheGiB: number;
  };
  tools: {
    exa?: { keyEnv: string };
    tikhub?: { keyEnv: string };
    wisers?: { url: string; accessClientIdEnv: string; accessClientSecretEnv: string };
    upstreams: UpstreamMcp[];
  };
  /** absent = no limit */
  budget?: BudgetConfig;
}

/** How egress sets thinking per model: `withTools` for chat turns, `withoutTools` for tool-less calls. */
export type ReasoningPlan = Record<string, { withTools?: ReasoningEffort; withoutTools?: ReasoningEffort }>;

export function reasoningPlan(config: ChatConfig): ReasoningPlan {
  const plan: ReasoningPlan = {};
  for (const m of config.models) if (m.reasoning) plan[m.id] = { ...plan[m.id], withTools: m.reasoning };
  if (config.titleReasoning) plan[config.titleModel] = { ...plan[config.titleModel], withoutTools: config.titleReasoning };
  return plan;
}

/** Every model id this instance may call (egress refuses the rest): the offered models and the title model. */
export function allowedModels(config: ChatConfig): string[] {
  return [...new Set([...config.models.map((m) => m.id), config.titleModel])];
}

const DEFAULTS = {
  sandbox: { provider: "kubernetes" as const, image: "", namespace: "jarvis-sandbox", runtimeClass: "gvisor", idleMinutes: 30, maxLifetimeHours: 8, maxActive: 6, cpu: "1", memory: "1Gi" },
  egress: { defaultAllowedHosts: [] as string[], denyHosts: [] as string[], generatedAllowlist: true, maxUploadBytes: 8 * 1024 * 1024, routes: [] as EgressRoute[], cacheGiB: 5 },
  tools: { upstreams: [] as UpstreamMcp[] },
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
  const provider = c.sandbox?.provider ?? "kubernetes";
  if (provider !== "kubernetes" && provider !== "e2b" && provider !== "vm") throw new Error('config: sandbox.provider must be "kubernetes", "e2b" or "vm"');
  if (provider === "kubernetes" || provider === "vm") need("sandbox.image", c.sandbox?.image, "string");
  let vm: VmConfig | undefined;
  if (provider === "vm" || c.sandbox?.vm) {
    const v = c.sandbox?.vm ?? {};
    need("sandbox.vm.hub.url", v.hub?.url, "string");
    need("sandbox.vm.hub.name", v.hub?.name, "string");
    need("sandbox.vm.cpuFeatures", v.cpuFeatures, "string");
    const aws = v.aws
      ? {
          region: "eu-central-1",
          launchTemplate: "yyc-ai-sandbox-host",
          maxHosts: 3,
          accessKeyIdEnv: "SBX_AWS_ACCESS_KEY_ID",
          secretAccessKeyEnv: "SBX_AWS_SECRET_ACCESS_KEY",
          emptyMinutes: 5,
          instance: new URL(String(c.publicUrl)).hostname,
          ...v.aws,
        }
      : undefined;
    if (aws) need("sandbox.vm.hub.endpoint", v.hub?.endpoint, "string");
    vm = {
      sandboxCidr: "10.200.0.0/16",
      hostCidr: "10.201.0.0/24",
      egressVip: "10.201.1.1",
      controlCidr: "10.0.0.0/16",
      spareSlots: 1,
      cpuOvercommit: 3,
      minAvailableMemory: 256 * 1024 * 1024,
      minHostLifetimeSeconds: 900,
      ...v,
      hub: { endpoint: "", ...v.hub },
      tls: { caEnv: "SBX_CA_CERT", certEnv: "SBX_CLIENT_CERT", keyEnv: "SBX_CLIENT_KEY", hostCertEnv: "SBX_HOST_CERT", hostKeyEnv: "SBX_HOST_KEY", ...v.tls },
      migration: { downtimeMs: 100, maxRounds: 20, maxDowntimeMs: 500, idleCpu: 0.05, ...v.migration },
      aws,
    };
  }
  let e2b: E2bConfig | undefined;
  if (provider === "e2b") {
    const e = c.sandbox?.e2b ?? {};
    need("sandbox.e2b.template", e.template, "string");
    need("sandbox.e2b.egressUrl", e.egressUrl, "string");
    if (!/^wss?:\/\/[^/]+\/wire$/.test(e.egressUrl)) throw new Error("config: sandbox.e2b.egressUrl must be wss://<host>/wire");
    e2b = {
      template: e.template,
      egressUrl: e.egressUrl,
      egressHeaders: e.egressHeaders ?? {},
      apiKeyEnv: e.apiKeyEnv ?? "E2B_API_KEY",
      apiUrl: String(e.apiUrl ?? "https://api.e2b.app").replace(/\/$/, ""),
      domain: e.domain ?? "e2b.app",
      instance: e.instance ?? new URL(String(c.publicUrl)).hostname,
    };
  }
  if (!Array.isArray(c.users) || !c.users.length) throw new Error("config: users must list at least one email");
  if (!Array.isArray(c.models) || !c.models.length) throw new Error("config: models must not be empty");
  if (!c.models.some((m: ModelEntry) => m.id === c.defaultModel)) throw new Error("config: defaultModel must be one of models");
  for (const m of c.models as ModelEntry[]) {
    if (m.reasoning !== undefined && !REASONING_EFFORTS.has(m.reasoning)) throw new Error(`config: models[${m.id}].reasoning must be one of ${[...REASONING_EFFORTS].join(", ")}`);
  }
  if (c.titleReasoning !== undefined && !REASONING_EFFORTS.has(c.titleReasoning)) throw new Error(`config: titleReasoning must be one of ${[...REASONING_EFFORTS].join(", ")}`);
  let budget: BudgetConfig | undefined;
  if (c.budget != null) {
    const b = c.budget;
    if (typeof b.limitUsd !== "number" || !Number.isFinite(b.limitUsd) || b.limitUsd < 0) throw new Error("config: budget.limitUsd must be a number >= 0");
    if (typeof b.since !== "string" || Number.isNaN(Date.parse(b.since))) throw new Error("config: budget.since must be an ISO timestamp");
    if (b.ownerName !== undefined && typeof b.ownerName !== "string") throw new Error("config: budget.ownerName must be a string");
    budget = { limitUsd: b.limitUsd, since: new Date(b.since).toISOString(), ...(b.ownerName ? { ownerName: b.ownerName } : {}) };
  }
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
    ...(c.titleReasoning ? { titleReasoning: c.titleReasoning } : {}),
    systemPromptFile: c.systemPromptFile,
    sandbox: { ...DEFAULTS.sandbox, ...c.sandbox, provider, e2b, vm },
    egress: { ...DEFAULTS.egress, ...c.egress },
    tools: { ...DEFAULTS.tools, ...c.tools },
    budget,
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
