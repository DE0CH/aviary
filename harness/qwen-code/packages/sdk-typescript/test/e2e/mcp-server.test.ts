/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * E2E tests for MCP (Model Context Protocol) server integration via SDK
 * Tests that the SDK can properly interact with MCP servers configured in qwen-code
 */

import { describe, it, expect, beforeAll } from 'vitest';
import { query } from '../../src/index.js';
import {
  isCLIAssistantMessage,
  isCLIResultMessage,
  isCLISystemMessage,
  isCLIUserMessage,
  type TextBlock,
  type ContentBlock,
  type CLIMessage,
  type ToolUseBlock,
  type CLISystemMessage,
} from '../../src/types/protocol.js';
import { writeFileSync, mkdirSync, chmodSync } from 'node:fs';
import { join } from 'node:path';

const TEST_CLI_PATH = process.env['TEST_CLI_PATH']!;
const E2E_TEST_FILE_DIR = process.env['E2E_TEST_FILE_DIR']!;

const SHARED_TEST_OPTIONS = {
  pathToQwenExecutable: TEST_CLI_PATH,
  permissionMode: 'yolo' as const,
};

/**
 * Helper to extract text from ContentBlock array
 */
function extractText(content: ContentBlock[]): string {
  return content
    .filter((block): block is TextBlock => block.type === 'text')
    .map((block) => block.text)
    .join('');
}

/**
 * Minimal MCP server implementation that doesn't require external dependencies
 * This implements the MCP protocol directly using Node.js built-ins
 */
const MCP_SERVER_SCRIPT = `#!/usr/bin/env node
/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

const readline = require('readline');
const fs = require('fs');

// Debug logging to stderr (only when MCP_DEBUG or VERBOSE is set)
const debugEnabled = process.env['MCP_DEBUG'] === 'true' || process.env['VERBOSE'] === 'true';
function debug(msg) {
  if (debugEnabled) {
    fs.writeSync(2, \`[MCP-DEBUG] \${msg}\\n\`);
  }
}

debug('MCP server starting...');

// Simple JSON-RPC implementation for MCP
class SimpleJSONRPC {
  constructor() {
    this.handlers = new Map();
    this.rl = readline.createInterface({
      input: process.stdin,
      output: process.stdout,
      terminal: false
    });
    
    this.rl.on('line', (line) => {
      debug(\`Received line: \${line}\`);
      try {
        const message = JSON.parse(line);
        debug(\`Parsed message: \${JSON.stringify(message)}\`);
        this.handleMessage(message);
      } catch (e) {
        debug(\`Parse error: \${e.message}\`);
      }
    });
  }
  
  send(message) {
    const msgStr = JSON.stringify(message);
    debug(\`Sending message: \${msgStr}\`);
    process.stdout.write(msgStr + '\\n');
  }
  
  async handleMessage(message) {
    if (message.method && this.handlers.has(message.method)) {
      try {
        const result = await this.handlers.get(message.method)(message.params || {});
        if (message.id !== undefined) {
          this.send({
            jsonrpc: '2.0',
            id: message.id,
            result
          });
        }
      } catch (error) {
        if (message.id !== undefined) {
          this.send({
            jsonrpc: '2.0',
            id: message.id,
            error: {
              code: -32603,
              message: error.message
            }
          });
        }
      }
    } else if (message.id !== undefined) {
      this.send({
        jsonrpc: '2.0',
        id: message.id,
        error: {
          code: -32601,
          message: 'Method not found'
        }
      });
    }
  }
  
  on(method, handler) {
    this.handlers.set(method, handler);
  }
}

// Create MCP server
const rpc = new SimpleJSONRPC();

// Handle initialize
rpc.on('initialize', async (params) => {
  debug('Handling initialize request');
  return {
    protocolVersion: '2024-11-05',
    capabilities: {
      tools: {}
    },
    serverInfo: {
      name: 'test-math-server',
      version: '1.0.0'
    }
  };
});

// Handle tools/list
rpc.on('tools/list', async () => {
  debug('Handling tools/list request');
  return {
    tools: [
      {
        name: 'add',
        description: 'Add two numbers together',
        inputSchema: {
          type: 'object',
          properties: {
            a: { type: 'number', description: 'First number' },
            b: { type: 'number', description: 'Second number' }
          },
          required: ['a', 'b']
        }
      },
      {
        name: 'multiply',
        description: 'Multiply two numbers together',
        inputSchema: {
          type: 'object',
          properties: {
            a: { type: 'number', description: 'First number' },
            b: { type: 'number', description: 'Second number' }
          },
          required: ['a', 'b']
        }
      }
    ]
  };
});

// Handle tools/call
rpc.on('tools/call', async (params) => {
  debug(\`Handling tools/call request for tool: \${params.name}\`);
  
  if (params.name === 'add') {
    const { a, b } = params.arguments;
    return {
      content: [{
        type: 'text',
        text: String(a + b)
      }]
    };
  }
  
  if (params.name === 'multiply') {
    const { a, b } = params.arguments;
    return {
      content: [{
        type: 'text',
        text: String(a * b)
      }]
    };
  }
  
  throw new Error('Unknown tool: ' + params.name);
});

// Send initialization notification
rpc.send({
  jsonrpc: '2.0',
  method: 'initialized'
});
`;

