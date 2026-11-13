// egress's own configuration (aviary platform spec 17.2 item 2): `/etc/egress/egress.json` (ConfigMap `egress-config`),
// nothing but egress's parts. Secrets come from the environment; a string in `inject` may name one as ${VAR}, expanded
// where it is used (expandEnv) and checked once at start.
import { parseExit, type ExitConfig } from "./exit.ts";
import { readFileSync } from "node:fs";
import { expandEnv, type ReasoningEffort, type ModelPlan } from "../common/config.ts";

export const EGRESS_CONFIG_PATH = "/etc/egress/egress.json";
const REASONING: ReadonlySet<string> = new Set(["none", "low", "medium", "high", "max"]);

export interface EgressModel {
  id: string;
  label: string;
  /** Thinking level for this model's chat turns (requests that carry tools); absent = the model's own default. */
  reasoning?: ReasoningEffort;
  /** The thinking levels a person may choose for this model's chat turns, lowest first (what the model's OpenRouter
   * endpoints take, e.g. none, low, high, max); `reasoning` is the one a conversation starts on and must be among
   * them. Absent = no choice: every chat turn runs at `reasoning`. */
  thinking?: ReasoningEffort[];
  /** OpenRouter endpoints (provider slugs, e.g. "deepinfra/fp8") this model's requests may use, in order of preference;
   * absent = OpenRouter picks among every ZDR endpoint. Pinned because endpoints differ in how they render a
   * conversation: one dropped part of long tool-and-image histories and the model took its own tool results for the
   * user's words (2026-11-10). */
  providers?: string[];
  /** The harness a conversation on this model runs in: "claude" (Claude Code; the default, never written out) or
   * "qwen" (Qwen Code, which calls the model route's OpenAI chat completions path). */
  harness?: Harness;
  /** The people (emails) who may pick this model; absent = everyone. A model being tried is listed for the people
   * trying it only. */
  visibleTo?: string[];
}

/** The harnesses a model can name. */
export type Harness = "claude" | "qwen";
const HARNESSES = new Set<string>(["claude", "qwen"]);

/** Which verified fact an `identityHeaders` header carries. */
export type IdentityFact = "sandbox" | "conversation" | "person";
const FACTS: ReadonlySet<string> = new Set(["sandbox", "conversation", "person"]);
const FIXED_NAMES: ReadonlySet<string> = new Set(["magpie", "hermit", "model", "tools"]);
/** A sandbox route's name: one lowercase DNS label (gateway-api `routes::valid_name`). */
const ROUTE_NAME = /^[a-z](?:[a-z0-9-]{0,61}[a-z0-9])?$/;
const HEADER_NAME = /^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/;

/** A configured route: the fixed names `model` and `tools` (3.11.3; `magpie` and `hermit` are built in), and any
 * number of sandbox routes (platform 3.11.8, a configured name beside the fixed ones: YYC's `labels`). */
export interface EgressRoute {
  name: string;
  upstream: string;
  /** the model route: zero data retention forced, model + budget checked, cost logged as kind "model" */
  model?: boolean;
  /** headers set on every upstream request; values may name ${VAR}s of egress's environment */
  inject: Record<string, string>;
  /** path prefixes allowed on the route (after the fixed name's own rule) */
  allowPaths?: string[];
  /** the upstream may stay silent this long before the request is cut (the tools route: 180) */
  timeoutSeconds?: number;
  /** sandbox routes only: headers set from the verified token for an upstream that reads its own names (YYC's label
   * store: `X-Jarvis-Conversation` ← the conversation), beside the `Aviary-Sandbox-*` triple */
  identityHeaders?: Record<string, IdentityFact>;
  /** sandbox routes only: `false` for an upstream outside YYC (a private GitHub repository, read-only): it gets no
   * `Aviary-*` header at all, so no staff identity leaves; the route's log row still names who asked */
  identity?: false;
}

/** A sandbox route (not `model` or `tools`). */
export const isSandboxRoute = (r: EgressRoute): boolean => r.name !== "model" && r.name !== "tools";

/** A path prefix a sandbox route may have: `/…/`, no dot or empty segment, nothing percent-encoded that could leave it. */
function cleanPrefix(p: string): boolean {
  if (!p.startsWith("/") || !p.endsWith("/") || p.includes("//") || /%2e|%2f|%5c|\\/i.test(p)) return false;
  return p.slice(1, -1).split("/").every((s) => s !== "." && s !== "..");
}

