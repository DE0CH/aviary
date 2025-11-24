/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import React, { useState, useEffect, useRef } from 'react';
import { useVSCode } from './hooks/useVSCode.js';
import { useSessionManagement } from './hooks/session/useSessionManagement.js';
import { useFileContext } from './hooks/file/useFileContext.js';
import { useMessageHandling } from './hooks/message/useMessageHandling.js';
import { useToolCalls } from './hooks/useToolCalls.js';
import { useWebViewMessages } from './hooks/useWebViewMessages.js';
import { useMessageSubmit } from './hooks/useMessageSubmit.js';
import type {
  PermissionOption,
  ToolCall as PermissionToolCall,
} from './components/PermissionRequest.js';
import { PermissionDrawer } from './components/PermissionDrawer.js';
import { ToolCall } from './components/ToolCall.js';
import { hasToolCallOutput } from './components/toolcalls/shared/utils.js';
import { InProgressToolCall } from './components/InProgressToolCall.js';
import { EmptyState } from './components/EmptyState.js';
import { PlanDisplay, type PlanEntry } from './components/PlanDisplay.js';
import {
  CompletionMenu,
  type CompletionItem,
} from './components/CompletionMenu.js';
import { useCompletionTrigger } from './hooks/useCompletionTrigger.js';
import { SaveSessionDialog } from './components/SaveSessionDialog.js';
import { InfoBanner } from './components/InfoBanner.js';
import { ChatHeader } from './components/layouts/ChatHeader.js';
import {
  UserMessage,
  AssistantMessage,
  ThinkingMessage,
  StreamingMessage,
  WaitingMessage,
} from './components/messages/index.js';
import { InputForm } from './components/InputForm.js';
import { SessionSelector } from './components/session/SessionSelector.js';
import { FileIcon, UserIcon } from './components/icons/index.js';
import type { EditMode } from './types/toolCall.js';

