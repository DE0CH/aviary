// What every part of the web role works with. main.ts builds the real one; tests pass fakes (S3, Kubernetes,
// claw, sandbox agents) through the same shape.
import type { AccessVerifier } from "../common/access.ts";
import type { ChatConfig } from "../common/config.ts";
import type { Db, Listener } from "../common/db.ts";
import type { BlobStore } from "../common/s3.ts";
import type { K8sApi, Pod } from "./k8s.ts";

export interface WebDeps {
  config: ChatConfig;
  db: Db;
  dbUrl: string;
  listener: Listener;
  blobs: BlobStore;
  k8s: K8sApi;
  verifier: AccessVerifier;
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
  /** Sandbox agent URL of a pod; default http://<podIP>:7070. */
  agentUrlFor?: (pod: Pod) => string;
  /** Extra env for claw (tests). */
  clawEnv?: Record<string, string>;
}

export const replicaName = (s: string): string => s.toLowerCase().replace(/[^a-z0-9_]/g, "_").slice(0, 50) || "replica";

export const blobKey = (deps: WebDeps, conversationId: string, sha: string): string => `${deps.config.s3.prefix}files/${conversationId}/${sha}`;