export interface BudgetConfig {
  limitUsd: number;
  /** ISO timestamp: spend counts from here (a reset = a newer `since`) */
  since: string;
  /** who set it, named in the message staff see when it is used up */
  ownerName?: string;
}

export interface EgressConfig {
  /** `locale` picks the language of the budget message (zh… or English) */
  product: { name: string; locale: string };
  models: EgressModel[];
  defaultModel: string;
  /** the id Claude Code uses for its small fast calls (R18 `small`) */
  smallModel: string;
  titleModel: string;
  titleReasoning?: ReasoningEffort;
  /** the default model's context window in tokens (R18 `contextTokens`) */
  contextTokens: number;
  routes: EgressRoute[];
  /** the proxy's own addresses: proxy-form requests to one of them (any port) are served locally (the mirror) */
  selfHosts: string[];
  generatedAllowlist: boolean;
  defaultAllowedHosts: string[];
  denyHosts: string[];
  /** extra refused address ranges (e.g. the node's own /64) */
  blockCidrs: string[];
  maxUploadBytes: number;
  cacheGiB: number;
  /** absent = no limit */
  budget?: BudgetConfig;
  /** the way out (exit.ts): internet connections by name through this SOCKS5 server; absent = resolve and connect here */
  exit?: ExitConfig;
}

const strings = (v: unknown, path: string): string[] => {
  if (v === undefined) return [];
  if (!Array.isArray(v) || v.some((x) => typeof x !== "string")) throw new Error(`egress config: ${path} must be a list of strings`);
  return v as string[];
};

