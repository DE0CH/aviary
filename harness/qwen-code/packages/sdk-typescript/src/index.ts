export { query } from './query/createQuery.js';
export { AbortError, isAbortError } from './types/errors.js';
export { Query } from './query/Query.js';
export { SdkLogger } from './utils/logger.js';

export type { QueryOptions } from './query/createQuery.js';
export type { LogLevel, LoggerConfig, ScopedLogger } from './utils/logger.js';

export type {
  ContentBlock,
  TextBlock,
  ThinkingBlock,
  ToolUseBlock,
  ToolResultBlock,
  CLIUserMessage,
  CLIAssistantMessage,
  CLISystemMessage,
  CLIResultMessage,
  CLIPartialAssistantMessage,
  CLIMessage,
} from './types/protocol.js';

export {
  isCLIUserMessage,
  isCLIAssistantMessage,
  isCLISystemMessage,
  isCLIResultMessage,
  isCLIPartialAssistantMessage,
} from './types/protocol.js';

export type {
  PermissionMode,
  CanUseTool,
  PermissionResult,
  ExternalMcpServerConfig,
  SdkMcpServerConfig,
} from './types/types.js';
