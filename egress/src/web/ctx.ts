// What every part of the web role works with. main.ts builds the real one; tests pass fakes (Kubernetes or E2B,
// sandbox agents) through the same shape.
import type { ChatConfig } from "../common/config.ts";
import type { Db } from "../common/db.ts";
import type { SandboxProvider } from "./provider.ts";

export interface WebDeps {
  config: ChatConfig;
  db: Db;
  dbUrl: string;
  /** Where sandboxes come from (config sandbox.provider). */
  sandboxProvider: SandboxProvider;
  /** The namespace web, egress and tools run in (sandboxes run in config.sandbox.namespace). */
  namespace: string;
  /** Seconds a stopping replica waits for the calls it is answering (a sandbox start takes up to 3 minutes). */
  drainSeconds: number;
}
