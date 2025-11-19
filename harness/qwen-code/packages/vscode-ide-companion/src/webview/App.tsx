/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import React, { useState, useEffect, useRef } from 'react';
import { useVSCode } from './hooks/useVSCode.js';
import type { Conversation } from '../storage/conversationStore.js';
import {
  PermissionRequest,
  type PermissionOption,
  type ToolCall as PermissionToolCall,
} from './components/PermissionRequest.js';
import { ToolCall, type ToolCallData } from './components/ToolCall.js';
import { EmptyState } from './components/EmptyState.js';

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

// Loading messages from Claude Code CLI
// Source: packages/cli/src/ui/hooks/usePhraseCycler.ts
const WITTY_LOADING_PHRASES = [
  "I'm Feeling Lucky",
  'Shipping awesomeness... ',
  'Painting the serifs back on...',
  'Navigating the slime mold...',
  'Consulting the digital spirits...',
  'Reticulating splines...',
  'Warming up the AI hamsters...',
  'Asking the magic conch shell...',
  'Generating witty retort...',
  'Polishing the algorithms...',
  "Don't rush perfection (or my code)...",
  'Brewing fresh bytes...',
  'Counting electrons...',
  'Engaging cognitive processors...',
  'Checking for syntax errors in the universe...',
  'One moment, optimizing humor...',
  'Shuffling punchlines...',
  'Untangling neural nets...',
  'Compiling brilliance...',
  'Loading wit.exe...',
  'Summoning the cloud of wisdom...',
  'Preparing a witty response...',
  "Just a sec, I'm debugging reality...",
  'Confuzzling the options...',
  'Tuning the cosmic frequencies...',
  'Crafting a response worthy of your patience...',
  'Compiling the 1s and 0s...',
  'Resolving dependencies... and existential crises...',
  'Defragmenting memories... both RAM and personal...',
  'Rebooting the humor module...',
  'Caching the essentials (mostly cat memes)...',
  'Optimizing for ludicrous speed',
  "Swapping bits... don't tell the bytes...",
  'Garbage collecting... be right back...',
  'Assembling the interwebs...',
  'Converting coffee into code...',
  'Updating the syntax for reality...',
  'Rewiring the synapses...',
  'Looking for a misplaced semicolon...',
  "Greasin' the cogs of the machine...",
  'Pre-heating the servers...',
  'Calibrating the flux capacitor...',
  'Engaging the improbability drive...',
  'Channeling the Force...',
  'Aligning the stars for optimal response...',
  'So say we all...',
  'Loading the next great idea...',
  "Just a moment, I'm in the zone...",
  'Preparing to dazzle you with brilliance...',
  "Just a tick, I'm polishing my wit...",
  "Hold tight, I'm crafting a masterpiece...",
  "Just a jiffy, I'm debugging the universe...",
  "Just a moment, I'm aligning the pixels...",
  "Just a sec, I'm optimizing the humor...",
  "Just a moment, I'm tuning the algorithms...",
  'Warp speed engaged...',
  'Mining for more Dilithium crystals...',
  "Don't panic...",
  'Following the white rabbit...',
  'The truth is in here... somewhere...',
  'Blowing on the cartridge...',
  'Loading... Do a barrel roll!',
  'Waiting for the respawn...',
  'Finishing the Kessel Run in less than 12 parsecs...',
  "The cake is not a lie, it's just still loading...",
  'Fiddling with the character creation screen...',
  "Just a moment, I'm finding the right meme...",
  "Pressing 'A' to continue...",
  'Herding digital cats...',
  'Polishing the pixels...',
  'Finding a suitable loading screen pun...',
  'Distracting you with this witty phrase...',
  'Almost there... probably...',
  'Our hamsters are working as fast as they can...',
  'Giving Cloudy a pat on the head...',
  'Petting the cat...',
  'Rickrolling my boss...',
  'Never gonna give you up, never gonna let you down...',
  'Slapping the bass...',
  'Tasting the snozberries...',
  "I'm going the distance, I'm going for speed...",
  'Is this the real life? Is this just fantasy?...',
  "I've got a good feeling about this...",
  'Poking the bear...',
  'Doing research on the latest memes...',
  'Figuring out how to make this more witty...',
  'Hmmm... let me think...',
  'What do you call a fish with no eyes? A fsh...',
  'Why did the computer go to therapy? It had too many bytes...',
  "Why don't programmers like nature? It has too many bugs...",
  'Why do programmers prefer dark mode? Because light attracts bugs...',
  'Why did the developer go broke? Because they used up all their cache...',
  "What can you do with a broken pencil? Nothing, it's pointless...",
  'Applying percussive maintenance...',
  'Searching for the correct USB orientation...',
  'Ensuring the magic smoke stays inside the wires...',
  'Rewriting in Rust for no particular reason...',
  'Trying to exit Vim...',
  'Spinning up the hamster wheel...',
  "That's not a bug, it's an undocumented feature...",
  'Engage.',
  "I'll be back... with an answer.",
  'My other process is a TARDIS...',
  'Communing with the machine spirit...',
  'Letting the thoughts marinate...',
  'Just remembered where I put my keys...',
  'Pondering the orb...',
  "I've seen things you people wouldn't believe... like a user who reads loading messages.",
  'Initiating thoughtful gaze...',
  "What's a computer's favorite snack? Microchips.",
  "Why do Java developers wear glasses? Because they don't C#.",
  'Charging the laser... pew pew!',
  'Dividing by zero... just kidding!',
  'Looking for an adult superviso... I mean, processing.',
  'Making it go beep boop.',
  'Buffering... because even AIs need a moment.',
  'Entangling quantum particles for a faster response...',
  'Polishing the chrome... on the algorithms.',
  'Are you not entertained? (Working on it!)',
  'Summoning the code gremlins... to help, of course.',
  'Just waiting for the dial-up tone to finish...',
  'Recalibrating the humor-o-meter.',
  'My other loading screen is even funnier.',
  "Pretty sure there's a cat walking on the keyboard somewhere...",
  'Enhancing... Enhancing... Still loading.',
  "It's not a bug, it's a feature... of this loading screen.",
  'Have you tried turning it off and on again? (The loading screen, not me.)',
  'Constructing additional pylons...',
  "New line? That's Ctrl+J.",
];

