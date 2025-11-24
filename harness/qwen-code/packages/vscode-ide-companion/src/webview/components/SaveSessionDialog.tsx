/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { useState, useEffect, useRef } from 'react';
import { CloseIcon } from './icons/index.js';

interface SaveSessionDialogProps {
  isOpen: boolean;
  onClose: () => void;
  onSave: (tag: string) => void;
  existingTags?: string[];
}

export const SaveSessionDialog: React.FC<SaveSessionDialogProps> = ({
  isOpen,
  onClose,
  onSave,
  existingTags = [],
}) => {
  const [tag, setTag] = useState('');
  const [error, setError] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (isOpen && inputRef.current) {
      // Focus the input when dialog opens
      setTimeout(() => {
        inputRef.current?.focus();
      }, 100);
    }
  }, [isOpen]);

  useEffect(() => {
    if (!isOpen) {
      // Reset state when dialog closes
      setTag('');
      setError('');
    }
  }, [isOpen]);

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();

    if (!tag.trim()) {
      setError('Please enter a name for this conversation');
      return;
    }

    // Check if tag already exists
    if (existingTags.includes(tag.trim())) {
      setError(
        'A conversation with this name already exists. Please choose a different name.',
      );
      return;
    }

    onSave(tag.trim());
  };

  if (!isOpen) {
    return null;
  }

  return (
    <div className="dialog-overlay" onClick={onClose}>
      <div className="dialog-content" onClick={(e) => e.stopPropagation()}>
        <div className="dialog-header">
          <h3>Save Conversation</h3>
          <button className="dialog-close" onClick={onClose} aria-label="Close">
            <CloseIcon width="16" height="16" />
          </button>
        </div>

        <form onSubmit={handleSubmit}>
          <div className="dialog-body">
            <div className="form-group">
              <label htmlFor="session-tag">Conversation Name</label>
              <input
                ref={inputRef}
                id="session-tag"
                type="text"
                value={tag}
                onChange={(e) => {
                  setTag(e.target.value);
                  if (error) {
                    setError('');
                  }
                }}
                placeholder="e.g., project-planning, bug-fix, research"
                className={error ? 'error' : ''}
              />
              {error && <div className="error-message">{error}</div>}
              <div className="form-help">
                Give this conversation a meaningful name so you can find it
                later
              </div>
            </div>
          </div>

          <div className="dialog-footer">
            <button
              type="button"
              className="secondary-button"
              onClick={onClose}
            >
              Cancel
            </button>
            <button type="submit" className="primary-button">
              Save Conversation
            </button>
          </div>
        </form>
      </div>
    </div>
  );
};
