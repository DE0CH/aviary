/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { TaskTool, TaskParams } from './task.js';
import { Config } from '../config/config.js';
import { SubagentManager } from '../subagents/subagent-manager.js';
import { SubagentConfig } from '../subagents/types.js';
import {
  SubAgentScope,
  ContextState,
  SubagentTerminateMode,
} from '../subagents/subagent.js';
import { partToString } from '../utils/partUtils.js';

// Type for accessing protected methods in tests
type TaskToolWithProtectedMethods = TaskTool & {
  createInvocation: (params: TaskParams) => {
    execute: (
      signal?: AbortSignal,
      liveOutputCallback?: (chunk: string) => void,
    ) => Promise<{
      llmContent: string;
      returnDisplay: unknown;
    }>;
    getDescription: () => string;
    shouldConfirmExecute: () => Promise<boolean>;
  };
};

// Mock dependencies
vi.mock('../subagents/subagent-manager.js');
vi.mock('../subagents/subagent.js');

const MockedSubagentManager = vi.mocked(SubagentManager);
const MockedContextState = vi.mocked(ContextState);

describe('TaskTool', () => {
  let config: Config;
  let taskTool: TaskTool;
  let mockSubagentManager: SubagentManager;

  const mockSubagents: SubagentConfig[] = [
    {
      name: 'file-search',
      description: 'Specialized agent for searching and analyzing files',
      systemPrompt: 'You are a file search specialist.',
      level: 'project',
      filePath: '/project/.qwen/agents/file-search.md',
    },
    {
      name: 'code-review',
      description: 'Agent for reviewing code quality and best practices',
      systemPrompt: 'You are a code review specialist.',
      level: 'user',
      filePath: '/home/user/.qwen/agents/code-review.md',
    },
  ];

  beforeEach(async () => {
    // Setup fake timers
    vi.useFakeTimers();

    // Create mock config
    config = {
      getProjectRoot: vi.fn().mockReturnValue('/test/project'),
      getSessionId: vi.fn().mockReturnValue('test-session-id'),
      getSubagentManager: vi.fn(),
    } as unknown as Config;

    // Setup SubagentManager mock
    mockSubagentManager = {
      listSubagents: vi.fn().mockResolvedValue(mockSubagents),
      loadSubagent: vi.fn(),
      createSubagentScope: vi.fn(),
    } as unknown as SubagentManager;

    MockedSubagentManager.mockImplementation(() => mockSubagentManager);

    // Make config return the mock SubagentManager
    vi.mocked(config.getSubagentManager).mockReturnValue(mockSubagentManager);

    // Create TaskTool instance
    taskTool = new TaskTool(config);

    // Allow async initialization to complete
    await vi.runAllTimersAsync();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  describe('initialization', () => {
    it('should initialize with correct name and properties', () => {
      expect(taskTool.name).toBe('task');
      expect(taskTool.displayName).toBe('Task');
      expect(taskTool.kind).toBe('execute');
    });

    it('should load available subagents during initialization', () => {
      expect(mockSubagentManager.listSubagents).toHaveBeenCalled();
    });

    it('should update description with available subagents', () => {
      expect(taskTool.description).toContain('file-search');
      expect(taskTool.description).toContain(
        'Specialized agent for searching and analyzing files',
      );
      expect(taskTool.description).toContain('code-review');
      expect(taskTool.description).toContain(
        'Agent for reviewing code quality and best practices',
      );
    });

    it('should handle empty subagents list gracefully', async () => {
      vi.mocked(mockSubagentManager.listSubagents).mockResolvedValue([]);

      const emptyTaskTool = new TaskTool(config);
      await vi.runAllTimersAsync();

      expect(emptyTaskTool.description).toContain(
        'No subagents are currently configured',
      );
    });

    it('should handle subagent loading errors gracefully', async () => {
      vi.mocked(mockSubagentManager.listSubagents).mockRejectedValue(
        new Error('Loading failed'),
      );

      const consoleSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});

      new TaskTool(config);
      await vi.runAllTimersAsync();

      expect(consoleSpy).toHaveBeenCalledWith(
        'Failed to load subagents for Task tool:',
        expect.any(Error),
      );
      consoleSpy.mockRestore();
    });
  });

  describe('schema generation', () => {
    it('should generate schema with subagent names as enum', () => {
      const schema = taskTool.schema;
      const properties = schema.parametersJsonSchema as {
        properties: {
          subagent_type: {
            enum?: string[];
          };
        };
      };
      expect(properties.properties.subagent_type.enum).toEqual([
        'file-search',
        'code-review',
      ]);
    });

    it('should generate schema without enum when no subagents available', async () => {
      vi.mocked(mockSubagentManager.listSubagents).mockResolvedValue([]);

      const emptyTaskTool = new TaskTool(config);
      await vi.runAllTimersAsync();

      const schema = emptyTaskTool.schema;
      const properties = schema.parametersJsonSchema as {
        properties: {
          subagent_type: {
            enum?: string[];
          };
        };
      };
      expect(properties.properties.subagent_type.enum).toBeUndefined();
    });
  });

  describe('validateToolParams', () => {
    const validParams: TaskParams = {
      description: 'Search files',
      prompt: 'Find all TypeScript files in the project',
      subagent_type: 'file-search',
    };

    it('should validate valid parameters', async () => {
      const result = taskTool.validateToolParams(validParams);
      expect(result).toBeNull();
    });

    it('should reject empty description', async () => {
      const result = taskTool.validateToolParams({
        ...validParams,
        description: '',
      });
      expect(result).toBe(
        'Parameter "description" must be a non-empty string.',
      );
    });

    it('should reject empty prompt', async () => {
      const result = taskTool.validateToolParams({
        ...validParams,
        prompt: '',
      });
      expect(result).toBe('Parameter "prompt" must be a non-empty string.');
    });

    it('should reject empty subagent_type', async () => {
      const result = taskTool.validateToolParams({
        ...validParams,
        subagent_type: '',
      });
      expect(result).toBe(
        'Parameter "subagent_type" must be a non-empty string.',
      );
    });

    it('should reject non-existent subagent', async () => {
      const result = taskTool.validateToolParams({
        ...validParams,
        subagent_type: 'non-existent',
      });
      expect(result).toBe(
        'Subagent "non-existent" not found. Available subagents: file-search, code-review',
      );
    });
  });

  describe('refreshSubagents', () => {
    it('should refresh available subagents and update description', async () => {
      const newSubagents: SubagentConfig[] = [
        {
          name: 'test-agent',
          description: 'A test agent',
          systemPrompt: 'Test prompt',
          level: 'project',
          filePath: '/project/.qwen/agents/test-agent.md',
        },
      ];

      vi.mocked(mockSubagentManager.listSubagents).mockResolvedValue(
        newSubagents,
      );

      await taskTool.refreshSubagents();

      expect(taskTool.description).toContain('test-agent');
      expect(taskTool.description).toContain('A test agent');
    });
  });

  describe('TaskToolInvocation', () => {
    let mockSubagentScope: SubAgentScope;
    let mockContextState: ContextState;

    beforeEach(() => {
      mockSubagentScope = {
        runNonInteractive: vi.fn().mockResolvedValue(undefined),
        output: {
          result: 'Task completed successfully',
          terminate_reason: SubagentTerminateMode.GOAL,
        },
        getFinalText: vi.fn().mockReturnValue('Task completed successfully'),
        formatCompactResult: vi
          .fn()
          .mockReturnValue(
            '✅ Success: Search files completed with GOAL termination',
          ),
        getStatistics: vi.fn().mockReturnValue({
          rounds: 2,
          totalDurationMs: 1500,
          totalToolCalls: 3,
          successfulToolCalls: 3,
          failedToolCalls: 0,
        }),
      } as unknown as SubAgentScope;

      mockContextState = {
        set: vi.fn(),
      } as unknown as ContextState;

      MockedContextState.mockImplementation(() => mockContextState);

      vi.mocked(mockSubagentManager.loadSubagent).mockResolvedValue(
        mockSubagents[0],
      );
      vi.mocked(mockSubagentManager.createSubagentScope).mockResolvedValue(
        mockSubagentScope,
      );
    });

    it('should execute subagent successfully', async () => {
      const params: TaskParams = {
        description: 'Search files',
        prompt: 'Find all TypeScript files',
        subagent_type: 'file-search',
      };

      const invocation = (
        taskTool as TaskToolWithProtectedMethods
      ).createInvocation(params);
      const result = await invocation.execute();

      expect(mockSubagentManager.loadSubagent).toHaveBeenCalledWith(
        'file-search',
      );
      expect(mockSubagentManager.createSubagentScope).toHaveBeenCalledWith(
        mockSubagents[0],
        config,
        expect.any(Object), // eventEmitter parameter
      );
      expect(mockSubagentScope.runNonInteractive).toHaveBeenCalledWith(
        mockContextState,
        undefined, // signal parameter (undefined when not provided)
      );

      const llmText = partToString(result.llmContent);
      const parsedResult = JSON.parse(llmText) as {
        success: boolean;
        subagent_name?: string;
        error?: string;
      };
      expect(parsedResult.success).toBe(true);
      expect(parsedResult.subagent_name).toBe('file-search');
    });

    it('should handle subagent not found error', async () => {
      vi.mocked(mockSubagentManager.loadSubagent).mockResolvedValue(null);

      const params: TaskParams = {
        description: 'Search files',
        prompt: 'Find all TypeScript files',
        subagent_type: 'non-existent',
      };

      const invocation = (
        taskTool as TaskToolWithProtectedMethods
      ).createInvocation(params);
      const result = await invocation.execute();

      const llmText = partToString(result.llmContent);
      const parsedResult = JSON.parse(llmText) as {
        success: boolean;
        subagent_name?: string;
        error?: string;
      };
      expect(parsedResult.success).toBe(false);
      expect(parsedResult.error).toContain('Subagent "non-existent" not found');
    });

    it('should handle subagent execution failure', async () => {
      mockSubagentScope.output.terminate_reason = SubagentTerminateMode.ERROR;

      const params: TaskParams = {
        description: 'Search files',
        prompt: 'Find all TypeScript files',
        subagent_type: 'file-search',
      };

      const invocation = (
        taskTool as TaskToolWithProtectedMethods
      ).createInvocation(params);
      const result = await invocation.execute();

      const llmText = partToString(result.llmContent);
      const parsedResult = JSON.parse(llmText) as {
        success: boolean;
        subagent_name?: string;
        error?: string;
      };
      expect(parsedResult.success).toBe(false);
      expect(parsedResult.error).toContain(
        'Task did not complete successfully',
      );
    });

    it('should handle execution errors gracefully', async () => {
      vi.mocked(mockSubagentManager.createSubagentScope).mockRejectedValue(
        new Error('Creation failed'),
      );

      const params: TaskParams = {
        description: 'Search files',
        prompt: 'Find all TypeScript files',
        subagent_type: 'file-search',
      };

      const invocation = (
        taskTool as TaskToolWithProtectedMethods
      ).createInvocation(params);
      const result = await invocation.execute();

      const llmText = partToString(result.llmContent);
      const parsedResult = JSON.parse(llmText) as {
        success: boolean;
        subagent_name?: string;
        error?: string;
      };
      expect(parsedResult.success).toBe(false);
      expect(parsedResult.error).toContain('Failed to start subagent');
    });

    it('should execute subagent without live output callback', async () => {
      const params: TaskParams = {
        description: 'Search files',
        prompt: 'Find all TypeScript files',
        subagent_type: 'file-search',
      };

      const invocation = (
        taskTool as TaskToolWithProtectedMethods
      ).createInvocation(params);
      const result = await invocation.execute();

      // Verify that the task completed successfully
      expect(result.llmContent).toBeDefined();
      expect(result.returnDisplay).toBeDefined();

      // Verify the result has the expected structure
      const llmContent = Array.isArray(result.llmContent)
        ? result.llmContent
        : [result.llmContent];
      const parsedResult = JSON.parse((llmContent[0] as { text: string }).text);
      expect(parsedResult.success).toBe(true);
      expect(parsedResult.subagent_name).toBe('file-search');
    });

    it('should set context variables correctly', async () => {
      const params: TaskParams = {
        description: 'Search files',
        prompt: 'Find all TypeScript files',
        subagent_type: 'file-search',
      };

      const invocation = (
        taskTool as TaskToolWithProtectedMethods
      ).createInvocation(params);
      await invocation.execute();

      expect(mockContextState.set).toHaveBeenCalledWith(
        'task_prompt',
        'Find all TypeScript files',
      );
    });

    it('should return structured display object', async () => {
      const params: TaskParams = {
        description: 'Search files',
        prompt: 'Find all TypeScript files',
        subagent_type: 'file-search',
      };

      const invocation = (
        taskTool as TaskToolWithProtectedMethods
      ).createInvocation(params);
      const result = await invocation.execute();

      expect(typeof result.returnDisplay).toBe('object');
      expect(result.returnDisplay).toHaveProperty('type', 'subagent_execution');
      expect(result.returnDisplay).toHaveProperty(
        'subagentName',
        'file-search',
      );
      expect(result.returnDisplay).toHaveProperty(
        'taskDescription',
        'Search files',
      );
      expect(result.returnDisplay).toHaveProperty('status', 'completed');
    });

    it('should not require confirmation', async () => {
      const params: TaskParams = {
        description: 'Search files',
        prompt: 'Find all TypeScript files',
        subagent_type: 'file-search',
      };

      const invocation = (
        taskTool as TaskToolWithProtectedMethods
      ).createInvocation(params);
      const shouldConfirm = await invocation.shouldConfirmExecute();

      expect(shouldConfirm).toBe(false);
    });

    it('should provide correct description', async () => {
      const params: TaskParams = {
        description: 'Search files',
        prompt: 'Find all TypeScript files',
        subagent_type: 'file-search',
      };

      const invocation = (
        taskTool as TaskToolWithProtectedMethods
      ).createInvocation(params);
      const description = invocation.getDescription();

      expect(description).toBe(
        'file-search subagent: "Search files"',
      );
    });
  });
});
