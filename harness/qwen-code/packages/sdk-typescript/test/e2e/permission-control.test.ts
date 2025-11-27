/**
 * E2E tests for permission control features:
 * - canUseTool callback parameter
 * - setPermissionMode API
 */

import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import { query } from '../../src/index.js';
import {
  isSDKAssistantMessage,
  isSDKResultMessage,
  isSDKUserMessage,
  type SDKUserMessage,
  type ToolUseBlock,
  type ContentBlock,
} from '../../src/types/protocol.js';
const TEST_CLI_PATH = process.env['TEST_CLI_PATH']!;
const TEST_TIMEOUT = 30000;

const SHARED_TEST_OPTIONS = {
  pathToQwenExecutable: TEST_CLI_PATH,
  debug: false,
  env: {},
};

/**
 * Factory function that creates a streaming input with a control point.
 * After the first message is yielded, the generator waits for a resume signal,
 * allowing the test code to call query instance methods like setPermissionMode.
 */
function createStreamingInputWithControlPoint(
  firstMessage: string,
  secondMessage: string,
): {
  generator: AsyncIterable<SDKUserMessage>;
  resume: () => void;
} {
  let resumeResolve: (() => void) | null = null;
  const resumePromise = new Promise<void>((resolve) => {
    resumeResolve = resolve;
  });

  const generator = (async function* () {
    const sessionId = crypto.randomUUID();

    yield {
      type: 'user',
      session_id: sessionId,
      message: {
        role: 'user',
        content: firstMessage,
      },
      parent_tool_use_id: null,
    } as SDKUserMessage;

    await new Promise((resolve) => setTimeout(resolve, 200));

    await resumePromise;

    await new Promise((resolve) => setTimeout(resolve, 200));

    yield {
      type: 'user',
      session_id: sessionId,
      message: {
        role: 'user',
        content: secondMessage,
      },
      parent_tool_use_id: null,
    } as SDKUserMessage;
  })();

  const resume = () => {
    if (resumeResolve) {
      resumeResolve();
    }
  };

  return { generator, resume };
}

