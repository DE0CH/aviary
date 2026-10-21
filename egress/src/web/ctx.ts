// What every part of the web role works with. main.ts builds the real one; tests pass fakes (S3, Kubernetes or E2B,
// claw, sandbox agents) through the same shape.
import type { AccessVerifier } from "../common/access.ts";
import type { ChatConfig } from "../common/config.ts";
import type { Db, Listener } from "../common/db.ts";
import type { BlobStore } from "../common/s3.ts";
import type { K8sApi } from "./k8s.ts";
import type { SandboxProvider } from "./provider.ts";

export interface WebDeps {
  config: ChatConfig;
  db: Db;
  dbUrl: string;
  listener: Listener;
  blobs: BlobStore;
  /** Where sandboxes come from (config sandbox.provider). */
  sandboxProvider: SandboxProvider;
  verifier: AccessVerifier;
  /** This cluster's API as the web service account, when it runs in one (the admin monitor reads Deployments). */
  k8s?: K8sApi;
  /** This replica's name (pod name), [a-z0-9_] only: it names its cancel channel and heartbeat. */
  replica: string;
  /** The namespace web, egress and tools run in (sandboxes run in config.sandbox.namespace). */
  namespace: string;
  /** Local scratch: <workRoot>/<conversation>/{work,sessions,claw-config,sync.json,stored.json}. */
  workRoot: string;
  clawPath: string;
  bashPath: string;
  mcpBridgePath: string;
  /** http://egress.<ns>.svc.cluster.local:3128 */
  egressBase: string;
  /** http://tools.<ns>.svc.cluster.local:8080/mcp */
  toolsUrl: string;
  uiDir: string;
  brandDir: string;
  drainSeconds: number;
  /** Extra env for claw (tests). */
  clawEnv?: Record<string, string>;
}

export const replicaName = (s: string): string => s.toLowerCase().replace(/[^a-z0-9_]/g, "_").slice(0, 50) || "replica";

export const blobKey = (deps: WebDeps, conversationId: string, sha: string): string => `${deps.config.s3.prefix}files/${conversationId}/${sha}`;