export function parseEgressConfig(raw: unknown): EgressConfig {
  if (!raw || typeof raw !== "object") throw new Error("egress config: not an object");
  const c = raw as Record<string, any>;
  const need = (path: string, v: unknown) => {
    if (typeof v !== "string" || !v) throw new Error(`egress config: ${path} must be a non-empty string`);
  };
  need("product.name", c.product?.name);
  need("defaultModel", c.defaultModel);
  if (!Array.isArray(c.models) || !c.models.length) throw new Error("egress config: models must not be empty");
  for (const m of c.models as EgressModel[]) {
    need("models[].id", m?.id);
    need(`models[${m.id}].label`, m.label);
    if (m.reasoning !== undefined && !REASONING.has(m.reasoning)) throw new Error(`egress config: models[${m.id}].reasoning must be one of ${[...REASONING].join(", ")}`);
    if (m.thinking !== undefined) {
      const order = [...REASONING];
      const ok = Array.isArray(m.thinking) && m.thinking.length > 0 && m.thinking.every((l, i) => REASONING.has(l) && (i === 0 || order.indexOf(l) > order.indexOf(m.thinking![i - 1]!)));
      if (!ok) throw new Error(`egress config: models[${m.id}].thinking must be levels of ${order.join(", ")}, lowest first`);
      if (!m.reasoning || !m.thinking.includes(m.reasoning)) throw new Error(`egress config: models[${m.id}].reasoning must be one of its thinking levels`);
    }
    if (m.providers !== undefined && (!Array.isArray(m.providers) || !m.providers.length || !m.providers.every((p) => typeof p === "string" && /^[a-z0-9][a-z0-9./-]*$/.test(p))))
      throw new Error(`egress config: models[${m.id}].providers must be a non-empty list of OpenRouter provider slugs`);
    if (m.harness !== undefined && !HARNESSES.has(m.harness)) throw new Error(`egress config: models[${m.id}].harness must be one of ${[...HARNESSES].join(", ")}`);
    if (m.visibleTo !== undefined && (!Array.isArray(m.visibleTo) || !m.visibleTo.length || !m.visibleTo.every((e) => typeof e === "string" && /^[^\s@]+@[^\s@]+$/.test(e))))
      throw new Error(`egress config: models[${m.id}].visibleTo must be a non-empty list of emails`);
  }
  const dflt = (c.models as EgressModel[]).find((m) => m.id === c.defaultModel);
  if (dflt?.visibleTo) throw new Error("egress config: defaultModel must be a model everyone sees");
  if (!c.models.some((m: EgressModel) => m.id === c.defaultModel)) throw new Error("egress config: defaultModel must be one of models");
  if (c.titleReasoning !== undefined && !REASONING.has(c.titleReasoning)) throw new Error(`egress config: titleReasoning must be one of ${[...REASONING].join(", ")}`);
  if (!Number.isInteger(c.contextTokens) || c.contextTokens <= 0) throw new Error("egress config: contextTokens must be a positive integer");
  if (!Array.isArray(c.routes)) throw new Error("egress config: routes must be a list");
  const routes: EgressRoute[] = [];
  for (const r of c.routes as Record<string, any>[]) {
    const sandbox = r?.name !== "model" && r?.name !== "tools";
    if (sandbox) {
      if (typeof r?.name !== "string" || !ROUTE_NAME.test(r.name)) throw new Error(`egress config: route ${JSON.stringify(r?.name)}: a sandbox route's name is one lowercase DNS label`);
      if (FIXED_NAMES.has(r.name)) throw new Error(`egress config: route ${r.name}: magpie and hermit are built in`);
      if (!Array.isArray(r.allowPaths) || !r.allowPaths.length || r.allowPaths.some((p: unknown) => typeof p !== "string" || !cleanPrefix(p))) throw new Error(`egress config: routes[${r.name}].allowPaths: a sandbox route needs path prefixes, each /…/ (platform 3.11.8)`);
      if (r.identityHeaders !== undefined && (typeof r.identityHeaders !== "object" || Object.entries(r.identityHeaders).some(([k, v]) => !HEADER_NAME.test(k) || k.toLowerCase().startsWith("aviary-") || typeof v !== "string" || !FACTS.has(v)))) throw new Error(`egress config: routes[${r.name}].identityHeaders maps header names (not Aviary-*) to sandbox, conversation or person`);
      if (r.identity !== undefined && r.identity !== false) throw new Error(`egress config: routes[${r.name}].identity is false or absent`);
      if (r.identity === false && r.identityHeaders !== undefined) throw new Error(`egress config: routes[${r.name}]: identity: false takes no identityHeaders`);
    } else if (r.identityHeaders !== undefined || r.identity !== undefined) throw new Error(`egress config: routes[${r.name}].identityHeaders and identity: sandbox routes only`);
    if (routes.some((x) => x.name === r.name)) throw new Error(`egress config: route ${r.name} twice`);
    need(`routes[${r.name}].upstream`, r.upstream);
    if ((r.name === "model") !== (r.model === true)) throw new Error("egress config: the route named model, and only it, has model: true");
    if (r.inject !== undefined && (typeof r.inject !== "object" || Object.values(r.inject).some((v) => typeof v !== "string"))) throw new Error(`egress config: routes[${r.name}].inject must map header names to strings`);
    if (r.timeoutSeconds !== undefined && !(typeof r.timeoutSeconds === "number" && r.timeoutSeconds > 0)) throw new Error(`egress config: routes[${r.name}].timeoutSeconds must be a positive number`);
    routes.push({
      name: r.name,
      upstream: String(r.upstream).replace(/\/$/, ""),
      ...(r.model === true ? { model: true } : {}),
      inject: r.inject ?? {},
      ...(r.allowPaths ? { allowPaths: strings(r.allowPaths, `routes[${r.name}].allowPaths`) } : {}),
      ...(r.timeoutSeconds ? { timeoutSeconds: r.timeoutSeconds } : {}),
      ...(r.identityHeaders ? { identityHeaders: { ...r.identityHeaders } } : {}),
      ...(r.identity === false ? { identity: false as const } : {}),
    });
  }
  for (const n of ["model", "tools"] as const) if (!routes.some((r) => r.name === n)) throw new Error(`egress config: no route ${n}`);
  let budget: BudgetConfig | undefined;
  if (c.budget != null) {
    const b = c.budget;
    if (typeof b.limitUsd !== "number" || !Number.isFinite(b.limitUsd) || b.limitUsd < 0) throw new Error("egress config: budget.limitUsd must be a number >= 0");
    if (typeof b.since !== "string" || Number.isNaN(Date.parse(b.since))) throw new Error("egress config: budget.since must be an ISO timestamp");
    if (b.ownerName !== undefined && typeof b.ownerName !== "string") throw new Error("egress config: budget.ownerName must be a string");
    budget = { limitUsd: b.limitUsd, since: new Date(b.since).toISOString(), ...(b.ownerName ? { ownerName: b.ownerName } : {}) };
  }
  const num = (v: unknown, d: number, path: string): number => {
    if (v === undefined) return d;
    if (typeof v !== "number" || !Number.isFinite(v) || v < 0) throw new Error(`egress config: ${path} must be a number >= 0`);
    return v;
  };
  return {
    product: { name: c.product.name, locale: typeof c.product.locale === "string" && c.product.locale ? c.product.locale : "en" },
    models: c.models.map((m: EgressModel) => ({ id: m.id, label: m.label, ...(m.reasoning ? { reasoning: m.reasoning } : {}), ...(m.thinking ? { thinking: [...m.thinking] } : {}), ...(m.providers ? { providers: [...m.providers] } : {}), ...(m.harness && m.harness !== "claude" ? { harness: m.harness } : {}), ...(m.visibleTo ? { visibleTo: m.visibleTo.map((e) => e.toLowerCase()) } : {}) })),
    defaultModel: c.defaultModel,
    smallModel: typeof c.smallModel === "string" && c.smallModel ? c.smallModel : c.defaultModel,
    titleModel: typeof c.titleModel === "string" && c.titleModel ? c.titleModel : c.defaultModel,
    ...(c.titleReasoning ? { titleReasoning: c.titleReasoning } : {}),
    contextTokens: c.contextTokens,
    routes,
    selfHosts: strings(c.selfHosts, "selfHosts"),
    generatedAllowlist: c.generatedAllowlist !== false,
    defaultAllowedHosts: strings(c.defaultAllowedHosts, "defaultAllowedHosts"),
    denyHosts: strings(c.denyHosts, "denyHosts"),
    blockCidrs: strings(c.blockCidrs, "blockCidrs"),
    maxUploadBytes: num(c.maxUploadBytes, 8 * 1024 * 1024, "maxUploadBytes"),
    cacheGiB: num(c.cacheGiB, 5, "cacheGiB"),
    ...(budget ? { budget } : {}),
    ...(c.exit != null ? { exit: parseExit(c.exit) } : {}),
  };
}

