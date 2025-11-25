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
  const parsedExecutable = validateOptions(options);

  const isSingleTurn = typeof prompt === 'string';

  const pathToQwenExecutable =
    options.pathToQwenExecutable ?? parsedExecutable.executablePath;

  const abortController = options.abortController ?? new AbortController();

  const transport = new ProcessTransport({
    pathToQwenExecutable,
    cwd: options.cwd,
    model: options.model,
    permissionMode: options.permissionMode,
    env: options.env,
    abortController,
    debug: options.debug,
    stderr: options.stderr,
    maxSessionTurns: options.maxSessionTurns,
    coreTools: options.coreTools,
    excludeTools: options.excludeTools,
    authType: options.authType,
    includePartialMessages: options.includePartialMessages,
  });

  const queryOptions: QueryOptions = {
    ...options,
    abortController,
  };

  const queryInstance = new Query(transport, queryOptions, isSingleTurn);

  if (isSingleTurn) {
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

function validateOptions(
  options: QueryOptions,
): ReturnType<typeof parseExecutableSpec> {
  const validationResult = QueryOptionsSchema.safeParse(options);
  if (!validationResult.success) {
    const errors = validationResult.error.errors
      .map((err) => `${err.path.join('.')}: ${err.message}`)
      .join('; ');
    throw new Error(`Invalid QueryOptions: ${errors}`);
  }

  let parsedExecutable: ReturnType<typeof parseExecutableSpec>;
  try {
    parsedExecutable = parseExecutableSpec(options.pathToQwenExecutable);
  } catch (error) {
    const errorMessage = error instanceof Error ? error.message : String(error);
    throw new Error(`Invalid pathToQwenExecutable: ${errorMessage}`);
  }

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
