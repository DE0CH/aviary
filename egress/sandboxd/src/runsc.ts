// gVisor (the DE0CH/gvisor fork's runsc: pre-copy live migration) for sandboxd. docs/SANDBOX-VM.md, "Migration",
// "Security", "Docker in sandboxes".
//
// Flags are identical for every sandbox on every host (a migration restores with the source's configuration):
// systrap (no KVM), gVisor's own network stack (connections are in the saved state), the root overlay's upper layer
// per sandbox (capped), no host sockets or FIFOs, raw/packet sockets inside the sandbox's netstack (Docker needs them;
// the host's nft lock still decides what leaves), and set-user-ID binaries honoured inside the sandbox (sudo for
// `apt-get install`, docs/SANDBOX-VM.md "Installing packages"; a uid inside gVisor is the sentry's, never a host uid,
// and gVisor saves the setting in the sandbox's kernel state, so a migration keeps the source's).
import { mkdirSync, writeFileSync } from "node:fs";
import type { SandboxSpec } from "./config.ts";
import { run, type RunResult } from "./util.ts";

export const ROOT = "/run/sbx/runsc";
export const STATE_DIR = "/var/lib/sbx";
export const IMAGES = `${STATE_DIR}/images`;
export const BUNDLES = `${STATE_DIR}/bundles`;
export const CKPT = "/run/sbx/ckpt";
export const LOGS = "/var/log/sbx";

export const RUNSC_FLAGS = [
  "--root", ROOT,
  "--platform=systrap",
  "--network=sandbox",
  "--overlay2=root:self,size=5g",
  "--host-uds=none",
  "--host-fifo=none",
  "--net-raw",
  "--allow-packet-socket-write",
  "--allow-suid",
];

export function runsc(args: string[], o: { timeoutMs?: number; input?: string } = {}): Promise<RunResult> {
  return run(process.env.SBX_RUNSC || "runsc", [...RUNSC_FLAGS, ...args], o);
}

/**
 * The capabilities a sandbox's root has INSIDE gVisor (dockerd, and sudo for package installs); the agent runs as
 * uid 1000 with none of them, and gets them back only through sudo (sbx-init.mjs).
 */
const DOCKER_CAPS = [
  "CAP_CHOWN", "CAP_DAC_OVERRIDE", "CAP_FOWNER", "CAP_FSETID", "CAP_KILL", "CAP_SETGID", "CAP_SETUID", "CAP_SETPCAP",
  "CAP_NET_BIND_SERVICE", "CAP_NET_RAW", "CAP_NET_ADMIN", "CAP_SYS_CHROOT", "CAP_MKNOD", "CAP_AUDIT_WRITE", "CAP_SETFCAP",
  "CAP_SYS_ADMIN", "CAP_SYS_PTRACE", "CAP_SYS_RESOURCE", "CAP_IPC_LOCK", "CAP_SYS_NICE",
];

/** The OCI runtime spec of a sandbox (pure: tested). `imageEnv` is the image config's Env. */
export function bundleSpec(spec: SandboxSpec, imageEnv: string[]): object {
  const env = new Map<string, string>();
  for (const kv of imageEnv) {
    const i = kv.indexOf("=");
    if (i > 0) env.set(kv.slice(0, i), kv.slice(i + 1));
  }
  for (const [k, v] of Object.entries(spec.env)) env.set(k, v);
  env.set("HOME", "/home/sandbox");
  return {
    ociVersion: "1.0.0",
    annotations: spec.cpuFeatures ? { "dev.gvisor.internal.cpufeatures": spec.cpuFeatures } : {},
    process: {
      // root inside gVisor for sbx-init (dockerd on demand); the agent runs as uid 1000 with no capabilities (sudo inside)
      user: { uid: 0, gid: 0 },
      cwd: "/work",
      args: ["/usr/bin/tini", "--", "node", "/opt/agent/sbx-init.mjs"],
      env: [...env].map(([k, v]) => `${k}=${v}`),
      capabilities: { bounding: DOCKER_CAPS, effective: DOCKER_CAPS, permitted: DOCKER_CAPS, inheritable: [], ambient: [] },
      rlimits: [{ type: "RLIMIT_NOFILE", hard: 65536, soft: 65536 }],
    },
    root: { path: `${IMAGES}/${spec.image}/rootfs`, readonly: false },
    hostname: "sandbox",
    mounts: [
      { destination: "/proc", type: "proc", source: "proc" },
      { destination: "/dev", type: "tmpfs", source: "tmpfs", options: ["nosuid", "strictatime", "mode=755", "size=65536k"] },
      { destination: "/dev/pts", type: "devpts", source: "devpts", options: ["nosuid", "noexec", "newinstance", "ptmxmode=0666", "mode=0620"] },
      { destination: "/dev/shm", type: "tmpfs", source: "shm", options: ["nosuid", "noexec", "nodev", "mode=1777", "size=65536k"] },
      { destination: "/sys", type: "sysfs", source: "sysfs", options: ["nosuid", "noexec", "nodev", "ro"] },
      { destination: "/sys/fs/cgroup", type: "cgroup", source: "cgroup", options: ["nosuid", "noexec", "nodev"] },
      { destination: "/tmp", type: "tmpfs", source: "tmpfs", options: ["nosuid", "nodev", "mode=1777"] },
      { destination: "/run", type: "tmpfs", source: "tmpfs", options: ["nosuid", "nodev", "mode=755"] },
      // Docker's images and containers live in the sandbox's memory, so they move with it
      { destination: "/var/lib/docker", type: "tmpfs", source: "tmpfs", options: ["mode=711"] },
    ],
    linux: {
      namespaces: [{ type: "pid" }, { type: "ipc" }, { type: "uts" }, { type: "mount" }, { type: "network", path: `/var/run/netns/${spec.name}` }],
      cgroupsPath: `/sbx/${spec.name}`,
      resources: {
        memory: { limit: spec.memory },
        cpu: { quota: Math.round(spec.cpu * 100_000), period: 100_000 },
        pids: { limit: 4096 },
      },
    },
  };
}

export function writeBundle(spec: SandboxSpec, imageEnv: string[]): string {
  const dir = `${BUNDLES}/${spec.name}`;
  mkdirSync(dir, { recursive: true });
  writeFileSync(`${dir}/config.json`, JSON.stringify(bundleSpec(spec, imageEnv), null, 1), { mode: 0o600 });
  return dir;
}
