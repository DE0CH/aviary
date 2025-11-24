/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';

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
}

// Get edit mode display info
const getEditModeInfo = (editMode: EditMode) => {
  switch (editMode) {
    case 'ask':
      return {
        text: 'Ask before edits',
        title: 'Qwen will ask before each edit. Click to switch modes.',
        icon: (
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 16 16"
            fill="currentColor"
            aria-hidden="true"
          >
            <path
              fillRule="evenodd"
              d="M11.013 2.513a1.75 1.75 0 0 1 2.475 2.474L6.226 12.25a2.751 2.751 0 0 1-.892.596l-2.047.848a.75.75 0 0 1-.98-.98l.848-2.047a2.75 2.75 0 0 1 .596-.892l7.262-7.261Z"
              clipRule="evenodd"
            ></path>
          </svg>
        ),
      };
    case 'auto':
      return {
        text: 'Edit automatically',
        title: 'Qwen will edit files automatically. Click to switch modes.',
        icon: (
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 16 16"
            fill="currentColor"
            aria-hidden="true"
          >
            <path d="M2.53 3.956A1 1 0 0 0 1 4.804v6.392a1 1 0 0 0 1.53.848l5.113-3.196c.16-.1.279-.233.357-.383v2.73a1 1 0 0 0 1.53.849l5.113-3.196a1 1 0 0 0 0-1.696L9.53 3.956A1 1 0 0 0 8 4.804v2.731a.992.992 0 0 0-.357-.383L2.53 3.956Z"></path>
          </svg>
        ),
      };
    case 'plan':
      return {
        text: 'Plan mode',
        title: 'Qwen will plan before executing. Click to switch modes.',
        icon: (
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 16 16"
            fill="currentColor"
            aria-hidden="true"
          >
            <path d="M4.5 2a.5.5 0 0 0-.5.5v11a.5.5 0 0 0 .5.5h1a.5.5 0 0 0 .5-.5v-11a.5.5 0 0 0-.5-.5h-1ZM10.5 2a.5.5 0 0 0-.5.5v11a.5.5 0 0 0 .5.5h1a.5.5 0 0 0 .5-.5v-11a.5.5 0 0 0-.5-.5h-1Z"></path>
          </svg>
        ),
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

          {/* Input wrapper */}
          <div className="relative flex z-[1]">
            <div
              ref={inputFieldRef}
              contentEditable="plaintext-only"
              className="flex-1 self-stretch p-2.5 px-3.5 outline-none font-inherit leading-relaxed overflow-y-auto relative select-text min-h-[1.5em] max-h-[200px] bg-transparent border-none rounded-none overflow-x-hidden break-words whitespace-pre-wrap empty:before:content-[attr(data-placeholder)] empty:before:absolute empty:before:pointer-events-none disabled:text-gray-400 disabled:cursor-not-allowed"
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
                <svg
                  xmlns="http://www.w3.org/2000/svg"
                  viewBox="0 0 20 20"
                  fill="currentColor"
                  aria-hidden="true"
                  data-slot="icon"
                >
                  <path
                    fillRule="evenodd"
                    d="M6.28 5.22a.75.75 0 0 1 0 1.06L2.56 10l3.72 3.72a.75.75 0 0 1-1.06 1.06L.97 10.53a.75.75 0 0 1 0-1.06l4.25-4.25a.75.75 0 0 1 1.06 0Zm7.44 0a.75.75 0 0 1 1.06 0l4.25 4.25a.75.75 0 0 1 0 1.06l-4.25 4.25a.75.75 0 0 1-1.06-1.06L17.44 10l-3.72-3.72a.75.75 0 0 1 0-1.06ZM11.377 2.011a.75.75 0 0 1 .612.867l-2.5 14.5a.75.75 0 0 1-1.478-.255l2.5-14.5a.75.75 0 0 1 .866-.612Z"
                    clipRule="evenodd"
                  ></path>
                </svg>
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
              <svg
                width="16"
                height="16"
                viewBox="0 0 16 16"
                fill="none"
                xmlns="http://www.w3.org/2000/svg"
              >
                <path
                  d="M8.00293 1.11523L8.35059 1.12402H8.35352C11.9915 1.30834 14.8848 4.31624 14.8848 8C14.8848 11.8025 11.8025 14.8848 8 14.8848C4.19752 14.8848 1.11523 11.8025 1.11523 8C1.11523 7.67691 1.37711 7.41504 1.7002 7.41504C2.02319 7.41514 2.28516 7.67698 2.28516 8C2.28516 11.1563 4.84369 13.7148 8 13.7148C11.1563 13.7148 13.7148 11.1563 13.7148 8C13.7148 4.94263 11.3141 2.4464 8.29492 2.29297V2.29199L7.99609 2.28516H7.9873V2.28418L7.89648 2.27539L7.88281 2.27441V2.27344C7.61596 2.21897 7.41513 1.98293 7.41504 1.7002C7.41504 1.37711 7.67691 1.11523 8 1.11523H8.00293ZM8 3.81543C8.32309 3.81543 8.58496 4.0773 8.58496 4.40039V7.6377L10.9619 8.82715C11.2505 8.97169 11.3678 9.32256 11.2236 9.61133C11.0972 9.86425 10.8117 9.98544 10.5488 9.91504L10.5352 9.91211V9.91016L10.4502 9.87891L10.4385 9.87402V9.87305L7.73828 8.52344C7.54007 8.42433 7.41504 8.22155 7.41504 8V4.40039C7.41504 4.0773 7.67691 3.81543 8 3.81543ZM2.44336 5.12695C2.77573 5.19517 3.02597 5.48929 3.02637 5.8418C3.02637 6.19456 2.7761 6.49022 2.44336 6.55859L2.2959 6.57324C1.89241 6.57324 1.56543 6.24529 1.56543 5.8418C1.56588 5.43853 1.89284 5.1123 2.2959 5.1123L2.44336 5.12695ZM3.46094 2.72949C3.86418 2.72984 4.19017 3.05712 4.19043 3.45996V3.46094C4.19009 3.86393 3.86392 4.19008 3.46094 4.19043H3.45996C3.05712 4.19017 2.72983 3.86419 2.72949 3.46094V3.45996C2.72976 3.05686 3.05686 2.72976 3.45996 2.72949H3.46094ZM5.98926 1.58008C6.32235 1.64818 6.57324 1.94276 6.57324 2.2959L6.55859 2.44336C6.49022 2.7761 6.19456 3.02637 5.8418 3.02637C5.43884 3.02591 5.11251 2.69895 5.1123 2.2959L5.12695 2.14844C5.19504 1.81591 5.48906 1.56583 5.8418 1.56543L5.98926 1.58008Z"
                  strokeWidth="0.27"
                  style={{
                    stroke: thinkingEnabled
                      ? 'var(--app-qwen-ivory)'
                      : 'var(--app-secondary-foreground)',
                    fill: thinkingEnabled
                      ? 'var(--app-qwen-ivory)'
                      : 'var(--app-secondary-foreground)',
                  }}
                ></path>
              </svg>
            </button>

            {/* Command button */}
            <button
              type="button"
              className="flex items-center justify-center w-8 h-8 p-0 bg-transparent border border-transparent rounded-small cursor-pointer transition-all duration-150 flex-shrink-0 hover:bg-[var(--app-ghost-button-hover-background)] hover:text-[var(--app-primary-foreground)] [&>svg]:w-4 [&>svg]:h-4"
              style={{ color: 'var(--app-secondary-foreground)' }}
              title="Show command menu (/)"
              onClick={onShowCommandMenu}
            >
              <svg
                xmlns="http://www.w3.org/2000/svg"
                viewBox="0 0 20 20"
                fill="currentColor"
                aria-hidden="true"
              >
                <path
                  fillRule="evenodd"
                  d="M12.528 3.047a.75.75 0 0 1 .449.961L8.433 16.504a.75.75 0 1 1-1.41-.512l4.544-12.496a.75.75 0 0 1 .961-.449Z"
                  clipRule="evenodd"
                ></path>
              </svg>
            </button>

            {/* Attach button */}
            <button
              type="button"
              className="flex items-center justify-center w-8 h-8 p-0 bg-transparent border border-transparent rounded-small cursor-pointer transition-all duration-150 flex-shrink-0 hover:bg-[var(--app-ghost-button-hover-background)] hover:text-[var(--app-primary-foreground)] [&>svg]:w-4 [&>svg]:h-4"
              style={{ color: 'var(--app-secondary-foreground)' }}
              title="Attach context (Cmd/Ctrl + /)"
              onClick={onAttachContext}
            >
              <svg
                xmlns="http://www.w3.org/2000/svg"
                viewBox="0 0 20 20"
                fill="currentColor"
                aria-hidden="true"
              >
                <path
                  fillRule="evenodd"
                  d="M15.621 4.379a3 3 0 0 0-4.242 0l-7 7a3 3 0 0 0 4.241 4.243h.001l.497-.5a.75.75 0 0 1 1.064 1.057l-.498.501-.002.002a4.5 4.5 0 0 1-6.364-6.364l7-7a4.5 4.5 0 0 1 6.368 6.36l-3.455 3.553A2.625 2.625 0 1 1 9.52 9.52l3.45-3.451a.75.75 0 1 1 1.061 1.06l-3.45 3.451a1.125 1.125 0 0 0 1.587 1.595l3.454-3.553a3 3 0 0 0 0-4.242Z"
                  clipRule="evenodd"
                ></path>
              </svg>
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
              <svg
                xmlns="http://www.w3.org/2000/svg"
                viewBox="0 0 20 20"
                fill="currentColor"
                aria-hidden="true"
              >
                <path
                  fillRule="evenodd"
                  d="M10 17a.75.75 0 0 1-.75-.75V5.612L5.29 9.77a.75.75 0 0 1-1.08-1.04l5.25-5.5a.75.75 0 0 1 1.08 0l5.25 5.5a.75.75 0 1 1-1.08 1.04l-3.96-4.158V16.25A.75.75 0 0 1 10 17Z"
                  clipRule="evenodd"
                ></path>
              </svg>
            </button>
          </div>
        </form>
      </div>
    </div>
  );
};
