/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { ChevronDownIcon, PlusIcon } from '../icons/index.js';

interface ChatHeaderProps {
  currentSessionTitle: string;
  onLoadSessions: () => void;
  onSaveSession: () => void;
  onNewSession: () => void;
}

export const ChatHeader: React.FC<ChatHeaderProps> = ({
  currentSessionTitle,
  onLoadSessions,
  onSaveSession: _onSaveSession,
  onNewSession,
}) => (
  <div
    className="flex gap-1 select-none py-1.5 px-2.5"
    style={{
      borderBottom: '1px solid var(--app-primary-border-color)',
      backgroundColor: 'var(--app-header-background)',
    }}
  >
    {/* Past Conversations Button */}
    <button
      className="flex-none py-1 px-2 bg-transparent border border-transparent rounded cursor-pointer flex items-center justify-center outline-none font-medium transition-colors duration-200 hover:bg-[var(--app-ghost-button-hover-background)] focus:bg-[var(--app-ghost-button-hover-background)]"
      style={{
        borderRadius: 'var(--corner-radius-small)',
        color: 'var(--app-primary-foreground)',
        fontSize: 'var(--vscode-chat-font-size, 13px)',
      }}
      onClick={onLoadSessions}
      title="Past conversations"
    >
      <span className="flex items-center gap-1">
        <span style={{ fontSize: 'var(--vscode-chat-font-size, 13px)' }}>
          {currentSessionTitle}
        </span>
        <ChevronDownIcon className="w-3.5 h-3.5" />
      </span>
    </button>

    {/* Spacer */}
    <div className="flex-1"></div>

    {/* Save Session Button */}
    {/* <button
        className="flex-none p-0 bg-transparent border border-transparent rounded cursor-pointer flex items-center justify-center outline-none w-6 h-6 hover:bg-[var(--app-ghost-button-hover-background)] focus:bg-[var(--app-ghost-button-hover-background)]"
        style={{
          color: 'var(--app-primary-foreground)',
        }}
        onClick={onSaveSession}
        title="Save Conversation"
      >
        <svg
          xmlns="http://www.w3.org/2000/svg"
          viewBox="0 0 20 20"
          fill="currentColor"
          aria-hidden="true"
          data-slot="icon"
          className="w-4 h-4"
        >
          <path
            fillRule="evenodd"
            d="M4.25 2A2.25 2.25 0 0 0 2 4.25v11.5A2.25 2.25 0 0 0 4.25 18h11.5A2.25 2.25 0 0 0 18 15.75V8.25a.75.75 0 0 1 .217-.517l.083-.083a.75.75 0 0 1 1.061 0l2.239 2.239A.75.75 0 0 1 22 10.5v5.25a4.75 4.75 0 0 1-4.75 4.75H4.75A4.75 4.75 0 0 1 0 15.75V4.25A4.75 4.75 0 0 1 4.75 0h5a.75.75 0 0 1 0 1.5h-5ZM9.017 6.5a1.5 1.5 0 0 1 2.072.58l.43.862a1 1 0 0 0 .895.558h3.272a1.5 1.5 0 0 1 1.5 1.5v6.75a1.5 1.5 0 0 1-1.5 1.5h-7.5a1.5 1.5 0 0 1-1.5-1.5v-6.75a1.5 1.5 0 0 1 1.5-1.5h1.25a1 1 0 0 0 .895-.558l.43-.862a1.5 1.5 0 0 1 .511-.732ZM11.78 8.47a.75.75 0 0 0-1.06-1.06L8.75 9.379 7.78 8.41a.75.75 0 0 0-1.06 1.06l1.5 1.5a.75.75 0 0 0 1.06 0l2.5-2.5Z"
            clipRule="evenodd"
          ></path>
        </svg>
      </button> */}

    {/* New Session Button */}
    <button
      className="flex-none p-0 bg-transparent border border-transparent rounded cursor-pointer flex items-center justify-center outline-none w-6 h-6 hover:bg-[var(--app-ghost-button-hover-background)] focus:bg-[var(--app-ghost-button-hover-background)]"
      style={{
        color: 'var(--app-primary-foreground)',
      }}
      onClick={onNewSession}
      title="New Session"
    >
      <PlusIcon className="w-4 h-4" />
    </button>
  </div>
);
