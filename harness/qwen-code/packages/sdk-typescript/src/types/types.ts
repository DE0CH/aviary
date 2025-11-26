import type {
  PermissionMode,
  PermissionSuggestion,
  SubagentConfig,
} from './protocol.js';

export type { PermissionMode };

type JSONSchema = {
  type: string;
  properties?: Record<string, unknown>;
  required?: string[];
  description?: string;
  [key: string]: unknown;
};

export type ToolDefinition<TInput = unknown, TOutput = unknown> = {
  name: string;
  description: string;
  inputSchema: JSONSchema;
  handler: (input: TInput) => Promise<TOutput>;
};

export type TransportOptions = {
  pathToQwenExecutable: string;
  cwd?: string;
  model?: string;
  permissionMode?: PermissionMode;
  env?: Record<string, string>;
  abortController?: AbortController;
  debug?: boolean;
  stderr?: (message: string) => void;
  logLevel?: 'debug' | 'info' | 'warn' | 'error';
  maxSessionTurns?: number;
  coreTools?: string[];
  excludeTools?: string[];
  authType?: string;
  includePartialMessages?: boolean;
};

type ToolInput = Record<string, unknown>;

export type CanUseTool = (
  toolName: string,
  input: ToolInput,
  options: {
    signal: AbortSignal;
    suggestions?: PermissionSuggestion[] | null;
  },
) => Promise<PermissionResult>;

export type PermissionResult =
  | {
      behavior: 'allow';
      updatedInput: ToolInput;
    }
  | {
      behavior: 'deny';
      message: string;
      interrupt?: boolean;
    };

export interface ExternalMcpServerConfig {
  command: string;
  args?: string[];
  env?: Record<string, string>;
}

export interface SdkMcpServerConfig {
  connect: (transport: unknown) => Promise<void>;
}

/**
 * Configuration options for creating a query session with the Qwen CLI.
 */
export interface QueryOptions {
  /**
   * The working directory for the query session.
   * This determines the context in which file operations and commands are executed.
   * @default process.cwd()
   */
  cwd?: string;

  /**
   * The AI model to use for the query session.
   * This takes precedence over the environment variables `OPENAI_MODEL` and `QWEN_MODEL`
   * @example 'qwen-max', 'qwen-plus', 'qwen-turbo'
   */
  model?: string;

  /**
   * Path to the Qwen CLI executable or runtime specification.
   *
   * Supports multiple formats:
   * - 'qwen' -> native binary (auto-detected from PATH)
   * - '/path/to/qwen' -> native binary (explicit path)
   * - '/path/to/cli.js' -> Node.js bundle (default for .js files)
   * - '/path/to/index.ts' -> TypeScript source (requires tsx)
   * - 'bun:/path/to/cli.js' -> Force Bun runtime
   * - 'node:/path/to/cli.js' -> Force Node.js runtime
   * - 'tsx:/path/to/index.ts' -> Force tsx runtime
   * - 'deno:/path/to/cli.ts' -> Force Deno runtime
   *
   * If not provided, the SDK will auto-detect the native binary in this order:
   * 1. QWEN_CODE_CLI_PATH environment variable
   * 2. ~/.volta/bin/qwen
   * 3. ~/.npm-global/bin/qwen
   * 4. /usr/local/bin/qwen
   * 5. ~/.local/bin/qwen
   * 6. ~/node_modules/.bin/qwen
   * 7. ~/.yarn/bin/qwen
   *
   * The .ts files are only supported for debugging purposes.
   *
   * @example 'qwen'
   * @example '/usr/local/bin/qwen'
   * @example 'tsx:/path/to/packages/cli/src/index.ts'
   */
  pathToQwenExecutable?: string;

  /**
   * Environment variables to pass to the Qwen CLI process.
   * These variables will be merged with the current process environment.
   */
  env?: Record<string, string>;

  /**
   * Alias for `approval-mode` command line argument.
   * Behaves slightly differently from the command line argument.
   * Permission mode controlling how the CLI handles tool usage and file operations **in non-interactive mode**.
   * - 'default': Automatically deny all write-like tools(edit, write_file, etc.) and dangers commands.
   * - 'plan': Shows a plan before executing operations
   * - 'auto-edit': Automatically applies edits without confirmation
   * - 'yolo': Executes all operations without prompting
   * @default 'default'
   */
  permissionMode?: 'default' | 'plan' | 'auto-edit' | 'yolo';

  /**
   * Custom permission handler for tool usage.
   * This function is called when the SDK needs to determine if a tool should be allowed.
   * Use this with `permissionMode` to gain more control over the tool usage.
   * TODO: For now we don't support modifying the input.
   */
  canUseTool?: CanUseTool;

  /**
   * External MCP (Model Context Protocol) servers to connect to.
   * Each server is identified by a unique name and configured with command, args, and environment.
   * @example { 'my-server': { command: 'node', args: ['server.js'], env: { PORT: '3000' } } }
   */
  mcpServers?: Record<string, ExternalMcpServerConfig>;

  /**
   * AbortController to cancel the query session.
   * Call abortController.abort() to terminate the session and cleanup resources.
   * Remember to handle the AbortError when the session is aborted.
   */
  abortController?: AbortController;

  /**
   * Enable debug mode for verbose logging.
   * When true, additional diagnostic information will be output.
   * Use this with `logLevel` to control the verbosity of the logs.
   * @default false
   */
  debug?: boolean;

  /**
   * Custom handler for stderr output from the Qwen CLI process.
   * Use this to capture and process error messages or diagnostic output.
   */
  stderr?: (message: string) => void;

  /**
   * Logging level for the SDK.
   * Controls the verbosity of log messages output by the SDK.
   * @default 'info'
   */
  logLevel?: 'debug' | 'info' | 'warn' | 'error';

  /**
   * Maximum number of conversation turns before the session automatically terminates.
   * A turn consists of a user message and an assistant response.
   * @default -1 (unlimited)
   */
  maxSessionTurns?: number;

  /**
   * Equivalent to `tool.core` in settings.json.
   * List of core tools to enable for the session.
   * If specified, only these tools will be available to the AI.
   * @example ['read_file', 'write_file', 'run_terminal_cmd']
   */
  coreTools?: string[];

  /**
   * Equivalent to `tool.exclude` in settings.json.
   * List of tools to exclude from the session.
   * These tools will not be available to the AI, even if they are core tools.
   * @example ['run_terminal_cmd', 'delete_file']
   */
  excludeTools?: string[];

  /**
   * Authentication type for the AI service.
   * - 'openai': Use OpenAI-compatible authentication
   * - 'qwen-oauth': Use Qwen OAuth authentication
   *
   * Though we support 'qwen-oauth', it's not recommended to use it in the SDK.
   * Because the credentials are stored in `~/.qwen` and may need to refresh periodically.
   */
  authType?: 'openai' | 'qwen-oauth';

  /**
   * Configuration for subagents that can be invoked during the session.
   * Subagents are specialized AI agents that can handle specific tasks or domains.
   * The invocation is marked as a `task` tool use with the name of agent and a tool_use_id.
   * The tool use of these agent is marked with the parent_tool_use_id of the `task` tool use.
   */
  agents?: SubagentConfig[];

  /**
   * Include partial messages in the response stream.
   * When true, the SDK will emit incomplete messages as they are being generated,
   * allowing for real-time streaming of the AI's response.
   * @default false
   */
  includePartialMessages?: boolean;
}