describe('MCP Server Integration (E2E)', () => {
  let testDir: string;
  let serverScriptPath: string;

  beforeAll(() => {
    // Use the centralized E2E test directory from globalSetup
    testDir = join(E2E_TEST_FILE_DIR, 'mcp-server-test');
    mkdirSync(testDir, { recursive: true });

    // Write MCP server script
    serverScriptPath = join(testDir, 'mcp-server.cjs');
    writeFileSync(serverScriptPath, MCP_SERVER_SCRIPT);

    // Make script executable on Unix-like systems
    if (process.platform !== 'win32') {
      chmodSync(serverScriptPath, 0o755);
    }
  });

  describe('Basic MCP Tool Usage', () => {
    it('should use MCP add tool to add two numbers', async () => {
      const q = query({
        prompt:
          'Use the add tool to calculate 5 + 10. Just give me the result.',
        options: {
          ...SHARED_TEST_OPTIONS,
          cwd: testDir,
          debug: false,
          mcpServers: {
            'test-math-server': {
              command: 'node',
              args: [serverScriptPath],
            },
          },
        },
      });

      const messages: CLIMessage[] = [];
      let assistantText = '';
      let foundToolUse = false;

      try {
        for await (const message of q) {
          messages.push(message);

          if (isCLIAssistantMessage(message)) {
            const toolUseBlock = message.message.content.find(
              (block: ContentBlock): block is ToolUseBlock =>
                block.type === 'tool_use',
            );
            if (toolUseBlock && toolUseBlock.name === 'add') {
              foundToolUse = true;
            }
            assistantText += extractText(message.message.content);
          }
        }

        // Validate tool was called
        expect(foundToolUse).toBe(true);

        // Validate result contains expected answer
        expect(assistantText).toMatch(/15/);

        // Validate successful completion
        const lastMessage = messages[messages.length - 1];
        expect(isCLIResultMessage(lastMessage)).toBe(true);
        if (isCLIResultMessage(lastMessage)) {
          expect(lastMessage.subtype).toBe('success');
        }
      } finally {
        await q.close();
      }
    });

    it('should use MCP multiply tool to multiply two numbers', async () => {
      const q = query({
        prompt:
          'Use the multiply tool to calculate 6 * 7. Just give me the result.',
        options: {
          ...SHARED_TEST_OPTIONS,
          cwd: testDir,
          debug: false,
          mcpServers: {
            'test-math-server': {
              command: 'node',
              args: [serverScriptPath],
            },
          },
        },
      });

      const messages: CLIMessage[] = [];
      let assistantText = '';
      let foundToolUse = false;

      try {
        for await (const message of q) {
          messages.push(message);

          if (isCLIAssistantMessage(message)) {
            const toolUseBlock = message.message.content.find(
              (block: ContentBlock): block is ToolUseBlock =>
                block.type === 'tool_use',
            );
            if (toolUseBlock && toolUseBlock.name === 'multiply') {
              foundToolUse = true;
            }
            assistantText += extractText(message.message.content);
          }
        }

        // Validate tool was called
        expect(foundToolUse).toBe(true);

        // Validate result contains expected answer
        expect(assistantText).toMatch(/42/);

        // Validate successful completion
        const lastMessage = messages[messages.length - 1];
        expect(isCLIResultMessage(lastMessage)).toBe(true);
      } finally {
        await q.close();
      }
    });
  });

  describe('MCP Server Discovery', () => {
    it('should list MCP servers in system init message', async () => {
      const q = query({
        prompt: 'Hello',
        options: {
          ...SHARED_TEST_OPTIONS,
          cwd: testDir,
          debug: false,
          mcpServers: {
            'test-math-server': {
              command: 'node',
              args: [serverScriptPath],
            },
          },
        },
      });

      let systemMessage: CLISystemMessage | null = null;

      try {
        for await (const message of q) {
          if (isCLISystemMessage(message) && message.subtype === 'init') {
            systemMessage = message;
            break;
          }
        }

        // Validate MCP server is listed
        expect(systemMessage).not.toBeNull();
        expect(systemMessage!.mcp_servers).toBeDefined();
        expect(Array.isArray(systemMessage!.mcp_servers)).toBe(true);

        // Find our test server
        const testServer = systemMessage!.mcp_servers?.find(
          (server) => server.name === 'test-math-server',
        );
        expect(testServer).toBeDefined();

        // Note: tools are not exposed in the mcp_servers array in system message
        // They are available through the MCP protocol but not in the init message
      } finally {
        await q.close();
      }
    });
  });

  describe('Complex MCP Operations', () => {
    it('should chain multiple MCP tool calls', async () => {
      const q = query({
        prompt:
          'First use add to calculate 10 + 5, then multiply the result by 2. Give me the final answer.',
        options: {
          ...SHARED_TEST_OPTIONS,
          cwd: testDir,
          debug: false,
          mcpServers: {
            'test-math-server': {
              command: 'node',
              args: [serverScriptPath],
            },
          },
        },
      });

      const messages: CLIMessage[] = [];
      let assistantText = '';
      const toolCalls: string[] = [];

      try {
        for await (const message of q) {
          messages.push(message);

          if (isCLIAssistantMessage(message)) {
            const toolUseBlocks = message.message.content.filter(
              (block: ContentBlock): block is ToolUseBlock =>
                block.type === 'tool_use',
            );
            toolUseBlocks.forEach((block) => {
              toolCalls.push(block.name);
            });
            assistantText += extractText(message.message.content);
          }
        }

        // Validate both tools were called
        expect(toolCalls).toContain('add');
        expect(toolCalls).toContain('multiply');

        // Validate result: (10 + 5) * 2 = 30
        expect(assistantText).toMatch(/30/);

        // Validate successful completion
        const lastMessage = messages[messages.length - 1];
        expect(isCLIResultMessage(lastMessage)).toBe(true);
      } finally {
        await q.close();
      }
    });

    it('should handle multiple calls to the same MCP tool', async () => {
      const q = query({
        prompt:
          'Use the add tool twice: first add 1 + 2, then add 3 + 4. Tell me both results.',
        options: {
          ...SHARED_TEST_OPTIONS,
          cwd: testDir,
          debug: false,
          mcpServers: {
            'test-math-server': {
              command: 'node',
              args: [serverScriptPath],
            },
          },
        },
      });

      const messages: CLIMessage[] = [];
      let assistantText = '';
      const addToolCalls: ToolUseBlock[] = [];

      try {
        for await (const message of q) {
          messages.push(message);

          if (isCLIAssistantMessage(message)) {
            const toolUseBlocks = message.message.content.filter(
              (block: ContentBlock): block is ToolUseBlock =>
                block.type === 'tool_use',
            );
            toolUseBlocks.forEach((block) => {
              if (block.name === 'add') {
                addToolCalls.push(block);
              }
            });
            assistantText += extractText(message.message.content);
          }
        }

        // Validate add tool was called at least twice
        expect(addToolCalls.length).toBeGreaterThanOrEqual(2);

        // Validate results contain expected answers: 3 and 7
        expect(assistantText).toMatch(/3/);
        expect(assistantText).toMatch(/7/);

        // Validate successful completion
        const lastMessage = messages[messages.length - 1];
        expect(isCLIResultMessage(lastMessage)).toBe(true);
      } finally {
        await q.close();
      }
    });
  });

  describe('MCP Tool Message Flow', () => {
    it('should receive proper message sequence for MCP tool usage', async () => {
      const q = query({
        prompt: 'Use add to calculate 2 + 3',
        options: {
          ...SHARED_TEST_OPTIONS,
          cwd: testDir,
          debug: false,
          mcpServers: {
            'test-math-server': {
              command: 'node',
              args: [serverScriptPath],
            },
          },
        },
      });

      const messageTypes: string[] = [];
      let foundToolUse = false;
      let foundToolResult = false;

      try {
        for await (const message of q) {
          messageTypes.push(message.type);

          if (isCLIAssistantMessage(message)) {
            const toolUseBlock = message.message.content.find(
              (block: ContentBlock): block is ToolUseBlock =>
                block.type === 'tool_use',
            );
            if (toolUseBlock) {
              foundToolUse = true;
              expect(toolUseBlock.name).toBe('add');
              expect(toolUseBlock.input).toBeDefined();
            }
          }

          if (isCLIUserMessage(message)) {
            const content = message.message.content;
            const contentArray = Array.isArray(content)
              ? content
              : [{ type: 'text', text: content }];
            const toolResultBlock = contentArray.find(
              (block) => block.type === 'tool_result',
            );
            if (toolResultBlock) {
              foundToolResult = true;
            }
          }
        }

        // Validate message flow
        expect(foundToolUse).toBe(true);
        expect(foundToolResult).toBe(true);
        expect(messageTypes).toContain('system');
        expect(messageTypes).toContain('assistant');
        expect(messageTypes).toContain('user');
        expect(messageTypes).toContain('result');

        // Result should be last message
        expect(messageTypes[messageTypes.length - 1]).toBe('result');
      } finally {
        await q.close();
      }
    });
  });

  describe('Error Handling', () => {
    it('should handle gracefully when MCP tool is not available', async () => {
      const q = query({
        prompt: 'Use the subtract tool to calculate 10 - 5',
        options: {
          ...SHARED_TEST_OPTIONS,
          cwd: testDir,
          debug: false,
          mcpServers: {
            'test-math-server': {
              command: 'node',
              args: [serverScriptPath],
            },
          },
        },
      });

      const messages: CLIMessage[] = [];
      let assistantText = '';

      try {
        for await (const message of q) {
          messages.push(message);

          if (isCLIAssistantMessage(message)) {
            assistantText += extractText(message.message.content);
          }
        }

        // Should complete without crashing
        const lastMessage = messages[messages.length - 1];
        expect(isCLIResultMessage(lastMessage)).toBe(true);

        // Assistant should indicate tool is not available or provide alternative
        expect(assistantText.length).toBeGreaterThan(0);
      } finally {
        await q.close();
      }
    });
  });
});