export function loadEgressConfig(path = EGRESS_CONFIG_PATH): EgressConfig {
  return parseEgressConfig(JSON.parse(readFileSync(path, "utf8")));
}

/** Every model id egress lets through (A6.F15): the offered models, the default, the small and the title model. */
export function allowedModels(c: EgressConfig): string[] {
  return [...new Set([...c.models.map((m) => m.id), c.defaultModel, c.smallModel, c.titleModel])];
}

/** Per model: thinking (`withTools` for chat turns, `withoutTools` for tool-less calls such as titles) and the pinned
 * providers. */
export function modelPlan(c: EgressConfig): ModelPlan {
  const plan: ModelPlan = {};
  for (const m of c.models) if (m.reasoning) plan[m.id] = { ...plan[m.id], withTools: m.reasoning };
  for (const m of c.models) if (m.thinking) plan[m.id] = { ...plan[m.id], levels: [...m.thinking] };
  for (const m of c.models) if (m.providers) plan[m.id] = { ...plan[m.id], providers: [...m.providers] };
  if (c.titleReasoning) plan[c.titleModel] = { ...plan[c.titleModel], withoutTools: c.titleReasoning };
  return plan;
}

/** `GET /v1/aviary/models` (R18): for pigeon in-cluster and for sandboxes as `http://model/v1/aviary/models`. */
export function modelsAnswer(c: EgressConfig): { default: string; small: string; title: string; contextTokens: number; models: Array<{ id: string; label: string; reasoning: ReasoningEffort | null; thinking: ReasoningEffort[]; harness?: Harness; visibleTo?: string[] }> } {
  return {
    default: c.defaultModel,
    small: c.smallModel,
    title: c.titleModel,
    contextTokens: c.contextTokens,
    // `thinking`: the levels a person may choose (empty: none), `reasoning` the one a conversation starts on
    // `harness` and `visibleTo` only when they say something: a Claude Code model for everyone reads as it always did
    models: c.models.map((m) => ({
      id: m.id,
      label: m.label,
      reasoning: m.reasoning ?? null,
      thinking: m.thinking ? [...m.thinking] : [],
      ...(m.harness && m.harness !== "claude" ? { harness: m.harness } : {}),
      ...(m.visibleTo ? { visibleTo: [...m.visibleTo] } : {}),
    })),
  };
}

/** Every ${VAR} the routes name expands: a missing secret is a start failure (logged by name, never by value). */
export function checkInjectEnv(c: EgressConfig, env: Record<string, string | undefined> = process.env): void {
  for (const r of c.routes) for (const v of Object.values(r.inject)) expandEnv(v, env);
}
