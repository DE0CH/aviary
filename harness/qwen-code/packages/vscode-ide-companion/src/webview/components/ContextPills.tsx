/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import type { ContextAttachment } from '../ContextAttachmentManager.js';
import './ContextPills.css';

interface ContextPillsProps {
  attachments: ContextAttachment[];
  onRemove: (id: string) => void;
}

/**
 * Display attached context as pills/chips
 * Similar to ChatContextAttachments UI in vscode-copilot-chat
 */
export const ContextPills: React.FC<ContextPillsProps> = ({
  attachments,
  onRemove,
}) => {
  if (attachments.length === 0) {
    return null;
  }

  const getIcon = (type: string) => {
    switch (type) {
      case 'file':
        return (
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 16 16"
            fill="currentColor"
          >
            <path d="M5 3.5a.5.5 0 0 1 .5-.5h4a.5.5 0 0 1 0 1h-4a.5.5 0 0 1-.5-.5Zm0 2a.5.5 0 0 1 .5-.5h7a.5.5 0 0 1 0 1h-7a.5.5 0 0 1-.5-.5Zm0 2a.5.5 0 0 1 .5-.5h7a.5.5 0 0 1 0 1h-7a.5.5 0 0 1-.5-.5Zm0 2a.5.5 0 0 1 .5-.5h7a.5.5 0 0 1 0 1h-7a.5.5 0 0 1-.5-.5Z" />
          </svg>
        );
      case 'symbol':
        return (
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 16 16"
            fill="currentColor"
          >
            <path d="M8 1a.5.5 0 0 1 .5.5v5.793l2.146-2.147a.5.5 0 0 1 .708.708l-3 3a.5.5 0 0 1-.708 0l-3-3a.5.5 0 1 1 .708-.708L7.5 7.293V1.5A.5.5 0 0 1 8 1Z" />
          </svg>
        );
      case 'selection':
        return (
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 16 16"
            fill="currentColor"
          >
            <path d="M2 3.5a.5.5 0 0 1 .5-.5h11a.5.5 0 0 1 0 1h-11a.5.5 0 0 1-.5-.5Zm0 4a.5.5 0 0 1 .5-.5h7a.5.5 0 0 1 0 1h-7a.5.5 0 0 1-.5-.5Zm0 4a.5.5 0 0 1 .5-.5h9a.5.5 0 0 1 0 1h-9a.5.5 0 0 1-.5-.5Z" />
          </svg>
        );
      default:
        return (
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 16 16"
            fill="currentColor"
          >
            <path d="M8 2a.5.5 0 0 1 .5.5V5h2.5a.5.5 0 0 1 0 1H8.5v2.5a.5.5 0 0 1-1 0V6H5a.5.5 0 0 1 0-1h2.5V2.5A.5.5 0 0 1 8 2Z" />
          </svg>
        );
    }
  };

  return (
    <div className="context-pills-container">
      {attachments.map((attachment) => (
        <div key={attachment.id} className="context-pill">
          <div className="context-pill-icon">{getIcon(attachment.type)}</div>
          <div className="context-pill-label">{attachment.name}</div>
          <button
            className="context-pill-remove"
            onClick={() => onRemove(attachment.id)}
            aria-label="Remove attachment"
          >
            <svg
              xmlns="http://www.w3.org/2000/svg"
              viewBox="0 0 16 16"
              fill="currentColor"
            >
              <path d="M4.646 4.646a.5.5 0 0 1 .708 0L8 7.293l2.646-2.647a.5.5 0 0 1 .708.708L8.707 8l2.647 2.646a.5.5 0 0 1-.708.708L8 8.707l-2.646 2.647a.5.5 0 0 1-.708-.708L7.293 8 4.646 5.354a.5.5 0 0 1 0-.708Z" />
            </svg>
          </button>
        </div>
      ))}
    </div>
  );
};
