/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import React, { useState, useEffect, useRef, useCallback } from 'react';
import { useVSCode } from './hooks/useVSCode.js';
import type { Conversation } from '../storage/conversationStore.js';
import {
  type PermissionOption,
  type ToolCall as PermissionToolCall,
} from './components/PermissionRequest.js';
import { PermissionDrawer } from './components/PermissionDrawer.js';
import { ToolCall, type ToolCallData } from './components/ToolCall.js';
import { hasToolCallOutput } from './components/toolcalls/shared/utils.js';
import { EmptyState } from './components/EmptyState.js';
import { PlanDisplay, type PlanEntry } from './components/PlanDisplay.js';
import { MessageContent } from './components/MessageContent.js';
import {
  CompletionMenu,
  type CompletionItem,
} from './components/CompletionMenu.js';
import { useCompletionTrigger } from './hooks/useCompletionTrigger.js';

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

type EditMode = 'ask' | 'auto' | 'plan';

export const App: React.FC = () => {
  const vscode = useVSCode();
  const [messages, setMessages] = useState<TextMessage[]>([]);
  const [inputText, setInputText] = useState('');
  const [isStreaming, setIsStreaming] = useState(false);
  const [isWaitingForResponse, setIsWaitingForResponse] = useState(false);
  const [loadingMessage, setLoadingMessage] = useState('');
  const [currentStreamContent, setCurrentStreamContent] = useState('');
  const [qwenSessions, setQwenSessions] = useState<
    Array<Record<string, unknown>>
  >([]);
  const [currentSessionId, setCurrentSessionId] = useState<string | null>(null);
  const [currentSessionTitle, setCurrentSessionTitle] =
    useState<string>('Past Conversations');
  const [showSessionSelector, setShowSessionSelector] = useState(false);
  const [sessionSearchQuery, setSessionSearchQuery] = useState('');
  const [permissionRequest, setPermissionRequest] = useState<{
    options: PermissionOption[];
    toolCall: PermissionToolCall;
  } | null>(null);
  const [toolCalls, setToolCalls] = useState<Map<string, ToolCallData>>(
    new Map(),
  );
  const [planEntries, setPlanEntries] = useState<PlanEntry[]>([]);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputFieldRef = useRef<HTMLDivElement>(null);
  const [showBanner, setShowBanner] = useState(true);
  const currentStreamContentRef = useRef<string>('');
  const [editMode, setEditMode] = useState<EditMode>('ask');
  const [thinkingEnabled, setThinkingEnabled] = useState(false);
  const [activeFileName, setActiveFileName] = useState<string | null>(null);
  const [isComposing, setIsComposing] = useState(false);

  // Workspace files cache
  const [workspaceFiles, setWorkspaceFiles] = useState<
    Array<{
      id: string;
      label: string;
      description: string;
      path: string;
    }>
  >([]);

  // File reference map: @filename -> full path
  const fileReferenceMap = useRef<Map<string, string>>(new Map());

  // Request workspace files on mount or when @ is first triggered
  const hasRequestedFilesRef = useRef(false);

  // Debounce timer for search requests
  const searchTimerRef = useRef<NodeJS.Timeout | null>(null);

  // Get completion items based on trigger character
  const getCompletionItems = useCallback(
    async (trigger: '@' | '/', query: string): Promise<CompletionItem[]> => {
      if (trigger === '@') {
        // Request workspace files on first @ trigger
        if (!hasRequestedFilesRef.current) {
          hasRequestedFilesRef.current = true;
          vscode.postMessage({
            type: 'getWorkspaceFiles',
            data: {},
          });
        }

        // Convert workspace files to completion items
        const fileIcon = (
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 16 16"
            fill="currentColor"
          >
            <path d="M9 2H4a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h8a2 2 0 0 0 2-2V7l-5-5zm3 7V3.5L10.5 2H10v3a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V2H4a1 1 0 0 0-1 1v8a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1zM6 3h3v2H6V3z" />
          </svg>
        );

        // Convert all files to items
        const allItems: CompletionItem[] = workspaceFiles.map((file) => ({
          id: file.id,
          label: file.label,
          description: file.description,
          type: 'file' as const,
          icon: fileIcon,
          value: file.path,
        }));

        // If query provided, filter locally AND request from backend (debounced)
        if (query && query.length >= 1) {
          // Clear previous search timer
          if (searchTimerRef.current) {
            clearTimeout(searchTimerRef.current);
          }

          // Debounce backend search request (300ms)
          searchTimerRef.current = setTimeout(() => {
            vscode.postMessage({
              type: 'getWorkspaceFiles',
              data: { query },
            });
          }, 300);

          // Filter locally for immediate feedback
          const lowerQuery = query.toLowerCase();
          const filtered = allItems.filter(
            (item) =>
              item.label.toLowerCase().includes(lowerQuery) ||
              (item.description &&
                item.description.toLowerCase().includes(lowerQuery)),
          );

          return filtered;
        }

        return allItems;
      } else {
        // Slash commands - only /login for now
        const commands: CompletionItem[] = [
          {
            id: 'login',
            label: '/login',
            description: 'Login to Qwen Code',
            type: 'command',
            icon: (
              <svg
                xmlns="http://www.w3.org/2000/svg"
                viewBox="0 0 16 16"
                fill="currentColor"
              >
                <path d="M8 8a3 3 0 1 0 0-6 3 3 0 0 0 0 6ZM12.735 14c.618 0 1.093-.561.872-1.139a6.002 6.002 0 0 0-11.215 0c-.22.578.254 1.139.872 1.139h9.47Z" />
              </svg>
            ),
          },
        ];

        return commands.filter((cmd) =>
          cmd.label.toLowerCase().includes(query.toLowerCase()),
        );
      }
    },
    [vscode, workspaceFiles],
  );

  // Use completion trigger hook
  const completion = useCompletionTrigger(inputFieldRef, getCompletionItems);

  // Don't auto-refresh completion menu when workspace files update
  // This was causing flickering. User can re-type to get fresh results.

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

  // Handle completion item selection
  const handleCompletionSelect = useCallback(
    (item: CompletionItem) => {
      if (!inputFieldRef.current) {
        return;
      }

      const inputElement = inputFieldRef.current;
      const currentText = inputElement.textContent || '';

      if (item.type === 'file') {
        // Store file reference mapping
        const filePath = (item.value as string) || item.label;
        fileReferenceMap.current.set(item.label, filePath);

        console.log('[handleCompletionSelect] Current text:', currentText);
        console.log('[handleCompletionSelect] Selected file:', item.label);

        // Find the @ position in current text
        const atPos = currentText.lastIndexOf('@');

        if (atPos !== -1) {
          // Find the end of the query (could be at cursor or at next space/end)
          const textAfterAt = currentText.substring(atPos + 1);
          const spaceIndex = textAfterAt.search(/[\s\n]/);
          const queryEnd =
            spaceIndex === -1 ? currentText.length : atPos + 1 + spaceIndex;

          // Replace from @ to end of query with @filename
          const textBefore = currentText.substring(0, atPos);
          const textAfter = currentText.substring(queryEnd);
          const newText = `${textBefore}@${item.label} ${textAfter}`;

          console.log('[handleCompletionSelect] New text:', newText);

          // Update the input
          inputElement.textContent = newText;
          setInputText(newText);

          // Set cursor after the inserted filename (after the space)
          const newCursorPos = atPos + item.label.length + 2; // +1 for @, +1 for space

          // Wait for DOM to update, then set cursor
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
                  console.error(
                    '[handleCompletionSelect] Error setting cursor:',
                    e,
                  );
                  // Fallback: move cursor to end
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
      } else if (item.type === 'command') {
        // Replace entire input with command
        inputElement.textContent = item.label + ' ';
        setInputText(item.label + ' ');

        // Move cursor to end
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
      }

      // Close completion
      completion.closeCompletion();
    },
    [completion],
  );

  // Handle attach context button click (Cmd/Ctrl + /)
  const handleAttachContextClick = useCallback(async () => {
    if (inputFieldRef.current) {
      // Focus the input first
      inputFieldRef.current.focus();

      // Insert @ at the end of current text
      const currentText = inputFieldRef.current.textContent || '';
      const newText = currentText ? `${currentText} @` : '@';
      inputFieldRef.current.textContent = newText;
      setInputText(newText);

      // Move cursor to end
      const range = document.createRange();
      const sel = window.getSelection();
      range.selectNodeContents(inputFieldRef.current);
      range.collapse(false);
      sel?.removeAllRanges();
      sel?.addRange(range);

      // Wait for DOM to update before getting position and opening menu
      requestAnimationFrame(async () => {
        if (!inputFieldRef.current) {
          return;
        }

        // Get cursor position for menu placement
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

        // Open completion menu with @ trigger
        await completion.openCompletion('@', '', position);
      });
    }
  }, [completion]);

  // Handle keyboard shortcut for attach context (Cmd/Ctrl + /)
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      // Cmd/Ctrl + / for attach context
      if ((e.metaKey || e.ctrlKey) && e.key === '/') {
        e.preventDefault();
        handleAttachContextClick();
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [handleAttachContextClick]);

  // Handle removing context attachment
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

        case 'thoughtChunk': {
          const chunkData = message.data;
          // Handle thought chunks for AI thinking display
          const thinkingMessage: TextMessage = {
            role: 'thinking',
            content: chunkData.content || chunkData.chunk || '',
            timestamp: Date.now(),
          };
          setMessages((prev) => [...prev, thinkingMessage]);
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
          setIsWaitingForResponse(false); // Clear waiting state
          setCurrentStreamContent('');
          currentStreamContentRef.current = '';
          break;

        case 'error':
          setIsStreaming(false);
          setIsWaitingForResponse(false);
          break;

        case 'permissionRequest':
          // Show permission dialog
          handlePermissionRequest(message.data);
          break;

        case 'plan':
          // Update plan entries
          console.log('[App] Plan received:', message.data);
          if (message.data.entries && Array.isArray(message.data.entries)) {
            setPlanEntries(message.data.entries as PlanEntry[]);
          }
          break;

        case 'toolCall':
        case 'toolCallUpdate':
          // Handle tool call updates
          handleToolCallUpdate(message.data);
          break;

        case 'qwenSessionList': {
          const sessions = message.data.sessions || [];
          setQwenSessions(sessions);
          // Only update title if we have a current session selected
          if (currentSessionId && sessions.length > 0) {
            // Update title for the current session if it exists in the list
            const currentSession = sessions.find(
              (s: Record<string, unknown>) =>
                (s.id as string) === currentSessionId ||
                (s.sessionId as string) === currentSessionId,
            );
            if (currentSession) {
              const title =
                (currentSession.title as string) ||
                (currentSession.name as string) ||
                'Past Conversations';
              setCurrentSessionTitle(title);
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
          // Update current session title from session object
          if (message.data.session) {
            const session = message.data.session as Record<string, unknown>;
            const title =
              (session.title as string) ||
              (session.name as string) ||
              'Past Conversations';
            setCurrentSessionTitle(title);
            console.log('[App] Session title updated to:', title);
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
          setPlanEntries([]); // Clear plan entries when switching sessions
          break;

        case 'conversationCleared':
          setMessages([]);
          setCurrentStreamContent('');
          setToolCalls(new Map());
          // Reset session ID and title when conversation is cleared (new session created)
          setCurrentSessionId(null);
          setCurrentSessionTitle('Past Conversations');
          break;

        case 'sessionTitleUpdated': {
          // Update session title when first message is sent
          const sessionId = message.data?.sessionId as string;
          const title = message.data?.title as string;
          if (sessionId && title) {
            console.log('[App] Session title updated:', title);
            setCurrentSessionId(sessionId);
            setCurrentSessionTitle(title);
          }
          break;
        }

        case 'activeEditorChanged': {
          // 从扩展接收当前激活编辑器的文件名
          const fileName = message.data?.fileName as string | null;
          setActiveFileName(fileName);
          break;
        }

        case 'fileAttached': {
          // Handle file attachment from VSCode - insert as @mention
          const attachment = message.data as {
            id: string;
            type: string;
            name: string;
            value: string;
          };

          // Store file reference
          fileReferenceMap.current.set(attachment.name, attachment.value);

          // Insert @filename into input
          if (inputFieldRef.current) {
            const currentText = inputFieldRef.current.textContent || '';
            const newText = currentText
              ? `${currentText} @${attachment.name} `
              : `@${attachment.name} `;
            inputFieldRef.current.textContent = newText;
            setInputText(newText);

            // Move cursor to end
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
          // Handle workspace files list from VSCode
          const files = message.data?.files as Array<{
            id: string;
            label: string;
            description: string;
            path: string;
          }>;
          if (files) {
            setWorkspaceFiles(files);
          }
          break;
        }

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

  // Request current active editor on component mount
  useEffect(() => {
    vscode.postMessage({ type: 'getActiveEditor', data: {} });
  }, [vscode]);

  // Toggle edit mode: ask → auto → plan → ask
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

  // Toggle thinking on/off
  const handleToggleThinking = () => {
    setThinkingEnabled((prev) => !prev);
  };

  // Get edit mode display info
  const getEditModeInfo = () => {
    switch (editMode) {
      case 'ask':
        return {
          text: 'Ask before edits',
          title: 'Qwen will ask before each edit. Click to switch modes.',
          icon: (
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
          ),
        };
      case 'auto':
        return {
          text: 'Edit automatically',
          title: 'Qwen will edit files automatically. Click to switch modes.',
          icon: (
            <svg
              xmlns="http://www.w3.org/2000/svg"
              viewBox="0 0 16 16"
              fill="currentColor"
              aria-hidden="true"
            >
              <path d="M2.53 3.956A1 1 0 0 0 1 4.804v6.392a1 1 0 0 0 1.53.848l5.113-3.196c.16-.1.279-.233.357-.383v2.73a1 1 0 0 0 1.53.849l5.113-3.196a1 1 0 0 0 0-1.696L9.53 3.956A1 1 0 0 0 8 4.804v2.731a.992.992 0 0 0-.357-.383L2.53 3.956Z"></path>
            </svg>
          ),
        };
      case 'plan':
        return {
          text: 'Plan mode',
          title: 'Qwen will plan before executing. Click to switch modes.',
          icon: (
            <svg
              xmlns="http://www.w3.org/2000/svg"
              viewBox="0 0 16 16"
              fill="currentColor"
              aria-hidden="true"
            >
              <path d="M4.5 2a.5.5 0 0 0-.5.5v11a.5.5 0 0 0 .5.5h1a.5.5 0 0 0 .5-.5v-11a.5.5 0 0 0-.5-.5h-1ZM10.5 2a.5.5 0 0 0-.5.5v11a.5.5 0 0 0 .5.5h1a.5.5 0 0 0 .5-.5v-11a.5.5 0 0 0-.5-.5h-1Z"></path>
            </svg>
          ),
        };
      default:
        return {
          text: 'Unknown mode',
          title: 'Unknown edit mode',
          icon: null,
        };
    }
  };

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();

    if (!inputText.trim() || isStreaming) {
      return;
    }

    // Set waiting state with random loading message
    setIsWaitingForResponse(true);
    setLoadingMessage(getRandomLoadingMessage());

    // Parse @file references from input text
    const context: Array<{ type: string; name: string; value: string }> = [];
    const fileRefPattern = /@([^\s]+)/g;
    let match;

    while ((match = fileRefPattern.exec(inputText)) !== null) {
      const fileName = match[1];
      const filePath = fileReferenceMap.current.get(fileName);

      if (filePath) {
        context.push({
          type: 'file',
          name: fileName,
          value: filePath,
        });
      }
    }

    vscode.postMessage({
      type: 'sendMessage',
      data: {
        text: inputText,
        context: context.length > 0 ? context : undefined,
      },
    });

    // Clear input field and file reference map
    setInputText('');
    if (inputFieldRef.current) {
      inputFieldRef.current.textContent = '';
    }
    fileReferenceMap.current.clear();
  };

  const handleLoadQwenSessions = () => {
    vscode.postMessage({ type: 'getQwenSessions', data: {} });
    setShowSessionSelector(true);
  };

  const handleNewQwenSession = () => {
    // Send message to open a new chat tab
    vscode.postMessage({ type: 'openNewChatTab', data: {} });
    setShowSessionSelector(false);
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
    planEntries.length > 0;

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
            <span className="button-text">{currentSessionTitle}</span>
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
            {messages.map((msg, index) => {
              // Special styling for thinking messages (Claude Code style)
              const messageClass =
                msg.role === 'thinking'
                  ? 'message assistant thinking-message'
                  : `message ${msg.role}`;

              return (
                <div key={index} className={messageClass}>
                  <div className="message-content">
                    {msg.role === 'thinking' && (
                      <span className="thinking-indicator">
                        <span className="thinking-dot"></span>
                        <span className="thinking-dot"></span>
                        <span className="thinking-dot"></span>
                      </span>
                    )}
                    <MessageContent
                      content={msg.content}
                      onFileClick={(path) => {
                        vscode.postMessage({
                          type: 'openFile',
                          data: { path },
                        });
                      }}
                    />
                  </div>
                  <div className="message-timestamp">
                    {new Date(msg.timestamp).toLocaleTimeString()}
                  </div>
                </div>
              );
            })}

            {/* Tool Calls - only show those with actual output */}
            {Array.from(toolCalls.values())
              .filter((toolCall) => hasToolCallOutput(toolCall))
              .map((toolCall) => (
                <ToolCall key={toolCall.toolCallId} toolCall={toolCall} />
              ))}

            {/* Plan Display - shows task list when available */}
            {planEntries.length > 0 && <PlanDisplay entries={planEntries} />}

            {/* Loading/Waiting Message - in message list */}
            {isWaitingForResponse && loadingMessage && (
              <div className="message assistant waiting-message">
                <div className="message-content">
                  <span className="typing-indicator">
                    <span className="typing-dot"></span>
                    <span className="typing-dot"></span>
                    <span className="typing-dot"></span>
                  </span>
                  <span className="loading-text">{loadingMessage}</span>
                </div>
              </div>
            )}

            {isStreaming && currentStreamContent && (
              <div className="message assistant streaming">
                <div className="message-content">
                  <MessageContent
                    content={currentStreamContent}
                    onFileClick={(path) => {
                      vscode.postMessage({
                        type: 'openFile',
                        data: { path },
                      });
                    }}
                  />
                </div>
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
          {/* Context Pills - Removed: now using inline @mentions in input */}

          <form className="input-form" onSubmit={handleSubmit}>
            <div className="input-form-background"></div>
            <div className="input-banner"></div>
            <div className="input-wrapper">
              <div
                ref={inputFieldRef}
                contentEditable="plaintext-only"
                className="input-field-editable"
                role="textbox"
                aria-label="Message input"
                aria-multiline="true"
                data-placeholder="Ask Qwen Code …"
                onInput={(e) => {
                  const target = e.target as HTMLDivElement;
                  setInputText(target.textContent || '');
                }}
                onCompositionStart={() => {
                  setIsComposing(true);
                }}
                onCompositionEnd={() => {
                  setIsComposing(false);
                }}
                onKeyDown={(e) => {
                  // 如果正在进行中文输入法输入（拼音输入），不处理回车键
                  if (e.key === 'Enter' && !e.shiftKey && !isComposing) {
                    // 如果 CompletionMenu 打开，让它处理 Enter 键（选中文件）
                    if (completion.isOpen) {
                      return;
                    }
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
                title={getEditModeInfo().title}
                onClick={handleToggleEditMode}
              >
                {getEditModeInfo().icon}
                <span>{getEditModeInfo().text}</span>
              </button>
              {activeFileName && (
                <button
                  type="button"
                  className="action-button active-file-indicator"
                  title={`Showing Qwen Code your current file selection: ${activeFileName}`}
                  onClick={() => {
                    // Request to focus/reveal the active file
                    vscode.postMessage({
                      type: 'focusActiveEditor',
                      data: {},
                    });
                  }}
                >
                  <svg
                    xmlns="http://www.w3.org/2000/svg"
                    viewBox="0 0 20 20"
                    fill="currentColor"
                    aria-hidden="true"
                    data-slot="icon"
                  >
                    <path
                      fillRule="evenodd"
                      d="M6.28 5.22a.75.75 0 0 1 0 1.06L2.56 10l3.72 3.72a.75.75 0 0 1-1.06 1.06L.97 10.53a.75.75 0 0 1 0-1.06l4.25-4.25a.75.75 0 0 1 1.06 0Zm7.44 0a.75.75 0 0 1 1.06 0l4.25 4.25a.75.75 0 0 1 0 1.06l-4.25 4.25a.75.75 0 0 1-1.06-1.06L17.44 10l-3.72-3.72a.75.75 0 0 1 0-1.06ZM11.377 2.011a.75.75 0 0 1 .612.867l-2.5 14.5a.75.75 0 0 1-1.478-.255l2.5-14.5a.75.75 0 0 1 .866-.612Z"
                      clipRule="evenodd"
                    ></path>
                  </svg>
                  <span>{activeFileName}</span>
                </button>
              )}
              <div className="action-divider"></div>
              {/* Spacer 将右侧按钮推到右边 */}
              <div className="input-actions-spacer"></div>
              <button
                type="button"
                className={`action-icon-button thinking-button ${thinkingEnabled ? 'active' : ''}`}
                title={thinkingEnabled ? 'Thinking on' : 'Thinking off'}
                onClick={handleToggleThinking}
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
                onClick={async () => {
                  if (inputFieldRef.current) {
                    // Focus the input first to ensure cursor is in the right place
                    inputFieldRef.current.focus();

                    // Get cursor position for menu placement
                    const selection = window.getSelection();
                    let position = { top: 0, left: 0 };

                    // Try to get precise cursor position
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
                          // Fallback to input element position
                          const inputRect =
                            inputFieldRef.current.getBoundingClientRect();
                          position = {
                            top: inputRect.top,
                            left: inputRect.left,
                          };
                        }
                      } catch (error) {
                        console.error(
                          '[App] Error getting cursor position:',
                          error,
                        );
                        const inputRect =
                          inputFieldRef.current.getBoundingClientRect();
                        position = { top: inputRect.top, left: inputRect.left };
                      }
                    } else {
                      // No selection, use input element position
                      const inputRect =
                        inputFieldRef.current.getBoundingClientRect();
                      position = { top: inputRect.top, left: inputRect.left };
                    }

                    // Open completion menu with / commands
                    await completion.openCompletion('/', '', position);
                  }
                }}
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
                type="button"
                className="action-icon-button attach-button"
                title="Attach context (Cmd/Ctrl + /)"
                onClick={handleAttachContextClick}
              >
                <svg
                  xmlns="http://www.w3.org/2000/svg"
                  viewBox="0 0 20 20"
                  fill="currentColor"
                  aria-hidden="true"
                >
                  <path
                    fillRule="evenodd"
                    d="M15.621 4.379a3 3 0 0 0-4.242 0l-7 7a3 3 0 0 0 4.241 4.243h.001l.497-.5a.75.75 0 0 1 1.064 1.057l-.498.501-.002.002a4.5 4.5 0 0 1-6.364-6.364l7-7a4.5 4.5 0 0 1 6.368 6.36l-3.455 3.553A2.625 2.625 0 1 1 9.52 9.52l3.45-3.451a.75.75 0 1 1 1.061 1.06l-3.45 3.451a1.125 1.125 0 0 0 1.587 1.595l3.454-3.553a3 3 0 0 0 0-4.242Z"
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

      {/* Permission Drawer - Cursor style */}
      {permissionRequest && (
        <PermissionDrawer
          isOpen={!!permissionRequest}
          options={permissionRequest.options}
          toolCall={permissionRequest.toolCall}
          onResponse={handlePermissionResponse}
          onClose={() => setPermissionRequest(null)}
        />
      )}

      {/* Completion Menu for @ and / */}
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
