/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Execute tool call component - specialized for command execution operations
 */

import type React from 'react';
import type { BaseToolCallProps } from './shared/types.js';
import { ToolCallContainer } from './shared/LayoutComponents.js';
import { safeTitle, groupContent } from './shared/utils.js';

/**
 * Specialized component for Execute/Bash tool calls
 * Shows: Bash bullet + description + IN/OUT card
 */
export const ExecuteToolCall: React.FC<BaseToolCallProps> = ({ toolCall }) => {
  const { title, content, rawInput, toolCallId } = toolCall;
  const commandText = safeTitle(title);

  // Group content by type
  const { textOutputs, errors } = groupContent(content);

  // Extract command from rawInput if available
  let inputCommand = commandText;
  if (rawInput && typeof rawInput === 'object') {
    const inputObj = rawInput as { command?: string };
    inputCommand = inputObj.command || commandText;
  } else if (typeof rawInput === 'string') {
    inputCommand = rawInput;
  }

  // Error case
  if (errors.length > 0) {
    return (
      <ToolCallContainer label="Bash" status="error" toolCallId={toolCallId}>
        <div className="inline-flex text-[var(--app-secondary-foreground)] text-[0.85em] opacity-70 mt-[2px] mb-[2px] flex-row items-start w-full gap-1">
          <span className="flex-shrink-0 relative top-[-0.1em]">⎿</span>
          <span className="flex-shrink-0 w-full">{commandText}</span>
        </div>
        <div className="bg-[var(--app-input-background)] border border-[var(--app-input-border)] rounded-md p-3 flex flex-col gap-3">
          <div className="grid grid-cols-[80px_1fr] gap-3">
            <div className="text-xs text-[var(--app-secondary-foreground)] font-medium pt-[2px]">
              IN
            </div>
            <div className="text-[var(--app-primary-foreground)] font-mono text-[13px] break-words">
              {inputCommand}
            </div>
          </div>
          <div className="grid grid-cols-[80px_1fr] gap-3">
            <div className="text-xs text-[var(--app-secondary-foreground)] font-medium pt-[2px]">
              Error
            </div>
            <div className="text-[#c74e39] font-mono text-[13px] whitespace-pre-wrap break-words">
              {errors.join('\n')}
            </div>
          </div>
        </div>
      </ToolCallContainer>
    );
  }

  // Success with output
  if (textOutputs.length > 0) {
    const output = textOutputs.join('\n');
    const truncatedOutput =
      output.length > 500 ? output.substring(0, 500) + '...' : output;

    return (
      <ToolCallContainer label="Bash" status="success" toolCallId={toolCallId}>
        <div className="inline-flex text-[var(--app-secondary-foreground)] text-[0.85em] opacity-70 mt-[2px] mb-[2px] flex-row items-start w-full gap-1">
          <span className="flex-shrink-0 relative top-[-0.1em]">⎿</span>
          <span className="flex-shrink-0 w-full">{commandText}</span>
        </div>
        <div className="bg-[var(--app-input-background)] border border-[var(--app-input-border)] rounded-md p-3 flex flex-col gap-3">
          <div className="grid grid-cols-[80px_1fr] gap-3">
            <div className="text-xs text-[var(--app-secondary-foreground)] font-medium pt-[2px]">
              IN
            </div>
            <div className="text-[var(--app-primary-foreground)] font-mono text-[13px] break-words">
              {inputCommand}
            </div>
          </div>
          <div className="grid grid-cols-[80px_1fr] gap-3">
            <div className="text-xs text-[var(--app-secondary-foreground)] font-medium pt-[2px]">
              OUT
            </div>
            <div className="text-[var(--app-primary-foreground)] font-mono text-[13px] whitespace-pre-wrap opacity-90 break-words">
              {truncatedOutput}
            </div>
          </div>
        </div>
      </ToolCallContainer>
    );
  }

  // Success without output: show command with branch connector
  return (
    <ToolCallContainer label="Bash" status="success" toolCallId={toolCallId}>
      <div className="inline-flex text-[var(--app-secondary-foreground)] text-[0.85em] opacity-70 mt-[2px] mb-[2px] flex-row items-start w-full gap-1">
        <span className="flex-shrink-0 relative top-[-0.1em]">⎿</span>
        <span className="flex-shrink-0 w-full">{commandText}</span>
      </div>
    </ToolCallContainer>
  );
};
