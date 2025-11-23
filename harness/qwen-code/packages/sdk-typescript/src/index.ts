export { query } from './query/createQuery.js';

export { Query } from './query/Query.js';

export type { ExternalMcpServerConfig } from './types/queryOptionsSchema.js';

export type { QueryOptions } from './query/createQuery.js';

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

export { AbortError, isAbortError } from './types/errors.js';

export { ControlRequestType } from './types/protocol.js';

export { ProcessTransport } from './transport/ProcessTransport.js';
export type { Transport } from './transport/Transport.js';

export { Stream } from './utils/Stream.js';
export {
  serializeJsonLine,
  parseJsonLineSafe,
  isValidMessage,
  parseJsonLinesStream,
} from './utils/jsonLines.js';
export {
  findCliPath,
  resolveCliPath,
  prepareSpawnInfo,
} from './utils/cliPath.js';
export type { SpawnInfo } from './utils/cliPath.js';

export { createSdkMcpServer } from './mcp/createSdkMcpServer.js';
export {
  tool,
  createTool,
  validateToolName,
  validateInputSchema,
} from './mcp/tool.js';

export type {
  JSONSchema,
  ToolDefinition,
  PermissionMode,
  CanUseTool,
  PermissionResult,
} from './types/types.js';
