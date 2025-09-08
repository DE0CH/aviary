/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import {
  BaseDeclarativeTool,
  BaseToolInvocation,
  Kind,
  ToolResult,
  ToolResultDisplay,
  TaskResultDisplay,
} from './tools.js';
import { Config } from '../config/config.js';
import { SubagentManager } from '../subagents/subagent-manager.js';
import { SubagentConfig } from '../subagents/types.js';
import { ContextState } from '../subagents/subagent.js';
import {
  SubAgentEventEmitter,
  SubAgentToolCallEvent,
  SubAgentToolResultEvent,
  SubAgentFinishEvent,
} from '../subagents/subagent-events.js';
import { ChatRecordingService } from '../services/chatRecordingService.js';

export interface TaskParams {
  description: string;
  prompt: string;
  subagent_type: string;
}

export interface TaskResult {
  success: boolean;
  output?: string;
  error?: string;
  subagent_name?: string;
  execution_summary?: string;
}

/**
 * Task tool that enables primary agents to delegate tasks to specialized subagents.
 * The tool dynamically loads available subagents and includes them in its description
 * for the model to choose from.
 */
export class TaskTool extends BaseDeclarativeTool<TaskParams, ToolResult> {
  static readonly Name: string = 'task';

  private subagentManager: SubagentManager;
  private availableSubagents: SubagentConfig[] = [];

  constructor(private readonly config: Config) {
    // Initialize with a basic schema first
    const initialSchema = {
      type: 'object',
      properties: {
        description: {
          type: 'string',
          description: 'A short (3-5 word) description of the task',
        },
        prompt: {
          type: 'string',
          description: 'The task for the agent to perform',
        },
        subagent_type: {
          type: 'string',
          description: 'The type of specialized agent to use for this task',
        },
      },
      required: ['description', 'prompt', 'subagent_type'],
      additionalProperties: false,
      $schema: 'http://json-schema.org/draft-07/schema#',
    };

    super(
      TaskTool.Name,
      'Task',
      'Delegate tasks to specialized subagents. Loading available subagents...', // Initial description
      Kind.Execute,
      initialSchema,
      true, // isOutputMarkdown
      true, // canUpdateOutput - Enable live output updates for real-time progress
    );

    this.subagentManager = config.getSubagentManager();

    // Initialize the tool asynchronously
    this.initializeAsync();
  }

  /**
   * Asynchronously initializes the tool by loading available subagents
   * and updating the description and schema.
   */
  private async initializeAsync(): Promise<void> {
    try {
      this.availableSubagents = await this.subagentManager.listSubagents();
      this.updateDescriptionAndSchema();
    } catch (error) {
      console.warn('Failed to load subagents for Task tool:', error);
      this.availableSubagents = [];
      this.updateDescriptionAndSchema();
    }
  }

  /**
   * Updates the tool's description and schema based on available subagents.
   */
  private updateDescriptionAndSchema(): void {
    // Generate dynamic description
    const baseDescription = `Delegate tasks to specialized subagents. This tool allows you to offload specific tasks to agents optimized for particular domains, reducing context usage and improving task completion.

## When to Use This Tool

Use this tool proactively when:
- The task matches a specialized agent's description
- You want to reduce context usage for file searches or analysis
- The task requires domain-specific expertise
- You need to perform focused work that doesn't require the full conversation context

## Available Subagents

`;

    let subagentDescriptions = '';
    if (this.availableSubagents.length === 0) {
      subagentDescriptions =
        'No subagents are currently configured. You can create subagents using the /agents command.';
    } else {
      subagentDescriptions = this.availableSubagents
        .map((subagent) => `- **${subagent.name}**: ${subagent.description}`)
        .join('\n');
    }

    // Update description using object property assignment since it's readonly
    (this as { description: string }).description =
      baseDescription + subagentDescriptions;

    // Generate dynamic schema with enum of available subagent names
    const subagentNames = this.availableSubagents.map((s) => s.name);

    // Update the parameter schema by modifying the existing object
    const schema = this.parameterSchema as {
      properties?: {
        subagent_type?: {
          enum?: string[];
        };
      };
    };
    if (schema.properties && schema.properties.subagent_type) {
      if (subagentNames.length > 0) {
        schema.properties.subagent_type.enum = subagentNames;
      } else {
        delete schema.properties.subagent_type.enum;
      }
    }
  }

  /**
   * Refreshes the available subagents and updates the tool description.
   * This can be called when subagents are added or removed.
   */
  async refreshSubagents(): Promise<void> {
    await this.initializeAsync();
  }

