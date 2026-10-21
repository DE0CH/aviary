// Where sandboxes come from (docs/CHAT.md, "Sandbox providers"): the sandbox manager (sandbox.ts) owns the lifecycle
// and the Postgres record; a provider only makes, finds, keeps alive and removes the machines. Chosen by config
// `sandbox.provider`: "kubernetes" (gVisor pods, sandbox-k8s.ts: tests, local runs, a self-hosted cluster), "e2b"
// (E2B's hosted sandboxes, sandbox-e2b.ts) or "vm" (our own gVisor hosts with live migration, sandbox-vm.ts).
import type { VmStats } from "./sandbox-vm.ts";
import type { SandboxRow } from "./sandbox.ts";

/** How the web role (and jarvis-bash) reaches a sandbox's agent: base URL plus headers sent on every call. */
export interface AgentEndpoint {
  url: string;
  /** Provider headers (E2B's traffic access token); the exec token is added by AgentClient. */
  headers: Record<string, string>;
}

export interface ListedSandbox {
  /** The provider's name for it (what `sandboxes.pod` holds). */
  name: string;
  /** Epoch ms, or NaN when unknown. */
  createdAt: number;
}

export interface SandboxProvider {
  readonly kind: "kubernetes" | "e2b" | "vm";
  /**
   * Starts making the sandbox for a `starting` row and returns the provider's name for it as soon as it exists (the
   * manager stores it in `sandboxes.pod` before waiting, so the janitor never takes it for an orphan).
   */
  create(row: SandboxRow, egressToken: string): Promise<{ name: string }>;
  /** Waits until the sandbox's agent answers /healthz with the row's exec token; throws if it can't start. */
  waitReady(row: SandboxRow, deadline: number): Promise<AgentEndpoint>;
  /** Keeps the sandbox alive until at least `until` (a no-op where the provider has no timeout of its own). */
  keepAlive(row: SandboxRow, until: Date): Promise<void>;
  /** Removes it; a sandbox that is already gone is not an error. */
  destroy(name: string): Promise<void>;
  /** Every sandbox this instance owns at the provider (the janitor's orphan check). */
  list(): Promise<ListedSandbox[]>;
  /**
   * Hosts and per-sandbox use for the admin monitor (web/monitor.ts); read-only, best effort. "vm" answers its own
   * richer VmStats (scaler, decisions, migrations), which the monitor maps onto the common shape.
   */
  stats?(): Promise<ProviderStats | VmStats>;
}

// ---- the admin monitor (docs/CHAT.md, "Admin monitor") --------------------------------------------------------

/** One machine sandboxes run on: a Kubernetes node, a vm host (the Hetzner hub or an AWS burst host). */
export interface HostStats {
  name: string;
  /** "node" (kubernetes), "hub" or "aws" (vm) */
  kind: string;
  /** ready / not-ready (node); booting / active / draining / retiring (vm) */
  status: string;
  /** Answered just now (vm: sandboxd's /v1/host; node: Ready). */
  reachable: boolean;
  cpus: number | null;
  /** Cores in use now, when known. */
  cpuUsed: number | null;
  memTotal: number | null;
  memUsed: number | null;
  /** 1-minute load average, when the host reports one. */
  load1: number | null;
  sandboxes: number;
  /** How many sandboxes it may hold, when the provider has such a number. */
  maxSandboxes: number | null;
  /** When it (or its agent) started: uptime. */
  startedAt: string | null;
  /** USD spent on it so far (AWS hosts, from config sandbox.vm.aws.hourlyUsd). */
  costUsd: number | null;
  detail?: Record<string, unknown>;
}

/** One sandbox as its provider sees it (joined to the `sandboxes` row by `name` = sandboxes.pod). */
export interface SandboxStats {
  name: string;
  host: string | null;
  /** The provider's state: Running / Pending (pod), running / receiving (vm) … */
  state: string;
  cpuUsed: number | null;
  cpuLimit: number | null;
  memUsed: number | null;
  memLimit: number | null;
  restarts: number | null;
}

export interface ProviderStats {
  provider: SandboxProvider["kind"];
  hosts: HostStats[];
  sandboxes: SandboxStats[];
  /** Total sandbox slots across hosts (vm), else null: the pool is config sandbox.maxActive. */
  capacity: number | null;
  /** What could not be read and why (e.g. "metrics: 403"): shown as is on the page. */
  notes: string[];
  /** Provider-specific state (vm: the scaler's settings). */
  extra?: Record<string, unknown>;
}
