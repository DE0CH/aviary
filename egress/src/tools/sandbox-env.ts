// sandbox_environment (docs/CHAT.md, "sandbox_environment"): a read-only tool that tells the model how its sandbox is
// set up: the image (its Dockerfile and the preinstalled packages, both baked into the chat image at build time from
// chat/sandbox/, so they match the sandbox image of the same commit), the limits, how to install software, Docker
// and the network rules. Facts come from the instance's config and the build; nothing secret (no tokens, no
// addresses beyond the proxy's role).
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { ChatConfig } from "../common/config.ts";
import { str, text, ToolInputError, type ToolDef } from "./types.ts";

/** Where the chat image keeps the sandbox image's Dockerfile and package list (chat/Dockerfile). */
export const SANDBOX_ENV_DIR = process.env.JARVIS_SANDBOX_ENV_DIR || "/app/sandbox-env";

const SECTIONS = ["summary", "dockerfile", "packages", "all"] as const;

function readOr(dir: string, name: string): string | null {
  const p = join(dir, name);
  return existsSync(p) ? readFileSync(p, "utf8") : null;
}

/** The image reference sandboxes start from (vm/kubernetes: the image; e2b: the template). */
function imageOf(c: ChatConfig): string {
  return c.sandbox.provider === "e2b" ? `E2B template ${c.sandbox.e2b?.template ?? "?"}` : c.sandbox.image || "?";
}

export function summary(c: ChatConfig, commit: string): string {
  const own = c.sandbox.provider === "vm";
  const mib = Math.round(c.egress.maxUploadBytes / 1048576);
  const lines = [
    "# The sandbox",
    `Image: ${imageOf(c)} (built from the Dockerfile in section "dockerfile"; runtime commit ${commit || "unknown"}).`,
    "Base: Debian 13 (trixie) with Node 22 and Python 3; commands run as `bash -lc` in /work as user `node` (uid 1000), HOME /home/sandbox.",
    "",
    "## Limits",
    `- CPU ${c.sandbox.cpu} core(s), memory ${c.sandbox.memory} for everything in the sandbox (Docker included); going over the memory limit ends the whole sandbox.`,
    own ? "- Disk: writes outside the workspace (installs, /tmp is in memory) are capped at 5 GiB; processes at 4096." : "- Disk: the sandbox's own disk; keep installs small.",
    "- One command runs at most 10 minutes (default 2); use the bash tool's background mode for longer jobs (output in .jobs/<id>.log).",
    `- The sandbox is released after ${c.sandbox.idleMinutes} idle minutes or ${c.sandbox.maxLifetimeHours} h at most: files in /work are kept, installed packages and processes are not (reinstall after a reset).`,
    "- Files written under /work are in your workspace right after each bash command: read_file on an image you made (e.g. a matplotlib chart saved as /work/chart.png) shows it to you; mcp__tools__present_files shows deliverables to the user.",
    "",
    "## Installing software (lasts while the sandbox lives)",
    "- Python: `pip install x` (a virtualenv at /opt/py, first on PATH; no sudo needed). `uv` works too after installing it.",
    "- Node: `npm install -g x` (prefix ~/.npm-global, on PATH) or `npx x`.",
    own
      ? "- System packages: `sudo apt-get install -y x` (or plain `apt-get install -y x`): passwordless sudo, package lists are fetched on the first install, no recommended packages. Root exists only inside the sandbox."
      : "- System packages (apt) can't be installed here (no root): use pip, npm or a user-level installer.",
    "- Other tools: installers that put binaries into ~/.local/bin or ~/.cargo/bin (both on PATH), e.g. `curl -LsSf https://astral.sh/uv/install.sh | sh`, rustup; release binaries from GitHub into ~/.local/bin.",
    "- Everything downloads through the egress proxy (package mirrors for pip/npm/apt are cached there).",
    "",
    "## Docker",
    own
      ? "- `docker` works: the daemon starts on the first docker command (a few seconds). Containers share the sandbox's network (--network=host is added) and memory, so prefer small images."
      : "- Not available in this sandbox.",
    "",
    "## Network",
    "- The only way out is the egress proxy (HTTP_PROXY/HTTPS_PROXY are set): an automatic allowlist of package registries, code hosts, container registries, government/statistics, news and other widely used public sites.",
    `- Everything else is refused with HTTP 403, and nobody can enable a site from the chat. Pastebins, file drops, webhooks, forms and tunnels are always refused; uploads are capped at ${mib} MiB per connection.`,
    "- No direct connections (no DNS of its own, no other ports). To read or search web pages use the web_fetch and web_search tools instead; use the sandbox's internet for packages and data files.",
    "",
    'More: section "dockerfile" (how the image is built) or "packages" (every preinstalled apt, pip and npm package with its version; `match` filters).',
  ];
  return lines.join("\n");
}

export function packagesText(dir: string, match: string): string {
  const all = readOr(dir, "packages.txt");
  if (all === null || !all.split("\n").some((l) => l.trim() && !l.startsWith("#"))) return "The list of preinstalled packages was not recorded in this build; run `dpkg -l`, `pip list` or `npm ls -g` in the sandbox.";
  if (!match) return `# Preinstalled packages (name version)\n${all.trimEnd()}`;
  const m = match.toLowerCase();
  const hits = all.split("\n").filter((l) => l.startsWith("#") || l.toLowerCase().includes(m));
  const n = hits.filter((l) => !l.startsWith("#")).length;
  return n ? `# Preinstalled packages matching "${match}" (${n})\n${hits.join("\n")}` : `No preinstalled package matches "${match}"; install it (see section "summary").`;
}

export const sandboxEnvironment: ToolDef = {
  name: "sandbox_environment",
  title: "How the sandbox is set up",
  description:
    "Describes the bash sandbox: its image and the Dockerfile it is built from, every preinstalled package (apt, pip, " +
    "npm) with its version, its CPU/memory/disk/time limits, how to install more software (apt with sudo, pip, npm, " +
    "~/.local installers), Docker, and its network rules. Call it when you need to know what is installed or allowed " +
    "before working around it; it changes nothing. `section`: summary (default), dockerfile, packages or all; `match` " +
    "filters the package list (e.g. \"pandas\").",
  inputSchema: {
    type: "object",
    properties: {
      section: { type: "string", enum: [...SECTIONS], default: "summary" },
      match: { type: "string", description: "Only packages whose line contains this (section packages)." },
    },
    additionalProperties: false,
  },
  annotations: { title: "How the sandbox is set up", readOnlyHint: true, idempotentHint: true, openWorldHint: false },
  async run(args, ctx) {
    const section = str(args, "section") || "summary";
    if (!(SECTIONS as readonly string[]).includes(section)) throw new ToolInputError(`section must be one of ${SECTIONS.join(", ")}`);
    const match = str(args, "match").slice(0, 100);
    const commit = ctx.deps.env.JARVIS_COMMIT || "";
    const dir = ctx.deps.sandboxEnvDir ?? SANDBOX_ENV_DIR;
    const parts: string[] = [];
    if (section === "summary" || section === "all") parts.push(summary(ctx.deps.config, commit));
    if (section === "dockerfile" || section === "all") {
      const df = readOr(dir, "Dockerfile");
      parts.push(df === null ? "The Dockerfile was not recorded in this build." : `# The sandbox image's Dockerfile (chat/sandbox/Dockerfile)\n${df.trimEnd()}`);
    }
    if (section === "packages" || section === "all" || match) parts.push(packagesText(dir, match));
    return text(parts.join("\n\n"));
  },
};
