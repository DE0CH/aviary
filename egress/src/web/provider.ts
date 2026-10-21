// Where sandboxes come from (docs/CHAT.md, "Sandbox providers"): the sandbox manager (sandbox.ts) owns the lifecycle
// and the Postgres record; a provider only makes, finds, keeps alive and removes the machines. Chosen by config
// `sandbox.provider`: "kubernetes" (gVisor pods, sandbox-k8s.ts: tests, local runs, a self-hosted cluster), "e2b"
// (E2B's hosted sandboxes, sandbox-e2b.ts) or "vm" (our own gVisor hosts with live migration, sandbox-vm.ts).
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
}
