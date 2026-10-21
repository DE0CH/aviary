// What a sandbox made for the CloudCLI UI gets so Claude Code runs inside it (docs/CLAUDE-CODE.md): the environment
// that points Claude Code at egress (model calls and our MCP tools, authenticated by the sandbox's own egress token),
// and the managed CLAUDE.md (the instance's prompt plus the sandbox's rules), which sbx-init.mjs writes to
// /etc/claude-code. Nothing here is a secret the sandbox doesn't already hold (its egress token is in HTTP_PROXY).
import { readFile } from "node:fs/promises";
import type { ChatConfig } from "../common/config.ts";

/** Appended to the instance's prompt in the managed CLAUDE.md. */
export const SANDBOX_RULES = `# Your computer

You work on a sandboxed Linux computer. Your working folder is /workspace.

- **What lasts:** the sandbox is released after a while without use, and a new one is made when the conversation
  continues. Only /workspace and this conversation carry over. Installed packages, running processes and every file
  outside /workspace are gone after a release; install again what you need.
- **/workspace holds at most 1 GiB and 1,000 files.** A write past that fails with "Disk quota exceeded". Keep only
  results in /workspace: put caches, installs and bulk files (.npm, node_modules, virtualenvs, unpacked archives,
  thousands of small files) in /tmp or your home folder. Each file write in /workspace is a network round trip, so
  bulk file work there is slow.
- **Installing software:** \`sudo apt-get install -y …\` for system packages, \`pip install …\` for Python, \`npm install
  -g …\` for Node. They work through the sandbox's proxy.
- **Internet:** only sites on the sandbox's allowlist are reachable from here; a refused site cannot be enabled.
  Use the web search and web fetch tools for reading web pages.
`;

export async function managedClaudeMd(config: ChatConfig): Promise<string> {
  const prompt = await readFile(config.systemPromptFile, "utf8");
  return `${prompt.trimEnd()}\n\n${SANDBOX_RULES}`;
}

/** The sandbox env additions: Claude Code → egress for the model and the tools (MCP). */
export async function claudeCodeEnv(config: ChatConfig, egressBase: string, egressToken: string): Promise<Record<string, string>> {
  const mcp = {
    mcpServers: {
      yyc: { type: "http", url: `${egressBase}/x/tools/mcp`, headers: { Authorization: `Bearer ${egressToken}` } },
    },
  };
  return {
    ANTHROPIC_BASE_URL: `${egressBase}/x/claude`,
    ANTHROPIC_AUTH_TOKEN: egressToken,
    ANTHROPIC_MODEL: config.defaultModel,
    ANTHROPIC_DEFAULT_HAIKU_MODEL: config.titleModel,
    ANTHROPIC_SMALL_FAST_MODEL: config.titleModel,
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
    DISABLE_AUTOUPDATER: "1",
    DISABLE_TELEMETRY: "1",
    DISABLE_ERROR_REPORTING: "1",
    JARVIS_CLAUDE_MD: Buffer.from(await managedClaudeMd(config)).toString("base64"),
    JARVIS_CLAUDE_MCP: Buffer.from(JSON.stringify(mcp)).toString("base64"),
  };
}
