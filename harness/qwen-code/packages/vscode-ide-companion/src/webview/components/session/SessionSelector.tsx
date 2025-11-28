/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import React from 'react';
import { groupSessionsByDate } from '../../utils/sessionGrouping.js';
import { getTimeAgo } from '../../utils/timeUtils.js';
import { SearchIcon } from '../icons/index.js';

interface SessionSelectorProps {
  visible: boolean;
  sessions: Array<Record<string, unknown>>;
  currentSessionId: string | null;
  searchQuery: string;
  onSearchChange: (query: string) => void;
  onSelectSession: (sessionId: string) => void;
  onClose: () => void;
}

/**
 * Session selector component
 * Display session list and support search and selection
 */
export const SessionSelector: React.FC<SessionSelectorProps> = ({
  visible,
  sessions,
  currentSessionId,
  searchQuery,
  onSearchChange,
  onSelectSession,
  onClose,
}) => {
  if (!visible) {
    return null;
  }

  const hasNoSessions = sessions.length === 0;

  return (
    <>
      <div className="session-selector-backdrop" onClick={onClose} />
      <div
        className="session-dropdown"
        tabIndex={-1}
        style={{
          top: '34px',
          left: '10px',
        }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Search Box */}
        <div className="session-search">
          <SearchIcon className="session-search-icon" />
          <input
            type="text"
            className="session-search-input"
            placeholder="Search sessions…"
            value={searchQuery}
            onChange={(e) => onSearchChange(e.target.value)}
          />
        </div>

        {/* Session List with Grouping */}
        <div className="session-list-content">
          {hasNoSessions ? (
            <div
              style={{
                padding: '20px',
                textAlign: 'center',
                color: 'var(--app-secondary-foreground)',
              }}
            >
              {searchQuery ? 'No matching sessions' : 'No sessions available'}
            </div>
          ) : (
            groupSessionsByDate(sessions).map((group) => (
              <React.Fragment key={group.label}>
                <div className="session-group-label">{group.label}</div>
                <div className="session-group">
                  {group.sessions.map((session) => {
                    const sessionId =
                      (session.id as string) ||
                      (session.sessionId as string) ||
                      '';
                    const title =
                      (session.title as string) ||
                      (session.name as string) ||
                      'Untitled';
                    const lastUpdated =
                      (session.lastUpdated as string) ||
                      (session.startTime as string) ||
                      '';
                    const isActive = sessionId === currentSessionId;

                    return (
                      <button
                        key={sessionId}
                        className={`session-item ${isActive ? 'active' : ''}`}
                        onClick={() => {
                          onSelectSession(sessionId);
                          onClose();
                        }}
                      >
                        <span className="session-item-title">{title}</span>
                        <span className="session-item-time">
                          {getTimeAgo(lastUpdated)}
                        </span>
                      </button>
                    );
                  })}
                </div>
              </React.Fragment>
            ))
          )}
        </div>
      </div>
    </>
  );
};
