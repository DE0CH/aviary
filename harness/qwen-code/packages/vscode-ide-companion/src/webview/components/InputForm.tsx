/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import {
  EditPencilIcon,
  AutoEditIcon,
  PlanModeIcon,
  CodeBracketsIcon,
  ThinkingIcon,
  SlashCommandIcon,
  LinkIcon,
  ArrowUpIcon,
} from './icons/index.js';
import { ClaudeCompletionMenu } from './ui/ClaudeCompletionMenu.js';
import type { CompletionItem } from './CompletionTypes.js';

type EditMode = 'ask' | 'auto' | 'plan';

interface InputFormProps {
  inputText: string;
  // Note: RefObject<T> carries nullability in its `current` property, so the
  // generic should be `HTMLDivElement` (not `HTMLDivElement | null`).
  inputFieldRef: React.RefObject<HTMLDivElement>;
  isStreaming: boolean;
  isComposing: boolean;
  editMode: EditMode;
  thinkingEnabled: boolean;
  activeFileName: string | null;
  activeSelection: { startLine: number; endLine: number } | null;
  onInputChange: (text: string) => void;
  onCompositionStart: () => void;
  onCompositionEnd: () => void;
  onKeyDown: (e: React.KeyboardEvent) => void;
  onSubmit: (e: React.FormEvent) => void;
  onToggleEditMode: () => void;
  onToggleThinking: () => void;
  onFocusActiveEditor: () => void;
  onShowCommandMenu: () => void;
  onAttachContext: () => void;
  completionIsOpen: boolean;
  completionItems?: CompletionItem[];
  onCompletionSelect?: (item: CompletionItem) => void;
  onCompletionClose?: () => void;
}

// Get edit mode display info
const getEditModeInfo = (editMode: EditMode) => {
  switch (editMode) {
    case 'ask':
      return {
        text: 'Ask before edits',
        title: 'Qwen will ask before each edit. Click to switch modes.',
        icon: <EditPencilIcon />,
      };
    case 'auto':
      return {
        text: 'Edit automatically',
        title: 'Qwen will edit files automatically. Click to switch modes.',
        icon: <AutoEditIcon />,
      };
    case 'plan':
      return {
        text: 'Plan mode',
        title: 'Qwen will plan before executing. Click to switch modes.',
        icon: <PlanModeIcon />,
      };
    default:
      return {
        text: 'Unknown mode',
        title: 'Unknown edit mode',
        icon: null,
      };
  }
};