const getRandomLoadingMessage = () =>
  WITTY_LOADING_PHRASES[
    Math.floor(Math.random() * WITTY_LOADING_PHRASES.length)
  ];

export const App: React.FC = () => {
  const vscode = useVSCode();
  const [messages, setMessages] = useState<TextMessage[]>([]);
  const [inputText, setInputText] = useState('');
  const [isStreaming, setIsStreaming] = useState(false);
  const [_isWaitingForResponse, setIsWaitingForResponse] = useState(false);
  const [_loadingMessage, setLoadingMessage] = useState('');
  const [currentStreamContent, setCurrentStreamContent] = useState('');
  const [qwenSessions, setQwenSessions] = useState<
    Array<Record<string, unknown>>
  >([]);
  const [currentSessionId, setCurrentSessionId] = useState<string | null>(null);
  const [showSessionSelector, setShowSessionSelector] = useState(false);
  const [sessionSearchQuery, setSessionSearchQuery] = useState('');
  const [permissionRequest, setPermissionRequest] = useState<{
    options: PermissionOption[];
    toolCall: PermissionToolCall;
  } | null>(null);
  const [toolCalls, setToolCalls] = useState<Map<string, ToolCallData>>(
    new Map(),
  );
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputFieldRef = useRef<HTMLDivElement>(null);
  const [showBanner, setShowBanner] = useState(true);
  const currentStreamContentRef = useRef<string>('');

  const handlePermissionRequest = React.useCallback(
    (request: {
      options: PermissionOption[];
      toolCall: PermissionToolCall;
    }) => {
      setPermissionRequest(request);
    },
    [],
  );

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

  const handleToolCallUpdate = React.useCallback((update: ToolCallUpdate) => {
    setToolCalls((prev) => {
      const newMap = new Map(prev);
      const existing = newMap.get(update.toolCallId);

      // Helper function to safely convert title to string
      const safeTitle = (title: unknown): string => {
        if (typeof title === 'string') {
          return title;
        }
        if (title && typeof title === 'object') {
          return JSON.stringify(title);
        }
        return 'Tool Call';
      };

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
          title: safeTitle(update.title),
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
          ...(update.title && { title: safeTitle(update.title) }),
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
          currentStreamContentRef.current = '';
          break;

        case 'streamChunk': {
          const chunkData = message.data;
          if (chunkData.role === 'thinking') {
            // Handle thinking chunks separately if needed
            setCurrentStreamContent((prev) => {
              const newContent = prev + chunkData.chunk;
              currentStreamContentRef.current = newContent;
              return newContent;
            });
          } else {
            setCurrentStreamContent((prev) => {
              const newContent = prev + chunkData.chunk;
              currentStreamContentRef.current = newContent;
              return newContent;
            });
          }
          break;
        }

        case 'streamEnd':
          // Finalize the streamed message
          if (currentStreamContentRef.current) {
            const assistantMessage: TextMessage = {
              role: 'assistant',
              content: currentStreamContentRef.current,
              timestamp: Date.now(),
            };
            setMessages((prev) => [...prev, assistantMessage]);
          }
          setIsStreaming(false);
          setCurrentStreamContent('');
          currentStreamContentRef.current = '';
          break;

        case 'error':
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

        case 'qwenSessionList': {
          const sessions = message.data.sessions || [];
          setQwenSessions(sessions);
          // If no current session is selected and there are sessions, select the first one
          if (!currentSessionId && sessions.length > 0) {
            const firstSessionId =
              (sessions[0].id as string) || (sessions[0].sessionId as string);
            if (firstSessionId) {
              setCurrentSessionId(firstSessionId);
            }
          }
          break;
        }

        case 'qwenSessionSwitched':
          console.log('[App] Session switched:', message.data);
          setShowSessionSelector(false);
          // Update current session ID
          if (message.data.sessionId) {
            setCurrentSessionId(message.data.sessionId as string);
            console.log(
              '[App] Current session ID updated to:',
              message.data.sessionId,
            );
          }
          // Load messages from the session
          if (message.data.messages) {
            console.log(
              '[App] Loading messages:',
              message.data.messages.length,
            );
            setMessages(message.data.messages);
          } else {
            console.log('[App] No messages in session, clearing');
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
  }, [currentSessionId, handlePermissionRequest, handleToolCallUpdate]);

  useEffect(() => {
    // Auto-scroll to bottom when messages change
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, currentStreamContent]);

  // Load sessions on component mount
  useEffect(() => {
    vscode.postMessage({ type: 'getQwenSessions', data: {} });
  }, [vscode]);

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();

    if (!inputText.trim() || isStreaming) {
      return;
    }

    // Set waiting state with random loading message
    setIsWaitingForResponse(true);
    setLoadingMessage(getRandomLoadingMessage());

    vscode.postMessage({
      type: 'sendMessage',
      data: { text: inputText },
    });

    // Clear input field
    setInputText('');
    if (inputFieldRef.current) {
      inputFieldRef.current.textContent = '';
    }
  };

  const handleLoadQwenSessions = () => {
    vscode.postMessage({ type: 'getQwenSessions', data: {} });
    setShowSessionSelector(true);
  };

  const handleNewQwenSession = () => {
    vscode.postMessage({ type: 'newQwenSession', data: {} });
    setShowSessionSelector(false);
    setCurrentSessionId(null);
    // Clear messages in UI
    setMessages([]);
    setCurrentStreamContent('');
  };

  // Time ago formatter (matching Claude Code)
  const getTimeAgo = (timestamp: string): string => {
    if (!timestamp) {
      return '';
    }
    const now = new Date().getTime();
    const then = new Date(timestamp).getTime();
    const diffMs = now - then;
    const diffMins = Math.floor(diffMs / 60000);
    const diffHours = Math.floor(diffMs / 3600000);
    const diffDays = Math.floor(diffMs / 86400000);

    if (diffMins < 1) {
      return 'now';
    }
    if (diffMins < 60) {
      return `${diffMins}m`;
    }
    if (diffHours < 24) {
      return `${diffHours}h`;
    }
    if (diffDays === 1) {
      return 'Yesterday';
    }
    if (diffDays < 7) {
      return `${diffDays}d`;
    }
    return new Date(timestamp).toLocaleDateString();
  };

  // Group sessions by date (matching Claude Code)
  const groupSessionsByDate = (
    sessions: Array<Record<string, unknown>>,
  ): Array<{ label: string; sessions: Array<Record<string, unknown>> }> => {
    const now = new Date();
    const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
    const yesterday = new Date(today);
    yesterday.setDate(yesterday.getDate() - 1);

    const groups: {
      [key: string]: Array<Record<string, unknown>>;
    } = {
      Today: [],
      Yesterday: [],
      'This Week': [],
      Older: [],
    };

    sessions.forEach((session) => {
      const timestamp =
        (session.lastUpdated as string) || (session.startTime as string) || '';
      if (!timestamp) {
        groups['Older'].push(session);
        return;
      }

      const sessionDate = new Date(timestamp);
      const sessionDay = new Date(
        sessionDate.getFullYear(),
        sessionDate.getMonth(),
        sessionDate.getDate(),
      );

      if (sessionDay.getTime() === today.getTime()) {
        groups['Today'].push(session);
      } else if (sessionDay.getTime() === yesterday.getTime()) {
        groups['Yesterday'].push(session);
      } else if (sessionDay.getTime() > today.getTime() - 7 * 86400000) {
        groups['This Week'].push(session);
      } else {
        groups['Older'].push(session);
      }
    });

    return Object.entries(groups)
      .filter(([, sessions]) => sessions.length > 0)
      .map(([label, sessions]) => ({ label, sessions }));
  };

  // Filter sessions by search query
  const filteredSessions = React.useMemo(() => {
    if (!sessionSearchQuery.trim()) {
      return qwenSessions;
    }
    const query = sessionSearchQuery.toLowerCase();
    return qwenSessions.filter((session) => {
      const title = (
        (session.title as string) ||
        (session.name as string) ||
        ''
      ).toLowerCase();
      return title.includes(query);
    });
  }, [qwenSessions, sessionSearchQuery]);

  const handleSwitchSession = (sessionId: string) => {
    if (sessionId === currentSessionId) {
      console.log('[App] Already on this session, ignoring');
      setShowSessionSelector(false);
      return;
    }

    console.log('[App] Switching to session:', sessionId);
    vscode.postMessage({
      type: 'switchQwenSession',
      data: { sessionId },
    });
    // Don't set currentSessionId or close selector here - wait for qwenSessionSwitched response
  };

  // Check if there are any messages or active content
  const hasContent =
    messages.length > 0 ||
    isStreaming ||
    toolCalls.size > 0 ||
    permissionRequest !== null;

  return (
    <div className="chat-container">
      {showSessionSelector && (
        <>
          <div
            className="session-selector-backdrop"
            onClick={() => setShowSessionSelector(false)}
          />
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
              <svg
                xmlns="http://www.w3.org/2000/svg"
                viewBox="0 0 20 20"
                fill="currentColor"
                aria-hidden="true"
                data-slot="icon"
                className="session-search-icon"
              >
                <path
                  fillRule="evenodd"
                  d="M9 3.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11ZM2 9a7 7 0 1 1 12.452 4.391l3.328 3.329a.75.75 0 1 1-1.06 1.06l-3.329-3.328A7 7 0 0 1 2 9Z"
                  clipRule="evenodd"
                ></path>
              </svg>
              <input
                type="text"
                className="session-search-input"
                placeholder="Search sessions…"
                value={sessionSearchQuery}
                onChange={(e) => setSessionSearchQuery(e.target.value)}
              />
            </div>

            {/* Session List with Grouping */}
            <div className="session-list-content">
              {filteredSessions.length === 0 ? (
                <div
                  style={{
                    padding: '20px',
                    textAlign: 'center',
                    color: 'var(--app-secondary-foreground)',
                  }}
                >
                  {sessionSearchQuery
                    ? 'No matching sessions'
                    : 'No sessions available'}
                </div>
              ) : (
                groupSessionsByDate(filteredSessions).map((group) => (
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
                              handleSwitchSession(sessionId);
                              setShowSessionSelector(false);
                              setSessionSearchQuery('');
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
      )}

      <div className="chat-header">
        <button
          className="header-conversations-button"
          onClick={handleLoadQwenSessions}
          title="Past conversations"
        >
          <span className="button-content">
            <span className="button-text">Past Conversations</span>
            <svg
              xmlns="http://www.w3.org/2000/svg"
              viewBox="0 0 20 20"
              fill="currentColor"
              aria-hidden="true"
              className="dropdown-icon"
            >
              <path
                fillRule="evenodd"
                d="M5.22 8.22a.75.75 0 0 1 1.06 0L10 11.94l3.72-3.72a.75.75 0 1 1 1.06 1.06l-4.25 4.25a.75.75 0 0 1-1.06 0L5.22 9.28a.75.75 0 0 1 0-1.06Z"
                clipRule="evenodd"
              ></path>
            </svg>
          </span>
        </button>
        <div className="header-spacer"></div>
        <button
          className="new-session-header-button"
          onClick={handleNewQwenSession}
          title="New Session"
        >
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 20 20"
            fill="currentColor"
            aria-hidden="true"
            data-slot="icon"
            className="icon-svg"
          >
            <path d="M10.75 4.75a.75.75 0 0 0-1.5 0v4.5h-4.5a.75.75 0 0 0 0 1.5h4.5v4.5a.75.75 0 0 0 1.5 0v-4.5h4.5a.75.75 0 0 0 0-1.5h-4.5v-4.5Z"></path>
          </svg>
        </button>
      </div>

      <div className="messages-container">
        {!hasContent ? (
          <EmptyState />
        ) : (
          <>
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
          </>
        )}
      </div>

      {/* Info Banner */}
      {showBanner && (
        <div className="info-banner">
          <div className="banner-content">
            <svg
              className="banner-icon"
              width="16"
              height="16"
              viewBox="0 0 20 20"
              xmlns="http://www.w3.org/2000/svg"
            >
              <path d="M5.14648 7.14648C5.34175 6.95122 5.65825 6.95122 5.85352 7.14648L8.35352 9.64648C8.44728 9.74025 8.5 9.86739 8.5 10C8.5 10.0994 8.47037 10.1958 8.41602 10.2773L8.35352 10.3535L5.85352 12.8535C5.65825 13.0488 5.34175 13.0488 5.14648 12.8535C4.95122 12.6583 4.95122 12.3417 5.14648 12.1465L7.29297 10L5.14648 7.85352C4.95122 7.65825 4.95122 7.34175 5.14648 7.14648Z"></path>
              <path d="M14.5 12C14.7761 12 15 12.2239 15 12.5C15 12.7761 14.7761 13 14.5 13H9.5C9.22386 13 9 12.7761 9 12.5C9 12.2239 9.22386 12 9.5 12H14.5Z"></path>
              <path
                fillRule="evenodd"
                clipRule="evenodd"
                d="M16.5 4C17.3284 4 18 4.67157 18 5.5V14.5C18 15.3284 17.3284 16 16.5 16H3.5C2.67157 16 2 15.3284 2 14.5V5.5C2 4.67157 2.67157 4 3.5 4H16.5ZM3.5 5C3.22386 5 3 5.22386 3 5.5V14.5C3 14.7761 3.22386 15 3.5 15H16.5C16.7761 15 17 14.7761 17 14.5V5.5C17 5.22386 16.7761 5 16.5 5H3.5Z"
              ></path>
            </svg>
            <label>
              Prefer the Terminal experience?{' '}
              <a href="#" className="banner-link">
                Switch back in Settings.
              </a>
            </label>
          </div>
          <button
            className="banner-close"
            aria-label="Close banner"
            onClick={() => setShowBanner(false)}
          >
            <svg
              width="10"
              height="10"
              viewBox="0 0 14 14"
              fill="none"
              xmlns="http://www.w3.org/2000/svg"
            >
              <path
                d="M1 1L13 13M1 13L13 1"
                stroke="currentColor"
                strokeWidth="1.5"
                strokeLinecap="round"
              ></path>
            </svg>
          </button>
        </div>
      )}

      <div className="input-form-container">
        <div className="input-form-wrapper">
          <form className="input-form" onSubmit={handleSubmit}>
            <div className="input-banner"></div>
            <div className="input-wrapper">
              <div
                ref={inputFieldRef}
                contentEditable="plaintext-only"
                className="input-field-editable"
                role="textbox"
                aria-label="Message input"
                aria-multiline="true"
                data-placeholder="Ask qwen to edit…"
                onInput={(e) => {
                  const target = e.target as HTMLDivElement;
                  setInputText(target.textContent || '');
                }}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && !e.shiftKey) {
                    e.preventDefault();
                    handleSubmit(e);
                  }
                }}
                suppressContentEditableWarning
              />
            </div>
            <div className="input-actions">
              <button
                type="button"
                className="action-button edit-mode-button"
                title="Qwen will ask before each edit. Click to switch modes."
              >
                <svg
                  xmlns="http://www.w3.org/2000/svg"
                  viewBox="0 0 16 16"
                  fill="currentColor"
                  aria-hidden="true"
                >
                  <path
                    fillRule="evenodd"
                    d="M11.013 2.513a1.75 1.75 0 0 1 2.475 2.474L6.226 12.25a2.751 2.751 0 0 1-.892.596l-2.047.848a.75.75 0 0 1-.98-.98l.848-2.047a2.75 2.75 0 0 1 .596-.892l7.262-7.261Z"
                    clipRule="evenodd"
                  ></path>
                </svg>
                <span>Ask before edits</span>
              </button>
              <div className="action-divider"></div>
              <button
                type="button"
                className="action-icon-button thinking-button"
                title="Thinking off"
              >
                <svg
                  width="16"
                  height="16"
                  viewBox="0 0 16 16"
                  fill="none"
                  xmlns="http://www.w3.org/2000/svg"
                >
                  <path
                    d="M8.00293 1.11523L8.35059 1.12402H8.35352C11.9915 1.30834 14.8848 4.31624 14.8848 8C14.8848 11.8025 11.8025 14.8848 8 14.8848C4.19752 14.8848 1.11523 11.8025 1.11523 8C1.11523 7.67691 1.37711 7.41504 1.7002 7.41504C2.02319 7.41514 2.28516 7.67698 2.28516 8C2.28516 11.1563 4.84369 13.7148 8 13.7148C11.1563 13.7148 13.7148 11.1563 13.7148 8C13.7148 4.94263 11.3141 2.4464 8.29492 2.29297V2.29199L7.99609 2.28516H7.9873V2.28418L7.89648 2.27539L7.88281 2.27441V2.27344C7.61596 2.21897 7.41513 1.98293 7.41504 1.7002C7.41504 1.37711 7.67691 1.11523 8 1.11523H8.00293ZM8 3.81543C8.32309 3.81543 8.58496 4.0773 8.58496 4.40039V7.6377L10.9619 8.82715C11.2505 8.97169 11.3678 9.32256 11.2236 9.61133C11.0972 9.86425 10.8117 9.98544 10.5488 9.91504L10.5352 9.91211V9.91016L10.4502 9.87891L10.4385 9.87402V9.87305L7.73828 8.52344C7.54007 8.42433 7.41504 8.22155 7.41504 8V4.40039C7.41504 4.0773 7.67691 3.81543 8 3.81543ZM2.44336 5.12695C2.77573 5.19517 3.02597 5.48929 3.02637 5.8418C3.02637 6.19456 2.7761 6.49022 2.44336 6.55859L2.2959 6.57324C1.89241 6.57324 1.56543 6.24529 1.56543 5.8418C1.56588 5.43853 1.89284 5.1123 2.2959 5.1123L2.44336 5.12695ZM3.46094 2.72949C3.86418 2.72984 4.19017 3.05712 4.19043 3.45996V3.46094C4.19009 3.86393 3.86392 4.19008 3.46094 4.19043H3.45996C3.05712 4.19017 2.72983 3.86419 2.72949 3.46094V3.45996C2.72976 3.05686 3.05686 2.72976 3.45996 2.72949H3.46094ZM5.98926 1.58008C6.32235 1.64818 6.57324 1.94276 6.57324 2.2959L6.55859 2.44336C6.49022 2.7761 6.19456 3.02637 5.8418 3.02637C5.43884 3.02591 5.11251 2.69895 5.1123 2.2959L5.12695 2.14844C5.19504 1.81591 5.48906 1.56583 5.8418 1.56543L5.98926 1.58008Z"
                    strokeWidth="0.27"
                    style={{
                      stroke: 'var(--app-secondary-foreground)',
                      fill: 'var(--app-secondary-foreground)',
                    }}
                  ></path>
                </svg>
              </button>
              <button
                type="button"
                className="action-icon-button command-button"
                title="Show command menu (/)"
              >
                <svg
                  xmlns="http://www.w3.org/2000/svg"
                  viewBox="0 0 20 20"
                  fill="currentColor"
                  aria-hidden="true"
                >
                  <path
                    fillRule="evenodd"
                    d="M12.528 3.047a.75.75 0 0 1 .449.961L8.433 16.504a.75.75 0 1 1-1.41-.512l4.544-12.496a.75.75 0 0 1 .961-.449Z"
                    clipRule="evenodd"
                  ></path>
                </svg>
              </button>
              <button
                type="submit"
                className="send-button-icon"
                disabled={isStreaming || !inputText.trim()}
              >
                <svg
                  xmlns="http://www.w3.org/2000/svg"
                  viewBox="0 0 20 20"
                  fill="currentColor"
                  aria-hidden="true"
                >
                  <path
                    fillRule="evenodd"
                    d="M10 17a.75.75 0 0 1-.75-.75V5.612L5.29 9.77a.75.75 0 0 1-1.08-1.04l5.25-5.5a.75.75 0 0 1 1.08 0l5.25 5.5a.75.75 0 1 1-1.08 1.04l-3.96-4.158V16.25A.75.75 0 0 1 10 17Z"
                    clipRule="evenodd"
                  ></path>
                </svg>
              </button>
            </div>
          </form>
        </div>
      </div>
    </div>
  );
};
