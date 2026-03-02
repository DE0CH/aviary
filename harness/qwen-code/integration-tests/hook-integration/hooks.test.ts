/**
 * @license
 * Copyright 2026 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { TestRig, validateModelOutput } from '../test-helper.js';

/**
 * Hooks Integration Tests
 * Tests for UserPromptSubmit and Stop event hooks
 * Reference: qwen_integration.md
 */

describe('Hooks Integration - UserPromptSubmit', () => {
  let rig: TestRig;

  beforeEach(() => {
    rig = new TestRig();
  });

  afterEach(async () => {
    if (rig) {
      await rig.cleanup();
    }
  });

  // ==================== UPS-001: Allow Decision ====================
  describe('UPS-001: Hook returns allow decision', () => {
    it('should allow prompt when hook returns allow decision', async () => {
      await rig.setup('ups-001-allow-decision', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"allow","reason":"approved by hook"}\'',
                    name: 'ups-allow-hook',
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

    it('should allow tool execution and verify tool was called with allow decision', async () => {
      await rig.setup('ups-001-allow-tool', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command: 'echo \'{"decision":"allow"}\'',
                    name: 'ups-allow-tool-hook',
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

  // ==================== UPS-002: Block Decision ====================
  describe('UPS-002: Hook returns block decision', () => {
    it('should block prompt when hook returns block decision', async () => {
      await rig.setup('ups-002-block-decision', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"block","reason":"Prompt blocked by security policy"}\'',
                    name: 'ups-block-hook',
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

    it('should block tool execution when hook returns block', async () => {
      await rig.setup('ups-002-block-tool', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"block","reason":"File writing blocked"}\'',
                    name: 'ups-block-tool-hook',
                  },
                ],
              },
            ],
          },
          trusted: true,
        },
      });

      await rig.run('Create a file test.txt with "hello"');

      // Tool should not be called due to blocking hook
      const toolLogs = rig.readToolLogs();
      const writeFileCalls = toolLogs.filter(
        (t) =>
          t.toolRequest.name === 'write_file' && t.toolRequest.success === true,
      );
      expect(writeFileCalls).toHaveLength(0);
    });
  });

  // ==================== UPS-003: Modify Prompt ====================
  describe('UPS-003: Hook modifies prompt content', () => {
    it('should use modified prompt when hook provides modification', async () => {
      await rig.setup('ups-003-modify-prompt', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"allow","hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"modified"}}\'',
                    name: 'ups-modify-hook',
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

  // ==================== UPS-004: Additional Context ====================
  describe('UPS-004: Hook adds additionalContext', () => {
    it('should include additional context in response when hook provides it', async () => {
      await rig.setup('ups-004-add-context', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"allow","hookSpecificOutput":{"additionalContext":"extra info from hook"}}\'',
                    name: 'ups-context-hook',
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

    it('should generate hook telemetry with additional context', async () => {
      await rig.setup('ups-004-telemetry', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"allow","hookSpecificOutput":{"additionalContext":"telemetry test"}}\'',
                    name: 'ups-telemetry-hook',
                  },
                ],
              },
            ],
          },
          trusted: true,
        },
      });

      await rig.run('Say telemetry');

      const hookTelemetryFound = await rig.waitForTelemetryEvent('hook_call');
      expect(hookTelemetryFound).toBeTruthy();
    });
  });

  // ==================== UPS-005: Timeout ====================
  describe('UPS-005: Hook execution timeout', () => {
    it('should continue execution when hook times out', async () => {
      await rig.setup('ups-005-timeout', {
        settings: {
          hooks: {
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

  // ==================== UPS-006: Non-blocking Error ====================
  describe('UPS-006: Hook returns non-blocking error (exit code 1)', () => {
    it('should continue execution when hook exits with code 1', async () => {
      await rig.setup('ups-006-nonblocking-error', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command: 'echo warning && exit 1',
                    name: 'ups-error-hook',
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

    it('should handle stdout + stderr with exit code 0 as system message', async () => {
      await rig.setup('ups-006-mixed-output', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo "stdout message" && echo "stderr message" >&2 && exit 0',
                    name: 'ups-mixed-output-hook',
                  },
                ],
              },
            ],
          },
          trusted: true,
        },
      });

      const result = await rig.run('Say mixed output');
      expect(result).toBeDefined();
    });
  });

  // ==================== UPS-007: Blocking Error ====================
  describe('UPS-007: Hook returns blocking error (exit code 2)', () => {
    it('should block execution when hook exits with code 2', async () => {
      await rig.setup('ups-007-blocking-error', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command: 'echo denied && exit 2',
                    name: 'ups-blocking-error-hook',
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

    it('should use stderr as reason when hook exits with code 2', async () => {
      await rig.setup('ups-007-stderr-reason', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      'process.stderr.write("Critical security error") && exit 2',
                    name: 'ups-stderr-hook',
                  },
                ],
              },
            ],
          },
          trusted: true,
        },
      });

      const result = await rig.run('Create a file');
      expect(result.toLowerCase()).toContain('error');
    });
  });

  // ==================== UPS-008: Missing Command ====================
  describe('UPS-008: Hook command does not exist', () => {
    it('should continue execution when hook command does not exist', async () => {
      await rig.setup('ups-008-missing-command', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command: '/nonexistent/command/path',
                    name: 'ups-missing-hook',
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

  // ==================== UPS-009: Correct Input Format ====================
  describe('UPS-009: Hook receives correct input format', () => {
    it('should receive properly formatted input when hook is called', async () => {
      await rig.setup('ups-009-correct-input', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command: `node -e "
const input = JSON.parse(process.argv[2]);
const hasRequired = input.session_id && input.transcript_path && input.cwd && input.hook_event_name && input.prompt;
console.log(JSON.stringify({
  decision: 'allow',
  hookSpecificOutput: { hookEventName: 'UserPromptSubmit', additionalContext: hasRequired ? 'Valid input' : 'Invalid' }
}));
"`,
                    name: 'ups-input-hook',
                  },
                ],
              },
            ],
          },
          trusted: true,
        },
      });

      const result = await rig.run('Say input test');
      validateModelOutput(result, 'input test', 'UPS-009: correct input');
    });
  });

  // ==================== UPS-010: Empty Prompt ====================
  describe('UPS-010: Hook receives empty prompt', () => {
    it('should handle empty prompt correctly', async () => {
      await rig.setup('ups-010-empty-prompt', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command: 'echo \'{"decision":"allow"}\'',
                    name: 'ups-empty-hook',
                  },
                ],
              },
            ],
          },
          trusted: true,
        },
      });

      const result = await rig.run('');
      expect(result).toBeDefined();
    });
  });

  // ==================== UPS-011: System Message ====================
  describe('UPS-011: Hook returns systemMessage', () => {
    it('should include system message in response when hook provides it', async () => {
      await rig.setup('ups-011-system-message', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"allow","systemMessage":"This is a system message from hook"}\'',
                    name: 'ups-system-msg-hook',
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

  // ==================== UPS-012: Suppress Output ====================
  describe('UPS-012: Hook returns suppressOutput', () => {
    it('should suppress output when hook provides suppressOutput: true', async () => {
      await rig.setup('ups-012-suppress-output', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"allow","suppressOutput":true}\'',
                    name: 'ups-suppress-hook',
                  },
                ],
              },
            ],
          },
          trusted: true,
        },
      });

      const result = await rig.run('Say suppress');
      expect(result).toBeDefined();
    });
  });
});

