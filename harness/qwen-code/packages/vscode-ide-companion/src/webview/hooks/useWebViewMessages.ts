/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import { useEffect, useRef, useCallback } from 'react';
import type { Conversation } from '../../storage/conversationStore.js';
import type {
  PermissionOption,
  ToolCall as PermissionToolCall,
} from '../components/PermissionRequest.js';
import type { PlanEntry } from '../components/PlanDisplay.js';
import type { ToolCallUpdate } from '../types/toolCall.js';

interface UseWebViewMessagesProps {
  // Session management
  sessionManagement: {
    currentSessionId: string | null;
    setQwenSessions: (sessions: Array<Record<string, unknown>>) => void;
    setCurrentSessionId: (id: string | null) => void;
    setCurrentSessionTitle: (title: string) => void;
    setShowSessionSelector: (show: boolean) => void;
    handleSaveSessionResponse: (response: {
      success: boolean;
      message?: string;
    }) => void;
  };

  // File context
  fileContext: {
    setActiveFileName: (name: string | null) => void;
    setActiveFilePath: (path: string | null) => void;
    setActiveSelection: (
      selection: { startLine: number; endLine: number } | null,
    ) => void;
    setWorkspaceFiles: (
      files: Array<{
        id: string;
        label: string;
        description: string;
        path: string;
      }>,
    ) => void;
    addFileReference: (name: string, path: string) => void;
  };

  // Message handling
  messageHandling: {
    setMessages: (
      messages: Array<{
        role: 'user' | 'assistant' | 'thinking';
        content: string;
        timestamp: number;
        fileContext?: {
          fileName: string;
          filePath: string;
          startLine?: number;
          endLine?: number;
        };
      }>,
    ) => void;
    addMessage: (message: {
      role: 'user' | 'assistant' | 'thinking';
      content: string;
      timestamp: number;
    }) => void;
    clearMessages: () => void;
    startStreaming: () => void;
    appendStreamChunk: (chunk: string) => void;
    endStreaming: () => void;
    clearWaitingForResponse: () => void;
  };

  // Tool calls
  handleToolCallUpdate: (update: ToolCallUpdate) => void;
  clearToolCalls: () => void;

  // Plan
  setPlanEntries: (entries: PlanEntry[]) => void;

  // Permission
  handlePermissionRequest: (request: {
    options: PermissionOption[];
    toolCall: PermissionToolCall;
  }) => void;

  // Input
  inputFieldRef: React.RefObject<HTMLDivElement>;
  setInputText: (text: string) => void;
}

/**
 * WebView message handling Hook
 * Handles all messages from VSCode Extension uniformly
 */
