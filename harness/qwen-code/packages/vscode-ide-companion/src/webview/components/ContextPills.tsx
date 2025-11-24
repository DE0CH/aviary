/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import type { ContextAttachment } from '../ContextAttachmentManager.js';
import {
  FileListIcon,
  PlusSmallIcon,
  SymbolIcon,
  SelectionIcon,
  CloseSmallIcon,
} from './icons/index.js';
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
        return <FileListIcon />;
      case 'symbol':
        return <SymbolIcon />;
      case 'selection':
        return <SelectionIcon />;
      default:
        return <PlusSmallIcon />;
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
            <CloseSmallIcon />
          </button>
        </div>
      ))}
    </div>
  );
};