  override validateToolParams(params: TaskParams): string | null {
    // Validate required fields
    if (
      !params.description ||
      typeof params.description !== 'string' ||
      params.description.trim() === ''
    ) {
      return 'Parameter "description" must be a non-empty string.';
    }

    if (
      !params.prompt ||
      typeof params.prompt !== 'string' ||
      params.prompt.trim() === ''
    ) {
      return 'Parameter "prompt" must be a non-empty string.';
    }

    if (
      !params.subagent_type ||
      typeof params.subagent_type !== 'string' ||
      params.subagent_type.trim() === ''
    ) {
      return 'Parameter "subagent_type" must be a non-empty string.';
    }

    // Validate that the subagent exists
    const subagentExists = this.availableSubagents.some(
      (subagent) => subagent.name === params.subagent_type,
    );

    if (!subagentExists) {
      const availableNames = this.availableSubagents.map((s) => s.name);
      return `Subagent "${params.subagent_type}" not found. Available subagents: ${availableNames.join(', ')}`;
    }

    return null;
  }

  protected createInvocation(params: TaskParams) {
    return new TaskToolInvocation(this.config, this.subagentManager, params);
  }
}

class TaskToolInvocation extends BaseToolInvocation<TaskParams, ToolResult> {
  private readonly _eventEmitter: SubAgentEventEmitter;
  private currentDisplay: TaskResultDisplay | null = null;
  private currentToolCalls: Array<{
    name: string;
    status: 'executing' | 'success' | 'failed';
    error?: string;
    args?: Record<string, unknown>;
    result?: string;
    returnDisplay?: string;
  }> = [];

  constructor(
    private readonly config: Config,
    private readonly subagentManager: SubagentManager,
    params: TaskParams,
  ) {
    super(params);
    this._eventEmitter = new SubAgentEventEmitter();
  }

  get eventEmitter(): SubAgentEventEmitter {
    return this._eventEmitter;
  }

  /**
   * Updates the current display state and calls updateOutput if provided
   */
  private updateDisplay(
    updates: Partial<TaskResultDisplay>,
    updateOutput?: (output: ToolResultDisplay) => void,
  ): void {
    if (!this.currentDisplay) return;

    this.currentDisplay = {
      ...this.currentDisplay,
      ...updates,
    };

    if (updateOutput) {
      updateOutput(this.currentDisplay);
    }
  }

  /**
   * Sets up event listeners for real-time subagent progress updates
   */
  private setupEventListeners(
    updateOutput?: (output: ToolResultDisplay) => void,
  ): void {
    this.eventEmitter.on('start', () => {
      this.updateDisplay({ status: 'running' }, updateOutput);
    });

    this.eventEmitter.on('model_text', (..._args: unknown[]) => {
      // Model text events are no longer displayed as currentStep
      // Keep the listener for potential future use
    });

    this.eventEmitter.on('tool_call', (...args: unknown[]) => {
      const event = args[0] as SubAgentToolCallEvent;
      const newToolCall = {
        name: event.name,
        status: 'executing' as const,
        args: event.args,
      };
      this.currentToolCalls.push(newToolCall);

      this.updateDisplay(
        {
          progress: {
            toolCalls: [...this.currentToolCalls],
          },
        },
        updateOutput,
      );
    });

    this.eventEmitter.on('tool_result', (...args: unknown[]) => {
      const event = args[0] as SubAgentToolResultEvent;
      const toolCallIndex = this.currentToolCalls.findIndex(
        (call) => call.name === event.name,
      );
      if (toolCallIndex >= 0) {
        this.currentToolCalls[toolCallIndex] = {
          ...this.currentToolCalls[toolCallIndex],
          status: event.success ? 'success' : 'failed',
          error: event.error,
          // Note: result would need to be added to SubAgentToolResultEvent to be captured
        };

        this.updateDisplay(
          {
            progress: {
              toolCalls: [...this.currentToolCalls],
            },
          },
          updateOutput,
        );
      }
    });

    this.eventEmitter.on('finish', (...args: unknown[]) => {
      const event = args[0] as SubAgentFinishEvent;
      this.updateDisplay(
        {
          status: event.terminate_reason === 'GOAL' ? 'completed' : 'failed',
          terminateReason: event.terminate_reason,
          // Keep progress data including tool calls for final display
        },
        updateOutput,
      );
    });

    this.eventEmitter.on('error', () => {
      this.updateDisplay({ status: 'failed' }, updateOutput);
    });
  }

  getDescription(): string {
    return `${this.params.subagent_type} subagent: "${this.params.description}"`;
  }

