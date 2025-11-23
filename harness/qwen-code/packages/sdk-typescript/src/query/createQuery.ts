/**
 * Factory function for creating Query instances.
 */

import type { CLIUserMessage } from '../types/protocol.js';
import { serializeJsonLine } from '../utils/jsonLines.js';
import { ProcessTransport } from '../transport/ProcessTransport.js';
import { parseExecutableSpec } from '../utils/cliPath.js';
import { Query } from './Query.js';
import {
  QueryOptionsSchema,
  type QueryOptions,
} from '../types/queryOptionsSchema.js';

export type { QueryOptions };

export function query({
  prompt,
  options = {},
}: {
  prompt: string | AsyncIterable<CLIUserMessage>;
  options?: QueryOptions;
}): Query {
  // Validate options and obtain normalized executable metadata
  const parsedExecutable = validateOptions(options);

  // Determine if this is a single-turn or multi-turn query
  // Single-turn: string prompt (simple Q&A)
  // Multi-turn: AsyncIterable prompt (streaming conversation)
  const isSingleTurn = typeof prompt === 'string';

  // Resolve CLI specification while preserving explicit runtime directives
  const pathToQwenExecutable =
    options.pathToQwenExecutable ?? parsedExecutable.executablePath;

  // Use provided abortController or create a new one
  const abortController = options.abortController ?? new AbortController();

  // Create transport with abortController
  const transport = new ProcessTransport({
    pathToQwenExecutable,
    cwd: options.cwd,
    model: options.model,
    permissionMode: options.permissionMode,
    mcpServers: options.mcpServers,
    env: options.env,
    abortController,
    debug: options.debug,
    stderr: options.stderr,
    maxSessionTurns: options.maxSessionTurns,
    coreTools: options.coreTools,
    excludeTools: options.excludeTools,
    authType: options.authType,
  });

  // Build query options with abortController
  const queryOptions: QueryOptions = {
    ...options,
    abortController,
  };

  // Create Query
  const queryInstance = new Query(transport, queryOptions, isSingleTurn);

  // Handle prompt based on type
  if (isSingleTurn) {
    // For single-turn queries, send the prompt directly via transport
    const stringPrompt = prompt as string;
    const message: CLIUserMessage = {
      type: 'user',
      session_id: queryInstance.getSessionId(),
      message: {
        role: 'user',
        content: stringPrompt,
      },
      parent_tool_use_id: null,
    };

    (async () => {
      try {
        await queryInstance.initialized;
        transport.write(serializeJsonLine(message));
      } catch (err) {
        console.error('[query] Error sending single-turn prompt:', err);
      }
    })();
  } else {
    queryInstance
      .streamInput(prompt as AsyncIterable<CLIUserMessage>)
      .catch((err) => {
        console.error('[query] Error streaming input:', err);
      });
  }

  return queryInstance;
}

/**
 * Backward compatibility alias
 * @deprecated Use query() instead
 */
export const createQuery = query;

function validateOptions(
  options: QueryOptions,
): ReturnType<typeof parseExecutableSpec> {
  // Validate options using Zod schema
  const validationResult = QueryOptionsSchema.safeParse(options);
  if (!validationResult.success) {
    const errors = validationResult.error.errors
      .map((err) => `${err.path.join('.')}: ${err.message}`)
      .join('; ');
    throw new Error(`Invalid QueryOptions: ${errors}`);
  }

  // Validate executable path early to provide clear error messages
  let parsedExecutable: ReturnType<typeof parseExecutableSpec>;
  try {
    parsedExecutable = parseExecutableSpec(options.pathToQwenExecutable);
  } catch (error) {
    const errorMessage = error instanceof Error ? error.message : String(error);
    throw new Error(`Invalid pathToQwenExecutable: ${errorMessage}`);
  }

  // Validate no MCP server name conflicts (cross-field validation not easily expressible in Zod)
  if (options.mcpServers && options.sdkMcpServers) {
    const externalNames = Object.keys(options.mcpServers);
    const sdkNames = Object.keys(options.sdkMcpServers);

    const conflicts = externalNames.filter((name) => sdkNames.includes(name));
    if (conflicts.length > 0) {
      throw new Error(
        `MCP server name conflicts between mcpServers and sdkMcpServers: ${conflicts.join(', ')}`,
      );
    }
  }

  return parsedExecutable;
}
