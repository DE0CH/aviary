/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import React, { useState, useEffect, useRef } from 'react';
import { useVSCode } from './hooks/useVSCode.js';
import type { Conversation } from '../storage/ConversationStore.js';
import {
  PermissionRequest,
  type PermissionOption,
  type ToolCall as PermissionToolCall,
} from './components/PermissionRequest.js';
import { ToolCall, type ToolCallData } from './components/ToolCall.js';

interface ToolCallUpdate {
  type: 'tool_call' | 'tool_call_update';
  toolCallId: string;
  kind?: string;
  title?: string;
  status?: 'pending' | 'in_progress' | 'completed' | 'failed';
  rawInput?: unknown;
  content?: Array<{
    type: 'content' | 'diff';
    content?: {
      type: string;
      text?: string;
      [key: string]: unknown;
    };
    path?: string;
    oldText?: string | null;
    newText?: string;
    [key: string]: unknown;
  }>;
  locations?: Array<{
    path: string;
    line?: number | null;
  }>;
}

interface TextMessage {
  role: 'user' | 'assistant' | 'thinking';
  content: string;
  timestamp: number;
}

export const App: React.FC = () => {
  const vscode = useVSCode();
  const [messages, setMessages] = useState<TextMessage[]>([]);
  const [inputText, setInputText] = useState('');
  const [isStreaming, setIsStreaming] = useState(false);
  const [currentStreamContent, setCurrentStreamContent] = useState('');
  const [qwenSessions, setQwenSessions] = useState<
    Array<Record<string, unknown>>
  >([]);
  const [showSessionSelector, setShowSessionSelector] = useState(false);
  const [permissionRequest, setPermissionRequest] = useState<{
    options: PermissionOption[];
    toolCall: PermissionToolCall;
  } | null>(null);
  const [toolCalls, setToolCalls] = useState<Map<string, ToolCallData>>(
    new Map(),
  );
  const messagesEndRef = useRef<HTMLDivElement>(null);

  const handlePermissionRequest = React.useCallback(
    (request: {
      options: PermissionOption[];
      toolCall: PermissionToolCall;
    }) => {
      console.log('[WebView] Permission request received:', request);
      setPermissionRequest(request);
    },
    [],
  );

  const handlePermissionResponse = React.useCallback(
    (optionId: string) => {
      console.log('[WebView] Sending permission response:', optionId);
      vscode.postMessage({
        type: 'permissionResponse',
        data: { optionId },
      });
      setPermissionRequest(null);
    },
    [vscode],
  );

  const handleToolCallUpdate = React.useCallback((update: ToolCallUpdate) => {
    setToolCalls((prev) => {
      const newMap = new Map(prev);
      const existing = newMap.get(update.toolCallId);

      if (update.type === 'tool_call') {
        // New tool call - cast content to proper type
        const content = update.content?.map((item) => ({
          type: item.type as 'content' | 'diff',
          content: item.content,
          path: item.path,
          oldText: item.oldText,
          newText: item.newText,
        }));

        newMap.set(update.toolCallId, {
          toolCallId: update.toolCallId,
          kind: update.kind || 'other',
          title: update.title || 'Tool Call',
          status: update.status || 'pending',
          rawInput: update.rawInput as string | object | undefined,
          content,
          locations: update.locations,
        });
      } else if (update.type === 'tool_call_update' && existing) {
        // Update existing tool call
        const updatedContent = update.content
          ? update.content.map((item) => ({
              type: item.type as 'content' | 'diff',
              content: item.content,
              path: item.path,
              oldText: item.oldText,
              newText: item.newText,
            }))
          : undefined;

        newMap.set(update.toolCallId, {
          ...existing,
          ...(update.kind && { kind: update.kind }),
          ...(update.title && { title: update.title }),
          ...(update.status && { status: update.status }),
          ...(updatedContent && { content: updatedContent }),
          ...(update.locations && { locations: update.locations }),
        });
      }

      return newMap;
    });
  }, []);

  useEffect(() => {
    // Listen for messages from extension
    const handleMessage = (event: MessageEvent) => {
      const message = event.data;

      switch (message.type) {
        case 'conversationLoaded': {
          const conversation = message.data as Conversation;
          setMessages(conversation.messages);
          break;
        }

        case 'message': {
          const newMessage = message.data as TextMessage;
          setMessages((prev) => [...prev, newMessage]);
          break;
        }

        case 'streamStart':
          setIsStreaming(true);
          setCurrentStreamContent('');
          break;

        case 'streamChunk': {
          const chunkData = message.data;
          if (chunkData.role === 'thinking') {
            // Handle thinking chunks separately if needed
            setCurrentStreamContent((prev) => prev + chunkData.chunk);
          } else {
            setCurrentStreamContent((prev) => prev + chunkData.chunk);
          }
          break;
        }

        case 'streamEnd':
          // Finalize the streamed message
          if (currentStreamContent) {
            const assistantMessage: TextMessage = {
              role: 'assistant',
              content: currentStreamContent,
              timestamp: Date.now(),
            };
            setMessages((prev) => [...prev, assistantMessage]);
          }
          setIsStreaming(false);
          setCurrentStreamContent('');
          break;

        case 'error':
          console.error('Error from extension:', message.data.message);
          setIsStreaming(false);
          break;

        case 'permissionRequest':
          // Show permission dialog
          handlePermissionRequest(message.data);
          break;

        case 'toolCall':
        case 'toolCallUpdate':
          // Handle tool call updates
          handleToolCallUpdate(message.data);
          break;

        case 'qwenSessionList':
          setQwenSessions(message.data.sessions || []);
          break;

        case 'qwenSessionSwitched':
          setShowSessionSelector(false);
          // Load messages from the session
          if (message.data.messages) {
            setMessages(message.data.messages);
          } else {
            setMessages([]);
          }
          setCurrentStreamContent('');
          setToolCalls(new Map());
          break;

        case 'conversationCleared':
          setMessages([]);
          setCurrentStreamContent('');
          setToolCalls(new Map());
          break;

        default:
          break;
      }
    };

    window.addEventListener('message', handleMessage);
    return () => window.removeEventListener('message', handleMessage);
  }, [currentStreamContent, handlePermissionRequest, handleToolCallUpdate]);

  useEffect(() => {
    // Auto-scroll to bottom when messages change
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, currentStreamContent]);

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();

    if (!inputText.trim() || isStreaming) {
      console.log('Submit blocked:', { inputText, isStreaming });
      return;
    }

    console.log('Sending message:', inputText);
    vscode.postMessage({
      type: 'sendMessage',
      data: { text: inputText },
    });

    setInputText('');
  };

  const handleLoadQwenSessions = () => {
    vscode.postMessage({ type: 'getQwenSessions', data: {} });
    setShowSessionSelector(true);
  };

  const handleNewQwenSession = () => {
    vscode.postMessage({ type: 'newQwenSession', data: {} });
    setShowSessionSelector(false);
    // Clear messages in UI
    setMessages([]);
    setCurrentStreamContent('');
  };

  const handleSwitchSession = (sessionId: string) => {
    vscode.postMessage({
      type: 'switchQwenSession',
      data: { sessionId },
    });
  };

  return (
    <div className="chat-container">
      {showSessionSelector && (
        <div className="session-selector-overlay">
          <div className="session-selector">
            <div className="session-selector-header">
              <h3>Qwen Sessions</h3>
              <button onClick={() => setShowSessionSelector(false)}>✕</button>
            </div>
            <div className="session-selector-actions">
              <button
                className="new-session-button"
                onClick={handleNewQwenSession}
              >
                ➕ New Session
              </button>
            </div>
            <div className="session-list">
              {qwenSessions.length === 0 ? (
                <p className="no-sessions">No sessions available</p>
              ) : (
                qwenSessions.map((session) => {
                  const sessionId =
                    (session.id as string) ||
                    (session.sessionId as string) ||
                    '';
                  const title =
                    (session.title as string) ||
                    (session.name as string) ||
                    'Untitled Session';
                  const lastUpdated =
                    (session.lastUpdated as string) ||
                    (session.startTime as string) ||
                    '';
                  const messageCount = (session.messageCount as number) || 0;

                  return (
                    <div
                      key={sessionId}
                      className="session-item"
                      onClick={() => handleSwitchSession(sessionId)}
                    >
                      <div className="session-title">{title}</div>
                      <div className="session-meta">
                        <span className="session-time">
                          {new Date(lastUpdated).toLocaleString()}
                        </span>
                        <span className="session-count">
                          {messageCount} messages
                        </span>
                      </div>
                      <div className="session-id">
                        {sessionId.substring(0, 8)}...
                      </div>
                    </div>
                  );
                })
              )}
            </div>
          </div>
        </div>
      )}

      <div className="chat-header">
        <button className="session-button" onClick={handleLoadQwenSessions}>
          📋 Sessions
        </button>
      </div>

      <div className="messages-container">
        {messages.map((msg, index) => (
          <div key={index} className={`message ${msg.role}`}>
            <div className="message-content">{msg.content}</div>
            <div className="message-timestamp">
              {new Date(msg.timestamp).toLocaleTimeString()}
            </div>
          </div>
        ))}

        {/* Tool Calls */}
        {Array.from(toolCalls.values()).map((toolCall) => (
          <ToolCall key={toolCall.toolCallId} toolCall={toolCall} />
        ))}

        {/* Permission Request */}
        {permissionRequest && (
          <PermissionRequest
            options={permissionRequest.options}
            toolCall={permissionRequest.toolCall}
            onResponse={handlePermissionResponse}
          />
        )}

        {isStreaming && currentStreamContent && (
          <div className="message assistant streaming">
            <div className="message-content">{currentStreamContent}</div>
            <div className="streaming-indicator">●</div>
          </div>
        )}

        <div ref={messagesEndRef} />
      </div>

      <form className="input-form" onSubmit={handleSubmit}>
        <input
          type="text"
          className="input-field"
          placeholder="Type your message..."
          value={inputText}
          onChange={(e) => setInputText((e.target as HTMLInputElement).value)}
          disabled={isStreaming}
        />
        <button
          type="submit"
          className="send-button"
          disabled={isStreaming || !inputText.trim()}
        >
          Send
        </button>
      </form>
    </div>
  );
};