export const App: React.FC = () => {
  const vscode = useVSCode();

  // Core hooks
  const sessionManagement = useSessionManagement(vscode);
  const fileContext = useFileContext(vscode);
  const messageHandling = useMessageHandling();
  const {
    inProgressToolCalls,
    completedToolCalls,
    handleToolCallUpdate,
    clearToolCalls,
  } = useToolCalls();

  // UI state
  const [inputText, setInputText] = useState('');
  const [permissionRequest, setPermissionRequest] = useState<{
    options: PermissionOption[];
    toolCall: PermissionToolCall;
  } | null>(null);
  const [planEntries, setPlanEntries] = useState<PlanEntry[]>([]);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputFieldRef = useRef<HTMLDivElement>(null);
  const [showBanner, setShowBanner] = useState(true);
  const [editMode, setEditMode] = useState<EditMode>('ask');
  const [thinkingEnabled, setThinkingEnabled] = useState(false);
  const [isComposing, setIsComposing] = useState(false);
  const [showSaveDialog, setShowSaveDialog] = useState(false);

  // Completion system
  const getCompletionItems = React.useCallback(
    async (trigger: '@' | '/', query: string): Promise<CompletionItem[]> => {
      if (trigger === '@') {
        if (!fileContext.hasRequestedFiles) {
          fileContext.requestWorkspaceFiles();
        }

        const fileIcon = <FileIcon />;
        const allItems: CompletionItem[] = fileContext.workspaceFiles.map(
          (file) => ({
            id: file.id,
            label: file.label,
            description: file.description,
            type: 'file' as const,
            icon: fileIcon,
            value: file.path,
          }),
        );

        if (query && query.length >= 1) {
          fileContext.requestWorkspaceFiles(query);
          const lowerQuery = query.toLowerCase();
          return allItems.filter(
            (item) =>
              item.label.toLowerCase().includes(lowerQuery) ||
              (item.description &&
                item.description.toLowerCase().includes(lowerQuery)),
          );
        }

        return allItems;
      } else {
        const commands: CompletionItem[] = [
          {
            id: 'login',
            label: '/login',
            description: 'Login to Qwen Code',
            type: 'command',
            icon: <UserIcon />,
          },
        ];

        return commands.filter((cmd) =>
          cmd.label.toLowerCase().includes(query.toLowerCase()),
        );
      }
    },
    [fileContext],
  );

  const completion = useCompletionTrigger(inputFieldRef, getCompletionItems);

  // Message submission
  const { handleSubmit } = useMessageSubmit({
    vscode,
    inputText,
    setInputText,
    inputFieldRef,
    isStreaming: messageHandling.isStreaming,
    fileContext,
    messageHandling,
  });

  // WebView messages
  useWebViewMessages({
    sessionManagement,
    fileContext,
    messageHandling,
    handleToolCallUpdate,
    clearToolCalls,
    setPlanEntries,
    handlePermissionRequest: React.useCallback(
      (request: {
        options: PermissionOption[];
        toolCall: PermissionToolCall;
      }) => {
        setPermissionRequest(request);
      },
      [],
    ),
    inputFieldRef,
    setInputText,
  });

  // Permission handling
  const handlePermissionResponse = React.useCallback(
    (optionId: string) => {
      vscode.postMessage({
        type: 'permissionResponse',
        data: { optionId },
      });
      setPermissionRequest(null);
    },
    [vscode],
  );

  // Completion selection
  const handleCompletionSelect = React.useCallback(
    (item: CompletionItem) => {
      if (!inputFieldRef.current) {
        return;
      }

      const inputElement = inputFieldRef.current;
      const currentText = inputElement.textContent || '';

      if (item.type === 'command') {
        if (item.label === '/login') {
          inputElement.textContent = '';
          setInputText('');
          completion.closeCompletion();
          vscode.postMessage({
            type: 'login',
            data: {},
          });
          return;
        }

        inputElement.textContent = item.label + ' ';
        setInputText(item.label + ' ');

        setTimeout(() => {
          const range = document.createRange();
          const sel = window.getSelection();
          if (inputElement.firstChild) {
            range.setStart(inputElement.firstChild, (item.label + ' ').length);
            range.collapse(true);
          } else {
            range.selectNodeContents(inputElement);
            range.collapse(false);
          }
          sel?.removeAllRanges();
          sel?.addRange(range);
          inputElement.focus();
        }, 10);
      } else if (item.type === 'file') {
        const filePath = (item.value as string) || item.label;
        fileContext.addFileReference(item.label, filePath);

        const atPos = currentText.lastIndexOf('@');

        if (atPos !== -1) {
          const textAfterAt = currentText.substring(atPos + 1);
          const spaceIndex = textAfterAt.search(/[\s\n]/);
          const queryEnd =
            spaceIndex === -1 ? currentText.length : atPos + 1 + spaceIndex;

          const textBefore = currentText.substring(0, atPos);
          const textAfter = currentText.substring(queryEnd);
          const newText = `${textBefore}@${item.label} ${textAfter}`;

          inputElement.textContent = newText;
          setInputText(newText);

          const newCursorPos = atPos + item.label.length + 2;

          setTimeout(() => {
            const textNode = inputElement.firstChild;
            if (textNode && textNode.nodeType === Node.TEXT_NODE) {
              const selection = window.getSelection();
              if (selection) {
                const range = document.createRange();
                try {
                  range.setStart(
                    textNode,
                    Math.min(newCursorPos, newText.length),
                  );
                  range.collapse(true);
                  selection.removeAllRanges();
                  selection.addRange(range);
                } catch (e) {
                  console.error('[handleCompletionSelect] Error:', e);
                  range.selectNodeContents(inputElement);
                  range.collapse(false);
                  selection.removeAllRanges();
                  selection.addRange(range);
                }
              }
            }
            inputElement.focus();
          }, 10);
        }
      }

      completion.closeCompletion();
    },
    [completion, vscode, fileContext],
  );

  // Attach context (Cmd/Ctrl + /)
  const handleAttachContextClick = React.useCallback(async () => {
    if (inputFieldRef.current) {
      inputFieldRef.current.focus();

      const currentText = inputFieldRef.current.textContent || '';
      const newText = currentText ? `${currentText} @` : '@';
      inputFieldRef.current.textContent = newText;
      setInputText(newText);

      const range = document.createRange();
      const sel = window.getSelection();
      range.selectNodeContents(inputFieldRef.current);
      range.collapse(false);
      sel?.removeAllRanges();
      sel?.addRange(range);

      requestAnimationFrame(async () => {
        if (!inputFieldRef.current) {
          return;
        }

        let position = { top: 0, left: 0 };
        const selection = window.getSelection();

        if (selection && selection.rangeCount > 0) {
          try {
            const currentRange = selection.getRangeAt(0);
            const rangeRect = currentRange.getBoundingClientRect();
            if (rangeRect.top > 0 && rangeRect.left > 0) {
              position = {
                top: rangeRect.top,
                left: rangeRect.left,
              };
            } else {
              const inputRect = inputFieldRef.current.getBoundingClientRect();
              position = { top: inputRect.top, left: inputRect.left };
            }
          } catch (error) {
            console.error('[App] Error getting cursor position:', error);
            const inputRect = inputFieldRef.current.getBoundingClientRect();
            position = { top: inputRect.top, left: inputRect.left };
          }
        } else {
          const inputRect = inputFieldRef.current.getBoundingClientRect();
          position = { top: inputRect.top, left: inputRect.left };
        }

        await completion.openCompletion('@', '', position);
      });
    }
  }, [completion]);

  // Keyboard shortcut for attach context
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === '/') {
        e.preventDefault();
        handleAttachContextClick();
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [handleAttachContextClick]);

  // Auto-scroll to latest message
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messageHandling.messages, messageHandling.currentStreamContent]);

  // Load sessions on mount
  useEffect(() => {
    vscode.postMessage({ type: 'getQwenSessions', data: {} });
  }, [vscode]);

  // Request active editor on mount
  useEffect(() => {
    fileContext.requestActiveEditor();
  }, [fileContext]);

  // Toggle edit mode
  const handleToggleEditMode = () => {
    setEditMode((prev) => {
      if (prev === 'ask') {
        return 'auto';
      }
      if (prev === 'auto') {
        return 'plan';
      }
      return 'ask';
    });
  };

  const handleToggleThinking = () => {
    setThinkingEnabled((prev) => !prev);
  };

  const hasContent =
    messageHandling.messages.length > 0 ||
    messageHandling.isStreaming ||
    inProgressToolCalls.length > 0 ||
    completedToolCalls.length > 0 ||
    planEntries.length > 0;

  return (
    <div className="chat-container">
      <SessionSelector
        visible={sessionManagement.showSessionSelector}
        sessions={sessionManagement.filteredSessions}
        currentSessionId={sessionManagement.currentSessionId}
        searchQuery={sessionManagement.sessionSearchQuery}
        onSearchChange={sessionManagement.setSessionSearchQuery}
        onSelectSession={(sessionId) => {
          sessionManagement.handleSwitchSession(sessionId);
          sessionManagement.setSessionSearchQuery('');
        }}
        onClose={() => sessionManagement.setShowSessionSelector(false)}
      />

      <ChatHeader
        currentSessionTitle={sessionManagement.currentSessionTitle}
        onLoadSessions={sessionManagement.handleLoadQwenSessions}
        onSaveSession={() => setShowSaveDialog(true)}
        onNewSession={sessionManagement.handleNewQwenSession}
      />

      <div
        className="flex-1 overflow-y-auto overflow-x-hidden pt-5 pr-5 pl-5 pb-[120px] flex flex-col relative min-w-0 focus:outline-none [&::-webkit-scrollbar]:w-2 [&::-webkit-scrollbar-track]:bg-transparent [&::-webkit-scrollbar-thumb]:bg-white/20 [&::-webkit-scrollbar-thumb]:rounded-sm [&::-webkit-scrollbar-thumb:hover]:bg-white/30 [&>*]:flex [&>*]:gap-0 [&>*]:items-start [&>*]:text-left [&>*]:py-2 [&>*]:px-0 [&>*]:flex-col [&>*]:relative [&>*]:animate-[fadeIn_0.2s_ease-in]"
        style={{ backgroundColor: 'var(--app-primary-background)' }}
      >
        {!hasContent ? (
          <EmptyState />
        ) : (
          <>
            {messageHandling.messages.map((msg, index) => {
              const handleFileClick = (path: string) => {
                vscode.postMessage({
                  type: 'openFile',
                  data: { path },
                });
              };

              if (msg.role === 'thinking') {
                return (
                  <ThinkingMessage
                    key={index}
                    content={msg.content}
                    timestamp={msg.timestamp}
                    onFileClick={handleFileClick}
                  />
                );
              }

              if (msg.role === 'user') {
                return (
                  <UserMessage
                    key={index}
                    content={msg.content}
                    timestamp={msg.timestamp}
                    onFileClick={handleFileClick}
                    fileContext={msg.fileContext}
                  />
                );
              }

              return (
                <AssistantMessage
                  key={index}
                  content={msg.content}
                  timestamp={msg.timestamp}
                  onFileClick={handleFileClick}
                />
              );
            })}

            {inProgressToolCalls.map((toolCall) => (
              <InProgressToolCall
                key={toolCall.toolCallId}
                toolCall={toolCall}
              />
            ))}

            {completedToolCalls.filter(hasToolCallOutput).map((toolCall) => (
              <ToolCall key={toolCall.toolCallId} toolCall={toolCall} />
            ))}

            {planEntries.length > 0 && <PlanDisplay entries={planEntries} />}

            {messageHandling.isWaitingForResponse &&
              messageHandling.loadingMessage && (
                <WaitingMessage
                  loadingMessage={messageHandling.loadingMessage}
                />
              )}

            {messageHandling.isStreaming &&
              messageHandling.currentStreamContent && (
                <StreamingMessage
                  content={messageHandling.currentStreamContent}
                  onFileClick={(path) => {
                    vscode.postMessage({
                      type: 'openFile',
                      data: { path },
                    });
                  }}
                />
              )}

            <div ref={messagesEndRef} />
          </>
        )}
      </div>

      <InfoBanner
        visible={showBanner}
        onDismiss={() => setShowBanner(false)}
        onLinkClick={(e) => {
          e.preventDefault();
          vscode.postMessage({ type: 'openSettings', data: {} });
        }}
      />

      <InputForm
        inputText={inputText}
        inputFieldRef={inputFieldRef}
        isStreaming={messageHandling.isStreaming}
        isComposing={isComposing}
        editMode={editMode}
        thinkingEnabled={thinkingEnabled}
        activeFileName={fileContext.activeFileName}
        activeSelection={fileContext.activeSelection}
        onInputChange={setInputText}
        onCompositionStart={() => setIsComposing(true)}
        onCompositionEnd={() => setIsComposing(false)}
        onKeyDown={() => {}}
        onSubmit={handleSubmit}
        onToggleEditMode={handleToggleEditMode}
        onToggleThinking={handleToggleThinking}
        onFocusActiveEditor={fileContext.focusActiveEditor}
        onShowCommandMenu={async () => {
          if (inputFieldRef.current) {
            inputFieldRef.current.focus();

            const selection = window.getSelection();
            let position = { top: 0, left: 0 };

            if (selection && selection.rangeCount > 0) {
              try {
                const range = selection.getRangeAt(0);
                const rangeRect = range.getBoundingClientRect();
                if (rangeRect.top > 0 && rangeRect.left > 0) {
                  position = {
                    top: rangeRect.top,
                    left: rangeRect.left,
                  };
                } else {
                  const inputRect =
                    inputFieldRef.current.getBoundingClientRect();
                  position = { top: inputRect.top, left: inputRect.left };
                }
              } catch (error) {
                console.error('[App] Error getting cursor position:', error);
                const inputRect = inputFieldRef.current.getBoundingClientRect();
                position = { top: inputRect.top, left: inputRect.left };
              }
            } else {
              const inputRect = inputFieldRef.current.getBoundingClientRect();
              position = { top: inputRect.top, left: inputRect.left };
            }

            await completion.openCompletion('/', '', position);
          }
        }}
        onAttachContext={handleAttachContextClick}
        completionIsOpen={completion.isOpen}
      />

      <SaveSessionDialog
        isOpen={showSaveDialog}
        onClose={() => setShowSaveDialog(false)}
        onSave={sessionManagement.handleSaveSession}
        existingTags={sessionManagement.savedSessionTags}
      />

      {permissionRequest && (
        <PermissionDrawer
          isOpen={!!permissionRequest}
          options={permissionRequest.options}
          toolCall={permissionRequest.toolCall}
          onResponse={handlePermissionResponse}
          onClose={() => setPermissionRequest(null)}
        />
      )}

      {completion.isOpen && completion.items.length > 0 && (
        <CompletionMenu
          items={completion.items}
          position={completion.position}
          onSelect={handleCompletionSelect}
          onClose={completion.closeCompletion}
        />
      )}
    </div>
  );
};
