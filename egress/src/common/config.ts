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
  /** env vars with PEMs: the CA, the controller's client certificate/key, and the burst hosts' server certificate/key */
  tls: { caEnv: string; certEnv: string; keyEnv: string; hostCertEnv: string; hostKeyEnv: string };
  /**
   * downtimeMs: pre-copy's target for the final stop; maxDowntimeMs: across clouds, a move whose final stop would be
   * longer is aborted before the switch (the sandbox keeps running); idleCpu: cores a sandbox may use and still move
   * across clouds
   */
  migration: { downtimeMs: number; maxRounds: number; maxDowntimeMs: number; idleCpu: number };
  /** AWS burst hosts; absent = the hub only */
  aws?: {
    region: string;
    launchTemplate: string;
    /** steady-state maximum (the quota: 3 x m7i.xlarge = 12 of 16 vCPU); one more only while a host is replaced */
    maxHosts: number;
    accessKeyIdEnv: string;
    secretAccessKeyEnv: string;
    /** an empty AWS host is terminated after this long */
    emptyMinutes: number;
    /** tag `jarvis` on every instance: the scaler only ever touches instances with its own value */
    instance: string;
  };
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
  sandbox: { provider: "kubernetes" as const, image: "", namespace: "jarvis-sandbox", runtimeClass: "gvisor", idleMinutes: 30, maxLifetimeHours: 8, maxActive: 6, cpu: "1", memory: "1Gi" },
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
    sandbox: { ...DEFAULTS.sandbox, ...c.sandbox, provider, e2b, vm },
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