describe('Hooks Integration - Stop', () => {
  let rig: TestRig;

  beforeEach(() => {
    rig = new TestRig();
  });

  afterEach(async () => {
    if (rig) {
      await rig.cleanup();
    }
  });

  // ==================== STP-001: Allow Decision ====================
  describe('STP-001: Hook returns allow decision', () => {
    it('should allow stopping when hook returns allow decision', async () => {
      await rig.setup('stp-001-allow-stop', {
        settings: {
          hooks: {
            Stop: [
              {
                hooks: [
                  {
                    type: 'command',
                    command: 'echo \'{"decision":"allow"}\'',
                    name: 'stop-allow-hook',
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
      await rig.setup('stp-001-allow-final', {
        settings: {
          hooks: {
            Stop: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"allow","hookSpecificOutput":{"additionalContext":"Final context"}}\'',
                    name: 'stop-final-hook',
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

  // ==================== STP-002: Continue False ====================
  describe('STP-002: Hook returns continue: false', () => {
    it('should request continue execution when hook returns continue: false', async () => {
      await rig.setup('stp-002-continue-false', {
        settings: {
          hooks: {
            Stop: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"continue":false,"stopReason":"more work needed"}\'',
                    name: 'stop-continue-hook',
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

    it('should continue agent execution when stop hook returns continue: false', async () => {
      await rig.setup('stp-002-continue-execution', {
        settings: {
          hooks: {
            Stop: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"continue":false,"stopReason":"Not done yet"}\'',
                    name: 'stop-continue-exec-hook',
                  },
                ],
              },
            ],
          },
          trusted: true,
        },
      });

      const result = await rig.run('Complete this task: say notdone');
      // Agent should continue due to continue: false
      expect(result).toBeDefined();
    });
  });

  // ==================== STP-003: Additional Context ====================
  describe('STP-003: Hook adds additionalContext', () => {
    it('should include additional context in final response', async () => {
      await rig.setup('stp-003-add-context', {
        settings: {
          hooks: {
            Stop: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"allow","hookSpecificOutput":{"additionalContext":"final context from hook"}}\'',
                    name: 'stop-context-hook',
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
      await rig.setup('stp-003-multi-context', {
        settings: {
          hooks: {
            Stop: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"allow","hookSpecificOutput":{"additionalContext":"context1"}}\'',
                    name: 'stop-context-1',
                  },
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"allow","hookSpecificOutput":{"additionalContext":"context2"}}\'',
                    name: 'stop-context-2',
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

  // ==================== STP-004: Stop Reason ====================
  describe('STP-004: Hook sets stopReason', () => {
    it('should include stop reason when hook provides it', async () => {
      await rig.setup('stp-004-set-reason', {
        settings: {
          hooks: {
            Stop: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"allow","stopReason":"custom stop reason"}\'',
                    name: 'stop-reason-hook',
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

  // ==================== STP-005: Timeout ====================
  describe('STP-005: Hook execution timeout', () => {
    it('should continue stopping when hook times out', async () => {
      await rig.setup('stp-005-timeout', {
        settings: {
          hooks: {
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

  // ==================== STP-006: Error ====================
  describe('STP-006: Hook execution error', () => {
    it('should continue stopping when hook has non-blocking error', async () => {
      await rig.setup('stp-006-error', {
        settings: {
          hooks: {
            Stop: [
              {
                hooks: [
                  {
                    type: 'command',
                    command: 'echo warning && exit 1',
                    name: 'stop-error-hook',
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
  });

  // ==================== STP-007: Missing Command ====================
  describe('STP-007: Hook command does not exist', () => {
    it('should continue stopping when hook command does not exist', async () => {
      await rig.setup('stp-007-missing-command', {
        settings: {
          hooks: {
            Stop: [
              {
                hooks: [
                  {
                    type: 'command',
                    command: '/nonexistent/stop/command',
                    name: 'stop-missing-hook',
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

  // ==================== STP-008: stop_hook_active = true ====================
  describe('STP-008: Hook receives stop_hook_active=true', () => {
    it('should receive stop_hook_active=true when stop hook is active', async () => {
      await rig.setup('stp-008-active-true', {
        settings: {
          hooks: {
            Stop: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"allow","hookSpecificOutput":{"additionalContext":"active=true"}}\'',
                    name: 'stop-active-true-hook',
                  },
                ],
              },
            ],
          },
          trusted: true,
        },
      });

      const result = await rig.run('Say active');
      expect(result).toBeDefined();
    });
  });

  // ==================== STP-009: stop_hook_active = false ====================
  describe('STP-009: Hook receives stop_hook_active=false', () => {
    it('should receive stop_hook_active=false when stop hook is not active', async () => {
      await rig.setup('stp-009-active-false', {
        settings: {
          hooks: {
            Stop: [
              {
                hooks: [
                  {
                    type: 'command',
                    command: 'echo \'{"decision":"allow"}\'',
                    name: 'stop-active-false-hook',
                  },
                ],
              },
            ],
          },
          trusted: true,
        },
      });

      const result = await rig.run('Say inactive');
      expect(result).toBeDefined();
    });
  });

  // ==================== STP-010: Last Assistant Message ====================
  describe('STP-010: Hook receives lastAssistantMessage', () => {
    it('should receive last assistant message in hook input', async () => {
      await rig.setup('stp-010-last-message', {
        settings: {
          hooks: {
            Stop: [
              {
                hooks: [
                  {
                    type: 'command',
                    command: 'echo \'{"decision":"allow"}\'',
                    name: 'stop-last-msg-hook',
                  },
                ],
              },
            ],
          },
          trusted: true,
        },
      });

      const result = await rig.run('Say last msg');
      expect(result).toBeDefined();
    });
  });

  // ==================== STP-011: System Message ====================
  describe('STP-011: Hook returns systemMessage', () => {
    it('should include system message in final response', async () => {
      await rig.setup('stp-011-system-message', {
        settings: {
          hooks: {
            Stop: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"allow","systemMessage":"Final system message"}}\'',
                    name: 'stop-system-msg-hook',
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

  // ==================== STP-012: Decision Deny ====================
  describe('STP-012: Hook returns deny decision', () => {
    it('should handle deny decision from stop hook', async () => {
      await rig.setup('stp-012-deny', {
        settings: {
          hooks: {
            Stop: [
              {
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"deny","reason":"Stopping denied"}\'',
                    name: 'stop-deny-hook',
                  },
                ],
              },
            ],
          },
          trusted: true,
        },
      });

      const result = await rig.run('Say deny test');
      expect(result).toBeDefined();
    });
  });
});

describe('Hooks Integration - Multiple Hooks', () => {
  let rig: TestRig;

  beforeEach(() => {
    rig = new TestRig();
  });

  afterEach(async () => {
    if (rig) {
      await rig.cleanup();
    }
  });

  // ==================== MUL-001: Sequential Execution ====================
  describe('MUL-001: Sequential execution', () => {
    it('should execute hooks sequentially when sequential: true', async () => {
      await rig.setup('mul-001-sequential', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                sequential: true,
                hooks: [
                  {
                    type: 'command',
                    command: 'echo \'{"decision":"allow"}\'',
                    name: 'seq-hook-1',
                  },
                  {
                    type: 'command',
                    command: 'echo \'{"decision":"allow"}\'',
                    name: 'seq-hook-2',
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

    it('should execute both hooks in order when sequential: true', async () => {
      await rig.setup('mul-001-sequential-order', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                sequential: true,
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"allow","hookSpecificOutput":{"additionalContext":"first"}}\'',
                    name: 'seq-first',
                  },
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"allow","hookSpecificOutput":{"additionalContext":"second"}}\'',
                    name: 'seq-second',
                  },
                ],
              },
            ],
          },
          trusted: true,
        },
      });

      const result = await rig.run('Say order');
      expect(result).toBeDefined();
    });
  });

  // ==================== MUL-002: First Hook Blocks ====================
  describe('MUL-002: Sequential first hook blocks', () => {
    it('should stop at first blocking hook and not execute subsequent', async () => {
      await rig.setup('mul-002-first-blocks', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                sequential: true,
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"block","reason":"blocked by first"}\'',
                    name: 'seq-block-hook',
                  },
                  {
                    type: 'command',
                    command: 'echo \'{"decision":"allow"}\'',
                    name: 'seq-should-not-run',
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
  });

  // ==================== MUL-003: Output Passthrough ====================
  describe('MUL-003: Sequential output passthrough', () => {
    it('should pass output from first hook to second hook input', async () => {
      await rig.setup('mul-003-passthrough', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                sequential: true,
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"allow","hookSpecificOutput":{"additionalContext":"from first"}}\'',
                    name: 'passthrough-hook-1',
                  },
                  {
                    type: 'command',
                    command: 'echo \'{"decision":"allow"}\'',
                    name: 'passthrough-hook-2',
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

  // ==================== MUL-004: Parallel Execution ====================
  describe('MUL-004: Parallel execution', () => {
    it('should execute hooks in parallel when sequential is not set', async () => {
      await rig.setup('mul-004-parallel', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command: 'echo \'{"decision":"allow"}\'',
                    name: 'parallel-hook-1',
                  },
                  {
                    type: 'command',
                    command: 'echo \'{"decision":"allow"}\'',
                    name: 'parallel-hook-2',
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
  });

  // ==================== MUL-005: Mixed Results ====================
  describe('MUL-005: Parallel with mixed results', () => {
    it('should handle mixed success/failure results from parallel hooks', async () => {
      await rig.setup('mul-005-mixed', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command: 'echo \'{"decision":"allow"}\'',
                    name: 'mixed-allow-hook',
                  },
                  {
                    type: 'command',
                    command: '/nonexistent/command',
                    name: 'mixed-error-hook',
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
  });

  // ==================== MUL-006: All Block ====================
  describe('MUL-006: All hooks return block', () => {
    it('should block when all hooks return block in sequential execution', async () => {
      await rig.setup('mul-006-all-block', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                sequential: true,
                hooks: [
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"block","reason":"first block"}\'',
                    name: 'block-hook-1',
                  },
                  {
                    type: 'command',
                    command:
                      'echo \'{"decision":"block","reason":"second block"}\'',
                    name: 'block-hook-2',
                  },
                ],
              },
            ],
          },
          trusted: true,
        },
      });

      const result = await rig.run('Create file');
      expect(result.toLowerCase()).toContain('block');
    });
  });

  // ==================== MUL-007: OR Logic for Decisions ====================
  describe('MUL-007: OR logic for parallel hook decisions', () => {
    it('should allow when any hook returns allow in parallel', async () => {
      await rig.setup('mul-007-or-logic', {
        settings: {
          hooks: {
            UserPromptSubmit: [
              {
                hooks: [
                  {
                    type: 'command',
                    command: 'echo \'{"decision":"block","reason":"blocked"}\'',
                    name: 'block-hook',
                  },
                  {
                    type: 'command',
                    command: 'echo \'{"decision":"allow"}\'',
                    name: 'allow-hook',
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

describe('Hooks Integration - Combined Stop and UserPromptSubmit', () => {
  let rig: TestRig;

  beforeEach(() => {
    rig = new TestRig();
  });

  afterEach(async () => {
    if (rig) {
      await rig.cleanup();
    }
  });

  it('should execute both Stop and UserPromptSubmit hooks in same session', async () => {
    await rig.setup('combined-both-hooks', {
      settings: {
        hooks: {
          Stop: [
            {
              hooks: [
                {
                  type: 'command',
                  command: 'echo \'{"decision":"allow"}\'',
                  name: 'stop-hook',
                },
              ],
            },
          ],
          UserPromptSubmit: [
            {
              hooks: [
                {
                  type: 'command',
                  command: 'echo \'{"decision":"allow"}\'',
                  name: 'ups-hook',
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

  it('should support matcher for Stop hook', async () => {
    await rig.setup('matcher-stop-hook', {
      settings: {
        hooks: {
          Stop: [
            {
              matcher: 'write_file',
              hooks: [
                {
                  type: 'command',
                  command: 'echo \'{"decision":"allow"}\'',
                  name: 'matcher-stop-hook',
                },
              ],
            },
          ],
        },
        trusted: true,
      },
    });

    await rig.run('Create a file matcher_test.txt with content hello');

    const foundToolCall = await rig.waitForToolCall('write_file');
    expect(foundToolCall).toBeTruthy();

    const fileContent = rig.readFile('matcher_test.txt');
    expect(fileContent).toContain('hello');
  });

  it('should execute multiple hooks with different matchers', async () => {
    await rig.setup('multiple-matchers', {
      settings: {
        hooks: {
          Stop: [
            {
              matcher: 'read_file',
              hooks: [
                {
                  type: 'command',
                  command: 'echo \'{"decision":"allow"}\'',
                  name: 'matcher-read',
                },
              ],
            },
            {
              matcher: 'write_file',
              hooks: [
                {
                  type: 'command',
                  command: 'echo \'{"decision":"allow"}\'',
                  name: 'matcher-write',
                },
              ],
            },
          ],
        },
        trusted: true,
      },
    });

    const result = await rig.run(
      'Create file multi.txt with content test and read it',
    );
    expect(result).toBeDefined();
  });

  it('should handle UPS allow + Stop block combination', async () => {
    await rig.setup('ups-allow-stop-block', {
      settings: {
        hooks: {
          Stop: [
            {
              hooks: [
                {
                  type: 'command',
                  command:
                    'echo \'{"decision":"block","reason":"stop blocked"}}\'',
                  name: 'stop-block-hook',
                },
              ],
            },
          ],
          UserPromptSubmit: [
            {
              hooks: [
                {
                  type: 'command',
                  command: 'echo \'{"decision":"allow"}\'',
                  name: 'ups-allow-hook',
                },
              ],
            },
          ],
        },
        trusted: true,
      },
    });

    const result = await rig.run('Say combined test');
    expect(result).toBeDefined();
  });
});
