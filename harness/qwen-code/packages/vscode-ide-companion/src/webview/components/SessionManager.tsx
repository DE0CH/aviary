/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import React, { useState, useEffect } from 'react';
import { useVSCode } from '../hooks/useVSCode.js';
import {
  RefreshIcon,
  SaveDocumentIcon,
  SearchIcon,
  PlayIcon,
  SwitchIcon,
} from './icons/index.js';

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
          <RefreshIcon width="16" height="16" />
        </button>
      </div>

      <div className="session-manager-actions">
        <button className="secondary-button" onClick={handleSaveCurrent}>
          <SaveDocumentIcon width="16" height="16" />
          Save Current
        </button>
      </div>

      <div className="session-search">
        <SearchIcon width="16" height="16" />
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
                  <PlayIcon width="16" height="16" />
                </button>
                <button
                  className="icon-button"
                  onClick={() => handleSwitchSession(session.id)}
                  title="Switch to this conversation"
                >
                  <SwitchIcon width="16" height="16" />
                </button>
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  );
};