export const useWebViewMessages = ({
  sessionManagement,
  fileContext,
  messageHandling,
  handleToolCallUpdate,
  clearToolCalls,
  setPlanEntries,
  handlePermissionRequest,
  inputFieldRef,
  setInputText,
}: UseWebViewMessagesProps) => {
  // Use ref to store callbacks to avoid useEffect dependency issues
  const handlersRef = useRef({
    sessionManagement,
    fileContext,
    messageHandling,
    handleToolCallUpdate,
    clearToolCalls,
    setPlanEntries,
    handlePermissionRequest,
  });

  // Update refs
  useEffect(() => {
    handlersRef.current = {
      sessionManagement,
      fileContext,
      messageHandling,
      handleToolCallUpdate,
      clearToolCalls,
      setPlanEntries,
      handlePermissionRequest,
    };
  });

  const handleMessage = useCallback(
    (event: MessageEvent) => {
      const message = event.data;
      const handlers = handlersRef.current;

      switch (message.type) {
        case 'conversationLoaded': {
          const conversation = message.data as Conversation;
          handlers.messageHandling.setMessages(conversation.messages);
          break;
        }

        case 'message': {
          handlers.messageHandling.addMessage(message.data);
          break;
        }

        case 'streamStart':
          handlers.messageHandling.startStreaming();
          break;

        case 'streamChunk': {
          handlers.messageHandling.appendStreamChunk(message.data.chunk);
          break;
        }

        case 'thoughtChunk': {
          const thinkingMessage = {
            role: 'thinking' as const,
            content: message.data.content || message.data.chunk || '',
            timestamp: Date.now(),
          };
          handlers.messageHandling.addMessage(thinkingMessage);
          break;
        }

        case 'streamEnd':
          handlers.messageHandling.endStreaming();
          break;

        case 'error':
          handlers.messageHandling.clearWaitingForResponse();
          break;

        case 'permissionRequest': {
          handlers.handlePermissionRequest(message.data);

          const permToolCall = message.data?.toolCall as {
            toolCallId?: string;
            kind?: string;
            title?: string;
            status?: string;
            content?: unknown[];
            locations?: Array<{ path: string; line?: number | null }>;
          };

          if (permToolCall?.toolCallId) {
            let kind = permToolCall.kind || 'execute';
            if (permToolCall.title) {
              const title = permToolCall.title.toLowerCase();
              if (title.includes('touch') || title.includes('echo')) {
                kind = 'execute';
              } else if (title.includes('read') || title.includes('cat')) {
                kind = 'read';
              } else if (title.includes('write') || title.includes('edit')) {
                kind = 'edit';
              }
            }

            const normalizedStatus = (
              permToolCall.status === 'pending' ||
              permToolCall.status === 'in_progress' ||
              permToolCall.status === 'completed' ||
              permToolCall.status === 'failed'
                ? permToolCall.status
                : 'pending'
            ) as ToolCallUpdate['status'];

            handlers.handleToolCallUpdate({
              type: 'tool_call',
              toolCallId: permToolCall.toolCallId,
              kind,
              title: permToolCall.title,
              status: normalizedStatus,
              content: permToolCall.content as ToolCallUpdate['content'],
              locations: permToolCall.locations,
            });
          }
          break;
        }

        case 'plan':
          if (message.data.entries && Array.isArray(message.data.entries)) {
            handlers.setPlanEntries(message.data.entries as PlanEntry[]);
          }
          break;

        case 'toolCall':
        case 'toolCallUpdate': {
          const toolCallData = message.data;
          if (toolCallData.sessionUpdate && !toolCallData.type) {
            toolCallData.type = toolCallData.sessionUpdate;
          }
          handlers.handleToolCallUpdate(toolCallData);
          break;
        }

        case 'qwenSessionList': {
          const sessions = message.data.sessions || [];
          handlers.sessionManagement.setQwenSessions(sessions);
          if (
            handlers.sessionManagement.currentSessionId &&
            sessions.length > 0
          ) {
            const currentSession = sessions.find(
              (s: Record<string, unknown>) =>
                (s.id as string) ===
                  handlers.sessionManagement.currentSessionId ||
                (s.sessionId as string) ===
                  handlers.sessionManagement.currentSessionId,
            );
            if (currentSession) {
              const title =
                (currentSession.title as string) ||
                (currentSession.name as string) ||
                'Past Conversations';
              handlers.sessionManagement.setCurrentSessionTitle(title);
            }
          }
          break;
        }

        case 'qwenSessionSwitched':
          handlers.sessionManagement.setShowSessionSelector(false);
          if (message.data.sessionId) {
            handlers.sessionManagement.setCurrentSessionId(
              message.data.sessionId as string,
            );
          }
          if (message.data.session) {
            const session = message.data.session as Record<string, unknown>;
            const title =
              (session.title as string) ||
              (session.name as string) ||
              'Past Conversations';
            handlers.sessionManagement.setCurrentSessionTitle(title);
          }
          if (message.data.messages) {
            handlers.messageHandling.setMessages(message.data.messages);
          } else {
            handlers.messageHandling.clearMessages();
          }
          handlers.clearToolCalls();
          handlers.setPlanEntries([]);
          break;

        case 'conversationCleared':
          handlers.messageHandling.clearMessages();
          handlers.clearToolCalls();
          handlers.sessionManagement.setCurrentSessionId(null);
          handlers.sessionManagement.setCurrentSessionTitle(
            'Past Conversations',
          );
          break;

        case 'sessionTitleUpdated': {
          const sessionId = message.data?.sessionId as string;
          const title = message.data?.title as string;
          if (sessionId && title) {
            handlers.sessionManagement.setCurrentSessionId(sessionId);
            handlers.sessionManagement.setCurrentSessionTitle(title);
          }
          break;
        }

        case 'activeEditorChanged': {
          const fileName = message.data?.fileName as string | null;
          const filePath = message.data?.filePath as string | null;
          const selection = message.data?.selection as {
            startLine: number;
            endLine: number;
          } | null;
          handlers.fileContext.setActiveFileName(fileName);
          handlers.fileContext.setActiveFilePath(filePath);
          handlers.fileContext.setActiveSelection(selection);
          break;
        }

        case 'fileAttached': {
          const attachment = message.data as {
            id: string;
            type: string;
            name: string;
            value: string;
          };

          handlers.fileContext.addFileReference(
            attachment.name,
            attachment.value,
          );

          if (inputFieldRef.current) {
            const currentText = inputFieldRef.current.textContent || '';
            const newText = currentText
              ? `${currentText} @${attachment.name} `
              : `@${attachment.name} `;
            inputFieldRef.current.textContent = newText;
            setInputText(newText);

            const range = document.createRange();
            const sel = window.getSelection();
            range.selectNodeContents(inputFieldRef.current);
            range.collapse(false);
            sel?.removeAllRanges();
            sel?.addRange(range);
          }
          break;
        }

        case 'workspaceFiles': {
          const files = message.data?.files as Array<{
            id: string;
            label: string;
            description: string;
            path: string;
          }>;
          if (files) {
            handlers.fileContext.setWorkspaceFiles(files);
          }
          break;
        }

        case 'saveSessionResponse': {
          handlers.sessionManagement.handleSaveSessionResponse(message.data);
          break;
        }

        default:
          break;
      }
    },
    [inputFieldRef, setInputText],
  );

  useEffect(() => {
    window.addEventListener('message', handleMessage);
    return () => window.removeEventListener('message', handleMessage);
  }, [handleMessage]);
};