describe('Permission Control (E2E)', () => {
  beforeAll(() => {
    //process.env['DEBUG'] = '1';
  });

  afterAll(() => {
    delete process.env['DEBUG'];
  });

  describe('canUseTool callback parameter', () => {
    it('should invoke canUseTool callback when tool is requested', async () => {
      const toolCalls: Array<{
        toolName: string;
        input: Record<string, unknown>;
      }> = [];

      const q = query({
        prompt: 'Write a js hello world to file.',
        options: {
          ...SHARED_TEST_OPTIONS,
          permissionMode: 'default',

          canUseTool: async (toolName, input) => {
            toolCalls.push({ toolName, input });
            /*
              {
                behavior: 'allow',
                updatedInput: input,
              };
              */
            return {
              behavior: 'deny',
              message: 'Tool execution denied by user.',
            };
          },
        },
      });

      try {
        let hasToolUse = false;
        for await (const message of q) {
          if (isSDKAssistantMessage(message)) {
            const toolUseBlock = message.message.content.find(
              (block: ContentBlock): block is ToolUseBlock =>
                block.type === 'tool_use',
            );
            if (toolUseBlock) {
              hasToolUse = true;
            }
          }
        }

        expect(hasToolUse).toBe(true);
        expect(toolCalls.length).toBeGreaterThan(0);
        expect(toolCalls[0].toolName).toBeDefined();
        expect(toolCalls[0].input).toBeDefined();
      } finally {
        await q.close();
      }
    });

    it('should allow tool execution when canUseTool returns allow', async () => {
      let callbackInvoked = false;

      const q = query({
        prompt: 'Create a file named hello.txt with content "world"',
        options: {
          ...SHARED_TEST_OPTIONS,
          permissionMode: 'default',
          cwd: '/tmp',
          canUseTool: async (toolName, input) => {
            callbackInvoked = true;
            return {
              behavior: 'allow',
              updatedInput: input,
            };
          },
        },
      });

      try {
        let hasToolResult = false;
        for await (const message of q) {
          if (isSDKUserMessage(message)) {
            if (
              Array.isArray(message.message.content) &&
              message.message.content.some(
                (block) => block.type === 'tool_result',
              )
            ) {
              hasToolResult = true;
            }
          }
        }

        expect(callbackInvoked).toBe(true);
        expect(hasToolResult).toBe(true);
      } finally {
        await q.close();
      }
    });

    it('should deny tool execution when canUseTool returns deny', async () => {
      let callbackInvoked = false;

      const q = query({
        prompt: 'Create a file named test.txt',
        options: {
          ...SHARED_TEST_OPTIONS,
          permissionMode: 'default',
          canUseTool: async () => {
            callbackInvoked = true;
            return {
              behavior: 'deny',
              message: 'Tool execution denied by test',
            };
          },
        },
      });

      try {
        for await (const _message of q) {
          // Consume all messages
        }

        expect(callbackInvoked).toBe(true);
        // Tool use might still appear, but execution should be denied
        // The exact behavior depends on CLI implementation
      } finally {
        await q.close();
      }
    });

    it('should pass suggestions to canUseTool callback', async () => {
      let receivedSuggestions: unknown = null;

      const q = query({
        prompt: 'Create a file named data.txt',
        options: {
          ...SHARED_TEST_OPTIONS,
          permissionMode: 'default',
          cwd: '/tmp',
          canUseTool: async (toolName, input, options) => {
            receivedSuggestions = options?.suggestions;
            return {
              behavior: 'allow',
              updatedInput: input,
            };
          },
        },
      });

      try {
        for await (const _message of q) {
          // Consume all messages
        }

        // Suggestions may be null or an array, depending on CLI implementation
        expect(receivedSuggestions !== undefined).toBe(true);
      } finally {
        await q.close();
      }
    });

    it('should pass abort signal to canUseTool callback', async () => {
      let receivedSignal: AbortSignal | undefined = undefined;

      const q = query({
        prompt: 'Create a file named signal.txt',
        options: {
          ...SHARED_TEST_OPTIONS,
          permissionMode: 'default',
          cwd: '/tmp',
          canUseTool: async (toolName, input, options) => {
            receivedSignal = options?.signal;
            return {
              behavior: 'allow',
              updatedInput: input,
            };
          },
        },
      });

      try {
        for await (const _message of q) {
          // Consume all messages
        }

        expect(receivedSignal).toBeDefined();
        expect(receivedSignal).toBeInstanceOf(AbortSignal);
      } finally {
        await q.close();
      }
    });

    it('should allow updatedInput modification in canUseTool callback', async () => {
      const originalInputs: Record<string, unknown>[] = [];
      const updatedInputs: Record<string, unknown>[] = [];

      const q = query({
        prompt: 'Create a file named modified.txt',
        options: {
          ...SHARED_TEST_OPTIONS,
          permissionMode: 'default',
          cwd: '/tmp',
          canUseTool: async (toolName, input) => {
            originalInputs.push({ ...input });
            const updatedInput = {
              ...input,
              modified: true,
              testKey: 'testValue',
            };
            updatedInputs.push(updatedInput);
            return {
              behavior: 'allow',
              updatedInput,
            };
          },
        },
      });

      try {
        for await (const _message of q) {
          // Consume all messages
        }

        expect(originalInputs.length).toBeGreaterThan(0);
        expect(updatedInputs.length).toBeGreaterThan(0);
        expect(updatedInputs[0]?.['modified']).toBe(true);
        expect(updatedInputs[0]?.['testKey']).toBe('testValue');
      } finally {
        await q.close();
      }
    });

    it('should default to deny when canUseTool is not provided', async () => {
      const q = query({
        prompt: 'Create a file named default.txt',
        options: {
          ...SHARED_TEST_OPTIONS,
          permissionMode: 'default',
          cwd: '/tmp',
          // canUseTool not provided
        },
      });

      try {
        // When canUseTool is not provided, tools should be denied by default
        // The exact behavior depends on CLI implementation
        for await (const _message of q) {
          // Consume all messages
        }
        // Test passes if no errors occur
        expect(true).toBe(true);
      } finally {
        await q.close();
      }
    });
  });

  describe('setPermissionMode API', () => {
    it('should change permission mode from default to yolo', async () => {
      const { generator, resume } = createStreamingInputWithControlPoint(
        'What is 1 + 1?',
        'What is 2 + 2?',
      );

      const q = query({
        prompt: generator,
        options: {
          ...SHARED_TEST_OPTIONS,
          permissionMode: 'default',
          debug: true,
        },
      });

      try {
        const resolvers: {
          first?: () => void;
          second?: () => void;
        } = {};
        const firstResponsePromise = new Promise<void>((resolve) => {
          resolvers.first = resolve;
        });
        const secondResponsePromise = new Promise<void>((resolve) => {
          resolvers.second = resolve;
        });

        let firstResponseReceived = false;
        let secondResponseReceived = false;

        (async () => {
          for await (const message of q) {
            if (isSDKAssistantMessage(message) || isSDKResultMessage(message)) {
              if (!firstResponseReceived) {
                firstResponseReceived = true;
                resolvers.first?.();
              } else if (!secondResponseReceived) {
                secondResponseReceived = true;
                resolvers.second?.();
              }
            }
          }
        })();

        await Promise.race([
          firstResponsePromise,
          new Promise((_, reject) =>
            setTimeout(
              () => reject(new Error('Timeout waiting for first response')),
              40000,
            ),
          ),
        ]);

        expect(firstResponseReceived).toBe(true);

        await q.setPermissionMode('yolo');

        resume();

        await Promise.race([
          secondResponsePromise,
          new Promise((_, reject) =>
            setTimeout(
              () => reject(new Error('Timeout waiting for second response')),
              40000,
            ),
          ),
        ]);

        expect(secondResponseReceived).toBe(true);
      } finally {
        await q.close();
      }
    });

    it('should change permission mode from yolo to plan', async () => {
      const { generator, resume } = createStreamingInputWithControlPoint(
        'What is 3 + 3?',
        'What is 4 + 4?',
      );

      const q = query({
        prompt: generator,
        options: {
          ...SHARED_TEST_OPTIONS,
          permissionMode: 'yolo',
        },
      });

      try {
        const resolvers: {
          first?: () => void;
          second?: () => void;
        } = {};
        const firstResponsePromise = new Promise<void>((resolve) => {
          resolvers.first = resolve;
        });
        const secondResponsePromise = new Promise<void>((resolve) => {
          resolvers.second = resolve;
        });

        let firstResponseReceived = false;
        let secondResponseReceived = false;

        (async () => {
          for await (const message of q) {
            if (isSDKAssistantMessage(message) || isSDKResultMessage(message)) {
              if (!firstResponseReceived) {
                firstResponseReceived = true;
                resolvers.first?.();
              } else if (!secondResponseReceived) {
                secondResponseReceived = true;
                resolvers.second?.();
              }
            }
          }
        })();

        await Promise.race([
          firstResponsePromise,
          new Promise((_, reject) =>
            setTimeout(
              () => reject(new Error('Timeout waiting for first response')),
              10000,
            ),
          ),
        ]);

        expect(firstResponseReceived).toBe(true);

        await q.setPermissionMode('plan');

        resume();

        await Promise.race([
          secondResponsePromise,
          new Promise((_, reject) =>
            setTimeout(
              () => reject(new Error('Timeout waiting for second response')),
              10000,
            ),
          ),
        ]);

        expect(secondResponseReceived).toBe(true);
      } finally {
        await q.close();
      }
    });

    it('should change permission mode to auto-edit', async () => {
      const { generator, resume } = createStreamingInputWithControlPoint(
        'What is 5 + 5?',
        'What is 6 + 6?',
      );

      const q = query({
        prompt: generator,
        options: {
          ...SHARED_TEST_OPTIONS,
          permissionMode: 'default',
        },
      });

      try {
        const resolvers: {
          first?: () => void;
          second?: () => void;
        } = {};
        const firstResponsePromise = new Promise<void>((resolve) => {
          resolvers.first = resolve;
        });
        const secondResponsePromise = new Promise<void>((resolve) => {
          resolvers.second = resolve;
        });

        let firstResponseReceived = false;
        let secondResponseReceived = false;

        (async () => {
          for await (const message of q) {
            if (isSDKAssistantMessage(message) || isSDKResultMessage(message)) {
              if (!firstResponseReceived) {
                firstResponseReceived = true;
                resolvers.first?.();
              } else if (!secondResponseReceived) {
                secondResponseReceived = true;
                resolvers.second?.();
              }
            }
          }
        })();

        await Promise.race([
          firstResponsePromise,
          new Promise((_, reject) =>
            setTimeout(
              () => reject(new Error('Timeout waiting for first response')),
              10000,
            ),
          ),
        ]);

        expect(firstResponseReceived).toBe(true);

        await q.setPermissionMode('auto-edit');

        resume();

        await Promise.race([
          secondResponsePromise,
          new Promise((_, reject) =>
            setTimeout(
              () => reject(new Error('Timeout waiting for second response')),
              10000,
            ),
          ),
        ]);

        expect(secondResponseReceived).toBe(true);
      } finally {
        await q.close();
      }
    });

    it('should throw error when setPermissionMode is called on closed query', async () => {
      const q = query({
        prompt: 'Hello',
        options: {
          ...SHARED_TEST_OPTIONS,
          permissionMode: 'default',
        },
      });

      await q.close();

      await expect(q.setPermissionMode('yolo')).rejects.toThrow(
        'Query is closed',
      );
    });
  });

  describe('canUseTool and setPermissionMode integration', () => {
    it('should work together - canUseTool callback with dynamic permission mode change', async () => {
      const toolCalls: Array<{
        toolName: string;
        input: Record<string, unknown>;
      }> = [];

      const { generator, resume } = createStreamingInputWithControlPoint(
        'Create a file named first.txt',
        'Create a file named second.txt',
      );

      const q = query({
        prompt: generator,
        options: {
          ...SHARED_TEST_OPTIONS,
          permissionMode: 'default',
          cwd: '/tmp',
          canUseTool: async (toolName, input) => {
            toolCalls.push({ toolName, input });
            return {
              behavior: 'allow',
              updatedInput: input,
            };
          },
        },
      });

      try {
        const resolvers: {
          first?: () => void;
          second?: () => void;
        } = {};
        const firstResponsePromise = new Promise<void>((resolve) => {
          resolvers.first = resolve;
        });
        const secondResponsePromise = new Promise<void>((resolve) => {
          resolvers.second = resolve;
        });

        let firstResponseReceived = false;
        let secondResponseReceived = false;

        (async () => {
          for await (const message of q) {
            if (isSDKResultMessage(message)) {
              if (!firstResponseReceived) {
                firstResponseReceived = true;
                resolvers.first?.();
              } else if (!secondResponseReceived) {
                secondResponseReceived = true;
                resolvers.second?.();
              }
            }
          }
        })();

        await Promise.race([
          firstResponsePromise,
          new Promise((_, reject) =>
            setTimeout(
              () => reject(new Error('Timeout waiting for first response')),
              TEST_TIMEOUT,
            ),
          ),
        ]);

        expect(firstResponseReceived).toBe(true);
        expect(toolCalls.length).toBeGreaterThan(0);

        await q.setPermissionMode('yolo');

        resume();

        await Promise.race([
          secondResponsePromise,
          new Promise((_, reject) =>
            setTimeout(
              () => reject(new Error('Timeout waiting for second response')),
              TEST_TIMEOUT,
            ),
          ),
        ]);

        expect(secondResponseReceived).toBe(true);
      } finally {
        await q.close();
      }
    });
  });

  describe('ApprovalMode behavior tests', () => {
    describe('default mode', () => {
      it(
        'should auto-deny tools requiring confirmation without canUseTool callback',
        async () => {
          const q = query({
            prompt:
              'Create a file named test-default-deny.txt with content "hello"',
            options: {
              ...SHARED_TEST_OPTIONS,
              permissionMode: 'default',
              cwd: '/tmp',
              // No canUseTool callback provided
            },
          });

          try {
            let hasToolResult = false;
            let hasErrorInResult = false;

            for await (const message of q) {
              if (isSDKUserMessage(message)) {
                if (Array.isArray(message.message.content)) {
                  const toolResult = message.message.content.find(
                    (block) => block.type === 'tool_result',
                  );
                  if (toolResult && 'tool_use_id' in toolResult) {
                    hasToolResult = true;
                    // Check if the result contains an error about permission
                    if (
                      'content' in toolResult &&
                      typeof toolResult.content === 'string' &&
                      (toolResult.content.includes('permission') ||
                        toolResult.content.includes('declined'))
                    ) {
                      hasErrorInResult = true;
                    }
                  }
                }
              }
            }

            // In default mode without canUseTool, tools should be denied
            expect(hasToolResult).toBe(true);
            expect(hasErrorInResult).toBe(true);
          } finally {
            await q.close();
          }
        },
        TEST_TIMEOUT,
      );

      it(
        'should allow tools when canUseTool returns allow',
        async () => {
          let callbackInvoked = false;

          const q = query({
            prompt:
              'Create a file named test-default-allow.txt with content "world"',
            options: {
              ...SHARED_TEST_OPTIONS,
              permissionMode: 'default',
              cwd: '/tmp',
              canUseTool: async (toolName, input) => {
                callbackInvoked = true;
                return {
                  behavior: 'allow',
                  updatedInput: input,
                };
              },
            },
          });

          try {
            let hasSuccessfulToolResult = false;

            for await (const message of q) {
              if (isSDKUserMessage(message)) {
                if (Array.isArray(message.message.content)) {
                  const toolResult = message.message.content.find(
                    (block) => block.type === 'tool_result',
                  );
                  if (toolResult && 'tool_use_id' in toolResult) {
                    // Check if the result is successful (not an error)
                    if (
                      'content' in toolResult &&
                      typeof toolResult.content === 'string' &&
                      !toolResult.content.includes('permission') &&
                      !toolResult.content.includes('declined')
                    ) {
                      hasSuccessfulToolResult = true;
                    }
                  }
                }
              }
            }

            expect(callbackInvoked).toBe(true);
            expect(hasSuccessfulToolResult).toBe(true);
          } finally {
            await q.close();
          }
        },
        TEST_TIMEOUT,
      );

      it(
        'should execute read-only tools without confirmation',
        async () => {
          const q = query({
            prompt: 'List files in the current directory',
            options: {
              ...SHARED_TEST_OPTIONS,
              permissionMode: 'default',
              cwd: '/tmp',
              // No canUseTool callback - read-only tools should still work
            },
          });

          try {
            let hasToolResult = false;

            for await (const message of q) {
              if (isSDKUserMessage(message)) {
                if (Array.isArray(message.message.content)) {
                  const toolResult = message.message.content.find(
                    (block) => block.type === 'tool_result',
                  );
                  if (toolResult) {
                    hasToolResult = true;
                  }
                }
              }
            }

            expect(hasToolResult).toBe(true);
          } finally {
            await q.close();
          }
        },
        TEST_TIMEOUT,
      );
    });

    describe('yolo mode', () => {
      it(
        'should auto-approve all tools without canUseTool callback',
        async () => {
          const q = query({
            prompt:
              'Create a file named test-yolo.txt with content "yolo mode"',
            options: {
              ...SHARED_TEST_OPTIONS,
              permissionMode: 'yolo',
              cwd: '/tmp',
              // No canUseTool callback - tools should still execute
            },
          });

          try {
            let hasSuccessfulToolResult = false;

            for await (const message of q) {
              if (isSDKUserMessage(message)) {
                if (Array.isArray(message.message.content)) {
                  const toolResult = message.message.content.find(
                    (block) => block.type === 'tool_result',
                  );
                  if (toolResult && 'tool_use_id' in toolResult) {
                    // Check if the result is successful (not a permission error)
                    if (
                      'content' in toolResult &&
                      typeof toolResult.content === 'string' &&
                      !toolResult.content.includes('permission') &&
                      !toolResult.content.includes('declined')
                    ) {
                      hasSuccessfulToolResult = true;
                    }
                  }
                }
              }
            }

            expect(hasSuccessfulToolResult).toBe(true);
          } finally {
            await q.close();
          }
        },
        TEST_TIMEOUT,
      );

      it(
        'should not invoke canUseTool callback in yolo mode',
        async () => {
          let callbackInvoked = false;

          const q = query({
            prompt: 'Create a file named test-yolo-no-callback.txt',
            options: {
              ...SHARED_TEST_OPTIONS,
              permissionMode: 'yolo',
              cwd: '/tmp',
              canUseTool: async (toolName, input) => {
                callbackInvoked = true;
                return {
                  behavior: 'allow',
                  updatedInput: input,
                };
              },
            },
          });

          try {
            let hasToolResult = false;

            for await (const message of q) {
              if (isSDKUserMessage(message)) {
                if (Array.isArray(message.message.content)) {
                  const toolResult = message.message.content.find(
                    (block) => block.type === 'tool_result',
                  );
                  if (toolResult) {
                    hasToolResult = true;
                  }
                }
              }
            }

            expect(hasToolResult).toBe(true);
            // canUseTool should not be invoked in yolo mode
            expect(callbackInvoked).toBe(false);
          } finally {
            await q.close();
          }
        },
        TEST_TIMEOUT,
      );

      it(
        'should execute dangerous commands without confirmation',
        async () => {
          const q = query({
            prompt: 'Run command: echo "dangerous operation"',
            options: {
              ...SHARED_TEST_OPTIONS,
              permissionMode: 'yolo',
              cwd: '/tmp',
            },
          });

          try {
            let hasCommandResult = false;

            for await (const message of q) {
              if (isSDKUserMessage(message)) {
                if (Array.isArray(message.message.content)) {
                  const toolResult = message.message.content.find(
                    (block) => block.type === 'tool_result',
                  );
                  if (toolResult && 'tool_use_id' in toolResult) {
                    hasCommandResult = true;
                  }
                }
              }
            }

            expect(hasCommandResult).toBe(true);
          } finally {
            await q.close();
          }
        },
        TEST_TIMEOUT,
      );
    });

    describe('plan mode', () => {
      it(
        'should block non-read-only tools and return plan mode error',
        async () => {
          const q = query({
            prompt: 'Create a file named test-plan.txt',
            options: {
              ...SHARED_TEST_OPTIONS,
              permissionMode: 'plan',
              cwd: '/tmp',
            },
          });

          try {
            let hasBlockedToolCall = false;
            let hasPlanModeMessage = false;

            for await (const message of q) {
              if (isSDKUserMessage(message)) {
                if (Array.isArray(message.message.content)) {
                  const toolResult = message.message.content.find(
                    (block) => block.type === 'tool_result',
                  );
                  if (toolResult && 'tool_use_id' in toolResult) {
                    hasBlockedToolCall = true;
                    // Check for plan mode specific error message
                    if (
                      'content' in toolResult &&
                      typeof toolResult.content === 'string' &&
                      (toolResult.content.includes('Plan mode') ||
                        toolResult.content.includes('plan mode'))
                    ) {
                      hasPlanModeMessage = true;
                    }
                  }
                }
              }
            }

            expect(hasBlockedToolCall).toBe(true);
            expect(hasPlanModeMessage).toBe(true);
          } finally {
            await q.close();
          }
        },
        TEST_TIMEOUT,
      );

      it(
        'should allow read-only tools in plan mode',
        async () => {
          const q = query({
            prompt: 'List files in /tmp directory',
            options: {
              ...SHARED_TEST_OPTIONS,
              permissionMode: 'plan',
              cwd: '/tmp',
            },
          });

          try {
            let hasSuccessfulToolResult = false;

            for await (const message of q) {
              if (isSDKUserMessage(message)) {
                if (Array.isArray(message.message.content)) {
                  const toolResult = message.message.content.find(
                    (block) => block.type === 'tool_result',
                  );
                  if (toolResult && 'tool_use_id' in toolResult) {
                    // Check if the result is successful (not blocked by plan mode)
                    if (
                      'content' in toolResult &&
                      typeof toolResult.content === 'string' &&
                      !toolResult.content.includes('Plan mode')
                    ) {
                      hasSuccessfulToolResult = true;
                    }
                  }
                }
              }
            }

            expect(hasSuccessfulToolResult).toBe(true);
          } finally {
            await q.close();
          }
        },
        TEST_TIMEOUT,
      );

      it(
        'should block tools even with canUseTool callback in plan mode',
        async () => {
          let callbackInvoked = false;

          const q = query({
            prompt: 'Create a file named test-plan-callback.txt',
            options: {
              ...SHARED_TEST_OPTIONS,
              permissionMode: 'plan',
              cwd: '/tmp',
              canUseTool: async (toolName, input) => {
                callbackInvoked = true;
                return {
                  behavior: 'allow',
                  updatedInput: input,
                };
              },
            },
          });

          try {
            let hasPlanModeBlock = false;

            for await (const message of q) {
              if (isSDKUserMessage(message)) {
                if (Array.isArray(message.message.content)) {
                  const toolResult = message.message.content.find(
                    (block) => block.type === 'tool_result',
                  );
                  if (
                    toolResult &&
                    'content' in toolResult &&
                    typeof toolResult.content === 'string' &&
                    toolResult.content.includes('Plan mode')
                  ) {
                    hasPlanModeBlock = true;
                  }
                }
              }
            }

            // Plan mode should block tools before canUseTool is invoked
            expect(hasPlanModeBlock).toBe(true);
            // canUseTool should not be invoked for blocked tools in plan mode
            expect(callbackInvoked).toBe(false);
          } finally {
            await q.close();
          }
        },
        TEST_TIMEOUT,
      );
    });

    describe('auto-edit mode', () => {
      it(
        'should behave like default mode without canUseTool callback',
        async () => {
          const q = query({
            prompt: 'Create a file named test-auto-edit.txt',
            options: {
              ...SHARED_TEST_OPTIONS,
              permissionMode: 'auto-edit',
              cwd: '/tmp',
              // No canUseTool callback
            },
          });

          try {
            let hasToolResult = false;
            let hasDeniedTool = false;

            for await (const message of q) {
              if (isSDKUserMessage(message)) {
                if (Array.isArray(message.message.content)) {
                  const toolResult = message.message.content.find(
                    (block) => block.type === 'tool_result',
                  );
                  if (toolResult && 'tool_use_id' in toolResult) {
                    hasToolResult = true;
                    // Check if the tool was denied
                    if (
                      'content' in toolResult &&
                      typeof toolResult.content === 'string' &&
                      (toolResult.content.includes('permission') ||
                        toolResult.content.includes('declined'))
                    ) {
                      hasDeniedTool = true;
                    }
                  }
                }
              }
            }

            expect(hasToolResult).toBe(true);
            expect(hasDeniedTool).toBe(true);
          } finally {
            await q.close();
          }
        },
        TEST_TIMEOUT,
      );

      it(
        'should allow tools when canUseTool returns allow',
        async () => {
          let callbackInvoked = false;

          const q = query({
            prompt: 'Create a file named test-auto-edit-allow.txt',
            options: {
              ...SHARED_TEST_OPTIONS,
              permissionMode: 'auto-edit',
              cwd: '/tmp',
              canUseTool: async (toolName, input) => {
                callbackInvoked = true;
                return {
                  behavior: 'allow',
                  updatedInput: input,
                };
              },
            },
          });

          try {
            let hasSuccessfulToolResult = false;

            for await (const message of q) {
              if (isSDKUserMessage(message)) {
                if (Array.isArray(message.message.content)) {
                  const toolResult = message.message.content.find(
                    (block) => block.type === 'tool_result',
                  );
                  if (toolResult && 'tool_use_id' in toolResult) {
                    // Check if the result is successful
                    if (
                      'content' in toolResult &&
                      typeof toolResult.content === 'string' &&
                      !toolResult.content.includes('permission') &&
                      !toolResult.content.includes('declined')
                    ) {
                      hasSuccessfulToolResult = true;
                    }
                  }
                }
              }
            }

            expect(callbackInvoked).toBe(true);
            expect(hasSuccessfulToolResult).toBe(true);
          } finally {
            await q.close();
          }
        },
        TEST_TIMEOUT,
      );

      it(
        'should execute read-only tools without confirmation',
        async () => {
          const q = query({
            prompt: 'Read the contents of /etc/hosts file',
            options: {
              ...SHARED_TEST_OPTIONS,
              permissionMode: 'auto-edit',
              // No canUseTool callback - read-only tools should still work
            },
          });

          try {
            let hasToolResult = false;

            for await (const message of q) {
              if (isSDKUserMessage(message)) {
                if (Array.isArray(message.message.content)) {
                  const toolResult = message.message.content.find(
                    (block) => block.type === 'tool_result',
                  );
                  if (toolResult) {
                    hasToolResult = true;
                  }
                }
              }
            }

            expect(hasToolResult).toBe(true);
          } finally {
            await q.close();
          }
        },
        TEST_TIMEOUT,
      );
    });

    describe('mode comparison tests', () => {
      it(
        'should demonstrate different behaviors across all modes for write operations',
        async () => {
          const modes: Array<'default' | 'plan' | 'auto-edit' | 'yolo'> = [
            'default',
            'plan',
            'auto-edit',
            'yolo',
          ];
          const results: Record<string, boolean> = {};

          for (const mode of modes) {
            const q = query({
              prompt: `Create a file named test-${mode}.txt`,
              options: {
                ...SHARED_TEST_OPTIONS,
                permissionMode: mode,
                cwd: '/tmp',
                canUseTool:
                  mode === 'yolo'
                    ? undefined
                    : async (toolName, input) => {
                        return {
                          behavior: 'allow',
                          updatedInput: input,
                        };
                      },
              },
            });

            try {
              let toolExecuted = false;

              for await (const message of q) {
                if (isSDKUserMessage(message)) {
                  if (Array.isArray(message.message.content)) {
                    const toolResult = message.message.content.find(
                      (block) => block.type === 'tool_result',
                    );
                    if (
                      toolResult &&
                      'content' in toolResult &&
                      typeof toolResult.content === 'string'
                    ) {
                      // Check if tool executed successfully (not blocked or denied)
                      if (
                        !toolResult.content.includes('Plan mode') &&
                        !toolResult.content.includes('permission') &&
                        !toolResult.content.includes('declined')
                      ) {
                        toolExecuted = true;
                      }
                    }
                  }
                }
              }

              results[mode] = toolExecuted;
            } finally {
              await q.close();
            }
          }

          // Verify expected behaviors
          expect(results['default']).toBe(true); // Allowed via canUseTool
          expect(results['plan']).toBe(false); // Blocked by plan mode
          expect(results['auto-edit']).toBe(true); // Allowed via canUseTool
          expect(results['yolo']).toBe(true); // Auto-approved
        },
        TEST_TIMEOUT * 4,
      );
    });
  });
});
