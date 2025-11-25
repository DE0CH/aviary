import type { PermissionMode, PermissionSuggestion } from './protocol.js';

export type { PermissionMode };

export type JSONSchema = {
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
