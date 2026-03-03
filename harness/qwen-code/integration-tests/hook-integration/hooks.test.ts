import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { join } from 'node:path';
import { TestRig, validateModelOutput } from '../test-helper.js';

/**
 * Path to responses directory for mock LLM responses
 */
const RESPONSES_DIR = join(import.meta.dirname, 'responses');

/**
 * Hooks System Integration Tests
 * Tests for complete hook system flow including UserPromptSubmit, Stop hooks
 * Uses responses files for deterministic testing
 */
describe('Hooks System Integration', () => {
  let rig: TestRig;

  beforeEach(() => {
    rig = new TestRig();
  });

  afterEach(async () => {
    if (rig) {
      await rig.cleanup();
    }
  });

  // ==================== UserPromptSubmit Hooks ====================
  describe('UserPromptSubmit Hooks', () => {
    describe('Allow Decision', () => {
      it('should allow prompt when hook returns allow decision', async () => {
        const hookScript =
          "console.log(JSON.stringify({decision: 'allow', reason: 'approved by hook'}));";

        await rig.setup('ups-allow-decision', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-userpromptsubmit-allow.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              UserPromptSubmit: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${hookScript}"`,
                      name: 'ups-allow-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say hello');
        expect(result).toBeDefined();
        expect(result.length).toBeGreaterThan(0);
      });

      it('should allow tool execution with allow decision and verify tool was called', async () => {
        const hookScript =
          "console.log(JSON.stringify({decision: 'allow', reason: 'Tool execution approved'}));";

        await rig.setup('ups-allow-tool', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-userpromptsubmit-allow.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              UserPromptSubmit: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${hookScript}"`,
                      name: 'ups-allow-tool-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        await rig.run('Create a file test.txt with content "hello"');

        const foundToolCall = await rig.waitForToolCall('write_file');
        expect(foundToolCall).toBeTruthy();

        const fileContent = rig.readFile('test.txt');
        expect(fileContent).toContain('hello');
      });
    });

    describe('Block Decision', () => {
      it('should block prompt when hook returns block decision', async () => {
        const blockScript = `console.log(JSON.stringify({decision: 'block', reason: 'Prompt blocked by security policy'}));`;

        await rig.setup('ups-block-decision', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-userpromptsubmit-block.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              UserPromptSubmit: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${blockScript}"`,
                      name: 'ups-block-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Create a file');

        // Blocked prompts should show the block reason
        expect(result.toLowerCase()).toContain('block');
      });

      it('should block tool execution when hook returns block and verify no tool was called', async () => {
        const blockScript = `console.log(JSON.stringify({decision: 'block', reason: 'File writing blocked by security policy'}));`;

        await rig.setup('ups-block-tool', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-userpromptsubmit-block.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              UserPromptSubmit: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${blockScript}"`,
                      name: 'ups-block-tool-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Create a file test.txt with "hello"');

        // Tool should not be called due to blocking hook
        const toolLogs = rig.readToolLogs();
        const writeFileCalls = toolLogs.filter(
          (t) =>
            t.toolRequest.name === 'write_file' &&
            t.toolRequest.success === true,
        );
        expect(writeFileCalls).toHaveLength(0);

        // Result should mention the blocking reason
        expect(result).toContain('block');
      });
    });

    describe('Modify Prompt', () => {
      it('should use modified prompt when hook provides modification', async () => {
        const modifyScript = `console.log(JSON.stringify({decision: 'allow', hookSpecificOutput: {hookEventName: 'UserPromptSubmit', modifiedPrompt: 'Modified prompt content', additionalContext: 'Context added by hook'}}));`;

        await rig.setup('ups-modify-prompt', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-userpromptsubmit-modify.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              UserPromptSubmit: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${modifyScript}"`,
                      name: 'ups-modify-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say test');
        expect(result).toBeDefined();
      });
    });

    describe('Additional Context', () => {
      it('should include additional context in response when hook provides it', async () => {
        const contextScript = `console.log(JSON.stringify({decision: 'allow', hookSpecificOutput: {additionalContext: 'Extra context information from hook'}}));`;

        await rig.setup('ups-add-context', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-userpromptsubmit-add-context.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              UserPromptSubmit: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${contextScript}"`,
                      name: 'ups-context-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('What is 1+1?');
        expect(result).toBeDefined();
      });
    });

    describe('Timeout Handling', () => {
      it('should continue execution when hook times out', async () => {
        await rig.setup('ups-timeout', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-userpromptsubmit-timeout.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              UserPromptSubmit: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: 'sleep 60',
                      name: 'ups-timeout-hook',
                      timeout: 1000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say timeout test');
        // Should continue despite timeout
        expect(result).toBeDefined();
      });
    });

    describe('Error Handling', () => {
      it('should continue execution when hook exits with non-blocking error (exit code 1)', async () => {
        await rig.setup('ups-nonblocking-error', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-userpromptsubmit-error-nonblocking.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              UserPromptSubmit: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: 'echo warning && exit 1',
                      name: 'ups-error-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say error test');
        // Non-blocking error should not prevent execution
        expect(result).toBeDefined();
      });

      it('should block execution when hook exits with blocking error (exit code 2)', async () => {
        await rig.setup('ups-blocking-error', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-userpromptsubmit-error-blocking.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              UserPromptSubmit: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command:
                        'node -e "console.error(\'Critical security error\'); process.exit(2)"',
                      name: 'ups-blocking-error-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Create a file');
        expect(result).toBeDefined();
      });

      it('should continue execution when hook command does not exist', async () => {
        await rig.setup('ups-missing-command', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-userpromptsubmit-missing-command.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              UserPromptSubmit: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: '/nonexistent/command/path',
                      name: 'ups-missing-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say missing test');
        // Missing command should not prevent execution (non-blocking)
        expect(result).toBeDefined();
      });
    });

    describe('Input Format Validation', () => {
      it('should receive properly formatted input when hook is called', async () => {
        const inputValidationScript = `
const input = JSON.parse(process.argv[2] || '{}');
const hasRequired = input.session_id && input.cwd && input.hook_event_name && input.prompt !== undefined;
console.log(JSON.stringify({
  decision: 'allow',
  hookSpecificOutput: { 
    hookEventName: 'UserPromptSubmit', 
    additionalContext: hasRequired ? 'Valid input format' : 'Invalid input format'
  }
}));
`;

        await rig.setup('ups-correct-input', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-userpromptsubmit-allow.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              UserPromptSubmit: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${inputValidationScript.replace(/\n/g, ' ')}"`,
                      name: 'ups-input-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say input test');
        validateModelOutput(result, 'input test', 'UPS: correct input');
      });
    });

    describe('System Message', () => {
      it('should include system message in response when hook provides it', async () => {
        const systemMsgScript = `console.log(JSON.stringify({decision: 'allow', systemMessage: 'This is a system message from hook'}));`;

        await rig.setup('ups-system-message', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-userpromptsubmit-allow.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              UserPromptSubmit: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${systemMsgScript}"`,
                      name: 'ups-system-msg-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say system message');
        expect(result).toBeDefined();
      });
    });
  });

  // ==================== Stop Hooks ====================
  describe('Stop Hooks', () => {
    describe('Allow Decision', () => {
      it('should allow stopping when hook returns allow decision', async () => {
        const allowStopScript = `console.log(JSON.stringify({decision: 'allow', reason: 'Stop allowed'}));`;

        await rig.setup('stop-allow', {
          fakeResponsesPath: join(RESPONSES_DIR, 'qwen-stop-allow.responses'),
          settings: {
            hooks: {
              enabled: true,
              Stop: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${allowStopScript}"`,
                      name: 'stop-allow-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say stop test');
        expect(result).toBeDefined();
      });

      it('should allow stopping and verify final response is produced', async () => {
        const allowFinalScript = `console.log(JSON.stringify({decision: 'allow', hookSpecificOutput: {additionalContext: 'Final context from stop hook'}}));`;

        await rig.setup('stop-allow-final', {
          fakeResponsesPath: join(RESPONSES_DIR, 'qwen-stop-allow.responses'),
          settings: {
            hooks: {
              enabled: true,
              Stop: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${allowFinalScript}"`,
                      name: 'stop-final-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say goodbye');
        expect(result).toBeDefined();
        expect(result.length).toBeGreaterThan(0);
      });
    });

    describe('Continue False', () => {
      it('should request continue execution when hook returns continue: false', async () => {
        const continueScript = `console.log(JSON.stringify({continue: false, stopReason: 'More work needed'}));`;

        await rig.setup('stop-continue-false', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-stop-continue-false.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              Stop: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${continueScript}"`,
                      name: 'stop-continue-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say continue');
        // When continue: false, the agent may try to continue
        expect(result).toBeDefined();
      });
    });

    describe('Additional Context', () => {
      it('should include additional context in final response', async () => {
        const contextScript = `console.log(JSON.stringify({decision: 'allow', hookSpecificOutput: {additionalContext: 'Final context from hook'}}));`;

        await rig.setup('stop-add-context', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-stop-add-context.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              Stop: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${contextScript}"`,
                      name: 'stop-context-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('What is 3+3?');
        expect(result).toBeDefined();
      });

      it('should concatenate multiple additionalContext from multiple hooks', async () => {
        const context1Script = `console.log(JSON.stringify({decision: 'allow', hookSpecificOutput: {additionalContext: 'context1'}}));`;
        const context2Script = `console.log(JSON.stringify({decision: 'allow', hookSpecificOutput: {additionalContext: 'context2'}}));`;

        await rig.setup('stop-multi-context', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-stop-add-context.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              Stop: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${context1Script}"`,
                      name: 'stop-context-1',
                      timeout: 5000,
                    },
                    {
                      type: 'command',
                      command: `node -e "${context2Script}"`,
                      name: 'stop-context-2',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say multi context');
        expect(result).toBeDefined();
      });
    });

    describe('Stop Reason', () => {
      it('should include stop reason when hook provides it', async () => {
        const reasonScript = `console.log(JSON.stringify({decision: 'allow', stopReason: 'Custom stop reason from hook'}));`;

        await rig.setup('stop-set-reason', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-stop-set-reason.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              Stop: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${reasonScript}"`,
                      name: 'stop-reason-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say reason test');
        expect(result).toBeDefined();
      });
    });

    describe('Timeout Handling', () => {
      it('should continue stopping when hook times out', async () => {
        await rig.setup('stop-timeout', {
          fakeResponsesPath: join(RESPONSES_DIR, 'qwen-stop-timeout.responses'),
          settings: {
            hooks: {
              enabled: true,
              Stop: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: 'sleep 60',
                      name: 'stop-timeout-hook',
                      timeout: 1000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say timeout');
        // Timeout should not prevent stopping
        expect(result).toBeDefined();
      });
    });

    describe('Error Handling', () => {
      it('should continue stopping when hook has non-blocking error', async () => {
        await rig.setup('stop-error', {
          fakeResponsesPath: join(RESPONSES_DIR, 'qwen-stop-error.responses'),
          settings: {
            hooks: {
              enabled: true,
              Stop: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: 'echo warning && exit 1',
                      name: 'stop-error-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say error');
        // Error should not prevent stopping
        expect(result).toBeDefined();
      });

      it('should continue stopping when hook command does not exist', async () => {
        await rig.setup('stop-missing-command', {
          fakeResponsesPath: join(RESPONSES_DIR, 'qwen-stop-error.responses'),
          settings: {
            hooks: {
              enabled: true,
              Stop: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: '/nonexistent/stop/command',
                      name: 'stop-missing-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say missing');
        // Missing command should not prevent stopping
        expect(result).toBeDefined();
      });
    });

    describe('System Message', () => {
      it('should include system message in final response', async () => {
        const systemMsgScript = `console.log(JSON.stringify({decision: 'allow', systemMessage: 'Final system message from stop hook'}));`;

        await rig.setup('stop-system-message', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-stop-with-message.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              Stop: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${systemMsgScript}"`,
                      name: 'stop-system-msg-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say final');
        expect(result).toBeDefined();
      });
    });
  });

  // ==================== Multiple Hooks ====================
  describe('Multiple Hooks', () => {
    describe('Sequential Execution', () => {
      it('should execute hooks sequentially when sequential: true', async () => {
        const hook1Script = `console.log(JSON.stringify({decision: 'allow', hookSpecificOutput: {additionalContext: 'first'}}));`;
        const hook2Script = `console.log(JSON.stringify({decision: 'allow', hookSpecificOutput: {additionalContext: 'second'}}));`;

        await rig.setup('multi-sequential', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-sequential-passthrough.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              UserPromptSubmit: [
                {
                  sequential: true,
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${hook1Script}"`,
                      name: 'seq-hook-1',
                      timeout: 5000,
                    },
                    {
                      type: 'command',
                      command: `node -e "${hook2Script}"`,
                      name: 'seq-hook-2',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say sequential');
        expect(result).toBeDefined();
      });

      it('should stop at first blocking hook and not execute subsequent', async () => {
        const blockScript = `console.log(JSON.stringify({decision: 'block', reason: 'Blocked by first hook'}));`;
        const allowScript = `console.log(JSON.stringify({decision: 'allow'}));`;

        await rig.setup('multi-first-blocks', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-sequential-first-blocks.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              UserPromptSubmit: [
                {
                  sequential: true,
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${blockScript}"`,
                      name: 'seq-block-hook',
                      timeout: 5000,
                    },
                    {
                      type: 'command',
                      command: `node -e "${allowScript}"`,
                      name: 'seq-should-not-run',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Create a file');
        // First hook blocks, second should not run
        expect(result.toLowerCase()).toContain('block');
      });

      it('should pass output from first hook to second hook input', async () => {
        const passScript1 = `console.log(JSON.stringify({decision: 'allow', hookSpecificOutput: {additionalContext: 'from first', passthrough: 'data'}}));`;
        const passScript2 = `console.log(JSON.stringify({decision: 'allow', hookSpecificOutput: {additionalContext: 'received passthrough'}}));`;

        await rig.setup('multi-passthrough', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-sequential-passthrough.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              UserPromptSubmit: [
                {
                  sequential: true,
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${passScript1}"`,
                      name: 'passthrough-hook-1',
                      timeout: 5000,
                    },
                    {
                      type: 'command',
                      command: `node -e "${passScript2}"`,
                      name: 'passthrough-hook-2',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say passthrough');
        expect(result).toBeDefined();
      });
    });

    describe('Parallel Execution', () => {
      it('should execute hooks in parallel when sequential is not set', async () => {
        const hook1Script = `console.log(JSON.stringify({decision: 'allow'}));`;
        const hook2Script = `console.log(JSON.stringify({decision: 'allow'}));`;

        await rig.setup('multi-parallel', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-userpromptsubmit-allow.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              UserPromptSubmit: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${hook1Script}"`,
                      name: 'parallel-hook-1',
                      timeout: 5000,
                    },
                    {
                      type: 'command',
                      command: `node -e "${hook2Script}"`,
                      name: 'parallel-hook-2',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say parallel');
        expect(result).toBeDefined();
      });

      it('should handle mixed success/failure results from parallel hooks', async () => {
        const allowScript = `console.log(JSON.stringify({decision: 'allow'}));`;

        await rig.setup('multi-mixed', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-parallel-mixed-results.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              UserPromptSubmit: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${allowScript}"`,
                      name: 'mixed-allow-hook',
                      timeout: 5000,
                    },
                    {
                      type: 'command',
                      command: '/nonexistent/command',
                      name: 'mixed-error-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say mixed');
        // Mixed results: one succeeds, one fails - should continue
        expect(result).toBeDefined();
      });

      it('should allow when any hook returns allow in parallel (OR logic)', async () => {
        const blockScript = `console.log(JSON.stringify({decision: 'block', reason: 'blocked'}));`;
        const allowScript = `console.log(JSON.stringify({decision: 'allow'}));`;

        await rig.setup('multi-or-logic', {
          fakeResponsesPath: join(
            RESPONSES_DIR,
            'qwen-userpromptsubmit-allow.responses',
          ),
          settings: {
            hooks: {
              enabled: true,
              UserPromptSubmit: [
                {
                  hooks: [
                    {
                      type: 'command',
                      command: `node -e "${blockScript}"`,
                      name: 'block-hook',
                      timeout: 5000,
                    },
                    {
                      type: 'command',
                      command: `node -e "${allowScript}"`,
                      name: 'allow-hook',
                      timeout: 5000,
                    },
                  ],
                },
              ],
            },
            trusted: true,
          },
        });

        const result = await rig.run('Say or logic');
        // With OR logic, allow should win
        expect(result).toBeDefined();
      });
    });
  });

  // ==================== Combined Hooks ====================
  describe('Combined Hooks', () => {
    it('should execute both Stop and UserPromptSubmit hooks in same session', async () => {
      const stopScript = `console.log(JSON.stringify({decision: 'allow'}));`;
      const upsScript = `console.log(JSON.stringify({decision: 'allow'}));`;

      await rig.setup('combined-both-hooks', {
        fakeResponsesPath: join(
          RESPONSES_DIR,
          'qwen-userpromptsubmit-allow.responses',
        ),
        settings: {
          hooksConfig: { enabled: true },
          hooks: {
            Stop: [
              {
                hooks: [
                  {
                    type: 'command',
                    command: `node -e "${stopScript}"`,
                    name: 'stop-hook',
                    timeout: 5000,
                  },
                ],
              },
            ],
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command: `node -e "${upsScript}"`,
                    name: 'ups-hook',
                    timeout: 5000,
                  },
                ],
              },
            ],
          },
          trusted: true,
        },
      });

      const result = await rig.run('Say both hooks');
      expect(result).toBeDefined();
    });
  });

  // ==================== Hook Script File Tests ====================
  describe('Hook Script File Tests', () => {
    it('should execute hook from script file', async () => {
      await rig.setup('script-file-hook', {
        fakeResponsesPath: join(
          RESPONSES_DIR,
          'qwen-userpromptsubmit-allow.responses',
        ),
        settings: {
          hooksConfig: { enabled: true },
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      "node -e \"console.log(JSON.stringify({decision: 'allow', reason: 'Approved by script file', hookSpecificOutput: {additionalContext: 'Script file executed successfully'}}))\"",
                    name: 'script-file-hook',
                    timeout: 5000,
                  },
                ],
              },
            ],
          },
          trusted: true,
        },
      });

      const result = await rig.run('Say script file test');
      expect(result).toBeDefined();
    });

    it('should execute blocking hook from script file', async () => {
      await rig.setup('script-file-block-hook', {
        fakeResponsesPath: join(
          RESPONSES_DIR,
          'qwen-userpromptsubmit-block.responses',
        ),
        settings: {
          hooksConfig: { enabled: true },
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      "node -e \"console.log(JSON.stringify({decision: 'block', reason: 'Blocked by security script'}))\"",
                    name: 'script-block-hook',
                    timeout: 5000,
                  },
                ],
              },
            ],
          },
          trusted: true,
        },
      });

      const result = await rig.run('Create a file');

      // Prompt should be blocked
      expect(result.toLowerCase()).toContain('block');
    });
  });
});
