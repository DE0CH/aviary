/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import React, { useState, useEffect } from 'react';
import { useVSCode } from '../hooks/useVSCode.js';

interface Session {
  id: string;
  name: string;
  lastUpdated: string;
  messageCount: number;
}

interface SessionManagerProps {
  currentSessionId: string | null;
  onSwitchSession: (sessionId: string) => void;
  onSaveSession: () => void;
  onResumeSession: (sessionId: string) => void;
}

export const SessionManager: React.FC<SessionManagerProps> = ({
  currentSessionId,
  onSwitchSession,
  onSaveSession,
  onResumeSession,
}) => {
  const vscode = useVSCode();
  const [sessions, setSessions] = useState<Session[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [searchQuery, setSearchQuery] = useState('');

  const loadSessions = React.useCallback(() => {
    setIsLoading(true);
    vscode.postMessage({
      type: 'listSavedSessions',
      data: {},
    });
  }, [vscode]);

  // Load sessions when component mounts
  useEffect(() => {
    loadSessions();
  }, [loadSessions]);

  // Listen for session list updates
  useEffect(() => {
    const handleMessage = (event: MessageEvent) => {
      const message = event.data;

      if (message.type === 'savedSessionsList') {
        setIsLoading(false);
        setSessions(message.data.sessions || []);
      }
    };

    window.addEventListener('message', handleMessage);
    return () => window.removeEventListener('message', handleMessage);
  }, []);

  const filteredSessions = sessions.filter((session) =>
    session.name.toLowerCase().includes(searchQuery.toLowerCase()),
  );

  const handleSaveCurrent = () => {
    onSaveSession();
  };

  const handleResumeSession = (sessionId: string) => {
    onResumeSession(sessionId);
  };

  const handleSwitchSession = (sessionId: string) => {
    onSwitchSession(sessionId);
  };

  return (
    <div className="session-manager">
      <div className="session-manager-header">
        <h3>Saved Conversations</h3>
        <button
          className="icon-button"
          onClick={loadSessions}
          disabled={isLoading}
          title="Refresh sessions"
        >
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
            <path
              d="M13.3333 8C13.3333 10.9455 10.9455 13.3333 8 13.3333C5.05451 13.3333 2.66663 10.9455 2.66663 8C2.66663 5.05451 5.05451 2.66663 8 2.66663"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
            />
            <path
              d="M10.6666 8L13.3333 8M13.3333 8L13.3333 5.33333M13.3333 8L10.6666 10.6667"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        </button>
      </div>

      <div className="session-manager-actions">
        <button className="secondary-button" onClick={handleSaveCurrent}>
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
            <path
              d="M2.66663 2.66663H10.6666L13.3333 5.33329V13.3333H2.66663V2.66663Z"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
            <path
              d="M8 10.6666V8M8 8V5.33329M8 8H10.6666M8 8H5.33329"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
          Save Current
        </button>
      </div>

      <div className="session-search">
        <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
          <path
            d="M7.33329 12.6666C10.2788 12.6666 12.6666 10.2788 12.6666 7.33329C12.6666 4.38777 10.2788 2 7.33329 2C4.38777 2 2 4.38777 2 7.33329C2 10.2788 4.38777 12.6666 7.33329 12.6666Z"
            stroke="currentColor"
            strokeWidth="1.5"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
          <path
            d="M13.9999 14L11.0999 11.1"
            stroke="currentColor"
            strokeWidth="1.5"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
        <input
          type="text"
          placeholder="Search conversations..."
          value={searchQuery}
          onChange={(e) => setSearchQuery(e.target.value)}
        />
      </div>

      <div className="session-list">
        {isLoading ? (
          <div className="session-list-loading">
            <div className="loading-spinner"></div>
            <span>Loading conversations...</span>
          </div>
        ) : filteredSessions.length === 0 ? (
          <div className="session-list-empty">
            {searchQuery
              ? 'No matching conversations'
              : 'No saved conversations yet'}
          </div>
        ) : (
          filteredSessions.map((session) => (
            <div
              key={session.id}
              className={`session-item ${session.id === currentSessionId ? 'active' : ''}`}
            >
              <div className="session-item-info">
                <div className="session-item-name">{session.name}</div>
                <div className="session-item-meta">
                  <span className="session-item-date">
                    {new Date(session.lastUpdated).toLocaleDateString()}
                  </span>
                  <span className="session-item-count">
                    {session.messageCount}{' '}
                    {session.messageCount === 1 ? 'message' : 'messages'}
                  </span>
                </div>
              </div>
              <div className="session-item-actions">
                <button
                  className="icon-button"
                  onClick={() => handleResumeSession(session.id)}
                  title="Resume this conversation"
                >
                  <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
                    <path
                      d="M5.33337 4L10.6667 8L5.33337 12"
                      stroke="currentColor"
                      strokeWidth="1.5"
                      strokeLinecap="round"
                      strokeLinejoin="round"
                    />
                  </svg>
                </button>
                <button
                  className="icon-button"
                  onClick={() => handleSwitchSession(session.id)}
                  title="Switch to this conversation"
                >
                  <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
                    <path
                      d="M10.6666 4L13.3333 6.66667L10.6666 9.33333"
                      stroke="currentColor"
                      strokeWidth="1.5"
                      strokeLinecap="round"
                      strokeLinejoin="round"
                    />
                    <path
                      d="M2.66663 6.66667H13.3333"
                      stroke="currentColor"
                      strokeWidth="1.5"
                      strokeLinecap="round"
                    />
                  </svg>
                </button>
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  );
};