  override async shouldConfirmExecute(): Promise<false> {
    // Task delegation should execute automatically without user confirmation
    return false;
  }

  async execute(
    signal?: AbortSignal,
    updateOutput?: (output: ToolResultDisplay) => void,
  ): Promise<ToolResult> {
    try {
      // Load the subagent configuration
      const subagentConfig = await this.subagentManager.loadSubagent(
        this.params.subagent_type,
      );

      if (!subagentConfig) {
        const errorDisplay = {
          type: 'subagent_execution' as const,
          subagentName: this.params.subagent_type,
          taskDescription: this.params.description,
          status: 'failed' as const,
          terminateReason: 'ERROR',
          result: `Subagent "${this.params.subagent_type}" not found`,
        };

        return {
          llmContent: [
            {
              text: JSON.stringify({
                success: false,
                error: `Subagent "${this.params.subagent_type}" not found`,
              }),
            },
          ],
          returnDisplay: errorDisplay,
        };
      }

      // Initialize the current display state
      this.currentDisplay = {
        type: 'subagent_execution' as const,
        subagentName: subagentConfig.name,
        taskDescription: this.params.description,
        status: 'running' as const,
      };

      // Set up event listeners for real-time updates
      this.setupEventListeners(updateOutput);

      // Send initial display
      if (updateOutput) {
        updateOutput(this.currentDisplay);
      }
      const chatRecorder = new ChatRecordingService(this.config);
      try {
        chatRecorder.initialize();
      } catch {
        // Initialization failed, continue without recording
      }
      const subagentScope = await this.subagentManager.createSubagentScope(
        subagentConfig,
        this.config,
        { eventEmitter: this.eventEmitter },
      );

      // Set up basic event listeners for chat recording
      this.eventEmitter.on('start', () => {
        chatRecorder.recordMessage({
          type: 'user',
          content: `Subagent(${this.params.subagent_type}) Task: ${this.params.description}\n\n${this.params.prompt}`,
        });
      });

      this.eventEmitter.on('finish', (e) => {
        const finishEvent = e as {
          inputTokens?: number;
          outputTokens?: number;
        };
        const text = subagentScope.getFinalText() || '';
        chatRecorder.recordMessage({ type: 'gemini', content: text });
        const input = finishEvent.inputTokens ?? 0;
        const output = finishEvent.outputTokens ?? 0;
        chatRecorder.recordMessageTokens({
          input,
          output,
          cached: 0,
          total: input + output,
        });
      });

      // Create context state with the task prompt
      const contextState = new ContextState();
      contextState.set('task_prompt', this.params.prompt);

      // Execute the subagent (blocking)
      await subagentScope.runNonInteractive(contextState, signal);

      // Get the results
      const finalText = subagentScope.getFinalText();
      const terminateReason = subagentScope.output.terminate_reason;
      const success = terminateReason === 'GOAL';

      // Format the results based on description (iflow-like switch)
      const wantDetailed = /\b(stats|statistics|detailed)\b/i.test(
        this.params.description,
      );
      const executionSummary = wantDetailed
        ? subagentScope.formatDetailedResult(this.params.description)
        : subagentScope.formatCompactResult(this.params.description);

      const result: TaskResult = {
        success,
        output: finalText,
        subagent_name: subagentConfig.name,
        execution_summary: executionSummary,
      };

      if (!success) {
        result.error = `Task did not complete successfully. Termination reason: ${terminateReason}`;
      }

      // Update the final display state
      this.updateDisplay(
        {
          status: success ? 'completed' : 'failed',
          terminateReason,
          result: finalText,
          executionSummary,
          // Keep progress data including tool calls for final display
        },
        updateOutput,
      );

      return {
        llmContent: [{ text: JSON.stringify(result) }],
        returnDisplay: this.currentDisplay!,
      };
    } catch (error) {
      const errorMessage =
        error instanceof Error ? error.message : String(error);
      console.error(`[TaskTool] Error starting subagent: ${errorMessage}`);

      const errorDisplay = {
        type: 'subagent_execution' as const,
        subagentName: this.params.subagent_type,
        taskDescription: this.params.description,
        status: 'failed' as const,
        terminateReason: 'ERROR',
        result: `Failed to start subagent: ${errorMessage}`,
      };

      return {
        llmContent: [
          {
            text: JSON.stringify({
              success: false,
              error: `Failed to start subagent: ${errorMessage}`,
            }),
          },
        ],
        returnDisplay: errorDisplay,
      };
    }
  }
}