export const InputForm: React.FC<InputFormProps> = ({
  inputText,
  inputFieldRef,
  isStreaming,
  isComposing,
  editMode,
  thinkingEnabled,
  activeFileName,
  activeSelection,
  onInputChange,
  onCompositionStart,
  onCompositionEnd,
  onKeyDown,
  onSubmit,
  onToggleEditMode,
  onToggleThinking,
  onFocusActiveEditor,
  onShowCommandMenu,
  onAttachContext,
  completionIsOpen,
  // Claude-style completion dropdown (optional)
  completionItems,
  onCompletionSelect,
  onCompletionClose,
}) => {
  const editModeInfo = getEditModeInfo(editMode);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    // If composing (Chinese IME input), don't process Enter key
    if (e.key === 'Enter' && !e.shiftKey && !isComposing) {
      // If CompletionMenu is open, let it handle Enter key
      if (completionIsOpen) {
        return;
      }
      e.preventDefault();
      onSubmit(e);
    }
    onKeyDown(e);
  };

  return (
    <div
      className="p-1 px-4 pb-4"
      style={{ backgroundColor: 'var(--app-primary-background)' }}
    >
      <div className="block">
        <form
          className="relative flex flex-col rounded-large border shadow-sm transition-all duration-200 focus-within:shadow-md"
          style={{
            backgroundColor:
              'var(--app-input-secondary-background, var(--app-input-background))',
            borderColor: 'var(--app-input-border)',
            color: 'var(--app-input-foreground)',
          }}
          onSubmit={onSubmit}
        >
          {/* Inner background layer */}
          <div
            className="absolute inset-0 rounded-large z-0"
            style={{ backgroundColor: 'var(--app-input-background)' }}
          />

          {/* Banner area */}
          <div className="input-banner" />

          {/* Input wrapper (Claude-style anchor container) */}
          <div className="relative flex z-[1]">
            {/* Claude-style anchored dropdown */}
            {completionIsOpen &&
              completionItems &&
              completionItems.length > 0 &&
              onCompletionSelect &&
              onCompletionClose && (
                // Render dropdown above the input, matching Claude Code
                <ClaudeCompletionMenu
                  items={completionItems}
                  onSelect={onCompletionSelect}
                  onClose={onCompletionClose}
                  title={undefined}
                />
              )}

            <div
              ref={inputFieldRef}
              contentEditable="plaintext-only"
              className="c flex-1 self-stretch p-2.5 px-3.5 outline-none font-inherit leading-relaxed overflow-y-auto relative select-text min-h-[1.5em] max-h-[200px] bg-transparent border-none rounded-none overflow-x-hidden break-words whitespace-pre-wrap empty:before:content-[attr(data-placeholder)] empty:before:absolute empty:before:pointer-events-none disabled:text-gray-400 disabled:cursor-not-allowed"
              style={{
                color: 'var(--app-input-foreground)',
                fontSize: 'var(--vscode-chat-font-size, 13px)',
              }}
              role="textbox"
              aria-label="Message input"
              aria-multiline="true"
              data-placeholder="Ask Qwen Code …"
              onInput={(e) => {
                const target = e.target as HTMLDivElement;
                onInputChange(target.textContent || '');
              }}
              onCompositionStart={onCompositionStart}
              onCompositionEnd={onCompositionEnd}
              onKeyDown={handleKeyDown}
              suppressContentEditableWarning
            />
          </div>

          {/* Actions row */}
          <div
            className="flex items-center p-1.5 gap-1.5 min-w-0 z-[1]"
            style={{
              color: 'var(--app-secondary-foreground)',
              borderTop: '0.5px solid var(--app-input-border)',
            }}
          >
            {/* Edit mode button */}
            <button
              type="button"
              className="flex items-center gap-1.5 px-2.5 py-1.5 h-8 bg-transparent border border-transparent rounded-small cursor-pointer text-xs whitespace-nowrap transition-colors duration-150 hover:bg-[var(--app-ghost-button-hover-background)] [&>svg]:w-4 [&>svg]:h-4 [&>svg]:flex-shrink-0"
              style={{ color: 'var(--app-primary-foreground)' }}
              title={editModeInfo.title}
              onClick={onToggleEditMode}
            >
              {editModeInfo.icon}
              <span>{editModeInfo.text}</span>
            </button>

            {/* Active file indicator */}
            {activeFileName && (
              <button
                type="button"
                className="flex items-center gap-1.5 px-2.5 py-1.5 h-8 bg-transparent border border-transparent rounded-small cursor-pointer text-xs whitespace-nowrap transition-colors duration-150 hover:bg-[var(--app-ghost-button-hover-background)] [&>svg]:w-4 [&>svg]:h-4 [&>svg]:flex-shrink-0 max-w-[200px] overflow-hidden text-ellipsis flex-shrink min-w-0"
                style={{ color: 'var(--app-primary-foreground)' }}
                title={`Showing Qwen Code your current file selection: ${activeFileName}${activeSelection ? `#${activeSelection.startLine}-${activeSelection.endLine}` : ''}`}
                onClick={onFocusActiveEditor}
              >
                <CodeBracketsIcon />
                <span>
                  {activeFileName}
                  {activeSelection &&
                    ` #${activeSelection.startLine}${activeSelection.startLine !== activeSelection.endLine ? `-${activeSelection.endLine}` : ''}`}
                </span>
              </button>
            )}

            {/* Divider */}
            <div
              className="w-px h-6 mx-0.5 flex-shrink-0"
              style={{
                backgroundColor: 'var(--app-transparent-inner-border)',
              }}
            />

            {/* Spacer */}
            <div className="flex-1 min-w-0" />

            {/* Thinking button */}
            <button
              type="button"
              className={`flex items-center justify-center w-8 h-8 p-0 bg-transparent border border-transparent rounded-small cursor-pointer transition-all duration-150 flex-shrink-0 hover:bg-[var(--app-ghost-button-hover-background)] [&>svg]:w-4 [&>svg]:h-4 ${
                thinkingEnabled
                  ? 'bg-qwen-clay-orange text-qwen-ivory [&>svg]:stroke-qwen-ivory [&>svg]:fill-qwen-ivory'
                  : ''
              }`}
              style={{
                color: thinkingEnabled
                  ? 'var(--app-qwen-ivory)'
                  : 'var(--app-secondary-foreground)',
                backgroundColor: thinkingEnabled
                  ? 'var(--app-qwen-clay-button-orange)'
                  : undefined,
              }}
              title={thinkingEnabled ? 'Thinking on' : 'Thinking off'}
              onClick={onToggleThinking}
            >
              <ThinkingIcon enabled={thinkingEnabled} />
            </button>

            {/* Command button */}
            <button
              type="button"
              className="flex items-center justify-center w-8 h-8 p-0 bg-transparent border border-transparent rounded-small cursor-pointer transition-all duration-150 flex-shrink-0 hover:bg-[var(--app-ghost-button-hover-background)] hover:text-[var(--app-primary-foreground)] [&>svg]:w-4 [&>svg]:h-4"
              style={{ color: 'var(--app-secondary-foreground)' }}
              title="Show command menu (/)"
              onClick={onShowCommandMenu}
            >
              <SlashCommandIcon />
            </button>

            {/* Attach button */}
            <button
              type="button"
              className="flex items-center justify-center w-8 h-8 p-0 bg-transparent border border-transparent rounded-small cursor-pointer transition-all duration-150 flex-shrink-0 hover:bg-[var(--app-ghost-button-hover-background)] hover:text-[var(--app-primary-foreground)] [&>svg]:w-4 [&>svg]:h-4"
              style={{ color: 'var(--app-secondary-foreground)' }}
              title="Attach context (Cmd/Ctrl + /)"
              onClick={onAttachContext}
            >
              <LinkIcon />
            </button>

            {/* Send button */}
            <button
              type="submit"
              className="flex items-center justify-center w-8 h-8 p-0 border border-transparent rounded-small cursor-pointer transition-all duration-150 ml-auto flex-shrink-0 hover:brightness-110 disabled:opacity-40 disabled:cursor-not-allowed [&>svg]:w-5 [&>svg]:h-5"
              style={{
                backgroundColor: 'var(--app-qwen-clay-button-orange)',
                color: 'var(--app-qwen-ivory)',
              }}
              disabled={isStreaming || !inputText.trim()}
            >
              <ArrowUpIcon />
            </button>
          </div>
        </form>
      </div>
    </div>
  );
};
