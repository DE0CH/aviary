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
import { ChatHeader } from './components/ui/ChatHeader.js';
import {
  UserMessage,
  AssistantMessage,
  ThinkingMessage,
  StreamingMessage,
  WaitingMessage,
} from './components/messages/index.js';
import { InputForm } from './components/InputForm.js';

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
  fileContext?: {
    fileName: string;
    filePath: string;
    startLine?: number;
    endLine?: number;
  };
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
  const [activeFilePath, setActiveFilePath] = useState<string | null>(null);
  const [activeSelection, setActiveSelection] = useState<{
    startLine: number;
    endLine: number;
  } | null>(null);
  const [isComposing, setIsComposing] = useState(false);
  const [showSaveDialog, setShowSaveDialog] = useState(false);
  const [savedSessionTags, setSavedSessionTags] = useState<string[]>([]);

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

      if (item.type === 'command') {
        // Handle /login command directly
        if (item.label === '/login') {
          // Clear input field
          inputElement.textContent = '';
          setInputText('');
          // Close completion
          completion.closeCompletion();
          // Send login command to extension
          vscode.postMessage({
            type: 'login',
            data: {},
          });
          return;
        }

        // For other commands, replace entire input with command
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
      } else if (item.type === 'file') {
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
      }

      // Close completion
      completion.closeCompletion();
    },
    [completion, vscode],
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
  const handleToolCallUpdate = React.useCallback(
    (update: ToolCallUpdate) => {
      setToolCalls((prevToolCalls) => {
        const newMap = new Map(prevToolCalls);
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
        } else if (update.type === 'tool_call_update') {
          // Update existing tool call, or create if it doesn't exist
          const updatedContent = update.content
            ? update.content.map((item) => ({
                type: item.type as 'content' | 'diff',
                content: item.content,
                path: item.path,
                oldText: item.oldText,
                newText: item.newText,
              }))
            : undefined;

          if (existing) {
            // Update existing tool call
            newMap.set(update.toolCallId, {
              ...existing,
              ...(update.kind && { kind: update.kind }),
              ...(update.title && { title: safeTitle(update.title) }),
              ...(update.status && { status: update.status }),
              ...(updatedContent && { content: updatedContent }),
              ...(update.locations && { locations: update.locations }),
            });
          } else {
            // Create new tool call if it doesn't exist (missed the initial tool_call message)
            newMap.set(update.toolCallId, {
              toolCallId: update.toolCallId,
              kind: update.kind || 'other',
              title: safeTitle(update.title),
              status: update.status || 'pending',
              rawInput: update.rawInput as string | object | undefined,
              content: updatedContent,
              locations: update.locations,
            });
          }
        }

        return newMap;
      });
    },
    [setToolCalls],
  );

  const handleSaveSession = useCallback(
    (tag: string) => {
      // Send save session request to extension
      vscode.postMessage({
        type: 'saveSession',
        data: { tag },
      });
      setShowSaveDialog(false);
    },
    [vscode],
  );

  // Handle save session response
  const handleSaveSessionResponse = useCallback(
    (response: { success: boolean; message?: string }) => {
      if (response.success) {
        // Add the new tag to saved session tags
        if (response.message) {
          const tagMatch = response.message.match(/tag: (.+)$/);
          if (tagMatch) {
            setSavedSessionTags((prev) => [...prev, tagMatch[1]]);
          }
        }
      } else {
        // Handle error - could show a toast or error message
        console.error('Failed to save session:', response.message);
      }
    },
    [setSavedSessionTags],
  );

  useEffect(() => {
    // Listen for messages from extension
    const handleMessage = (event: MessageEvent) => {
      const message = event.data;
      // console.log('[App] Received message from extension:', message.type, message);

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
          console.log('[App] 🧠 THOUGHT CHUNK RECEIVED:', chunkData);
          // Handle thought chunks for AI thinking display
          const thinkingMessage: TextMessage = {
            role: 'thinking',
            content: chunkData.content || chunkData.chunk || '',
            timestamp: Date.now(),
          };
          console.log('[App] 🧠 Adding thinking message:', thinkingMessage);
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

        // case 'notLoggedIn':
        //   // Show not logged in message with login button
        //   console.log('[App] Received notLoggedIn message:', message.data);
        //   setIsStreaming(false);
        //   setIsWaitingForResponse(false);
        //   setNotLoggedInMessage(
        //     (message.data as { message: string })?.message ||
        //       'Please login to start chatting.',
        //   );
        //   console.log('[App] Set notLoggedInMessage to:', (message.data as { message: string })?.message);
        //   break;

        case 'permissionRequest': {
          // Show permission dialog
          handlePermissionRequest(message.data);

          // Also create a tool call entry for the permission request
          // This ensures that if it's rejected, we can show it properly
          const permToolCall = message.data?.toolCall as {
            toolCallId?: string;
            kind?: string;
            title?: string;
            status?: string;
            content?: unknown[];
            locations?: Array<{ path: string; line?: number | null }>;
          };

          if (permToolCall?.toolCallId) {
            // Infer kind from title if not provided
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

            handleToolCallUpdate({
              type: 'tool_call',
              toolCallId: permToolCall.toolCallId,
              kind,
              title: permToolCall.title,
              status: permToolCall.status || 'pending',
              content: permToolCall.content as Array<{
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
              }>,
              locations: permToolCall.locations,
            });
          }
          break;
        }

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
          // 从扩展接收当前激活编辑器的文件名和选中的行号
          const fileName = message.data?.fileName as string | null;
          const filePath = message.data?.filePath as string | null;
          const selection = message.data?.selection as {
            startLine: number;
            endLine: number;
          } | null;
          setActiveFileName(fileName);
          setActiveFilePath(filePath);
          setActiveSelection(selection);
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

        case 'saveSessionResponse': {
          // Handle save session response
          handleSaveSessionResponse(message.data);
          break;
        }

        default:
          break;
      }
    };

    window.addEventListener('message', handleMessage);
    return () => window.removeEventListener('message', handleMessage);
  }, [
    currentSessionId,
    handlePermissionRequest,
    handleToolCallUpdate,
    handleSaveSessionResponse,
  ]);

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

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();

    if (!inputText.trim() || isStreaming) {
      return;
    }

    // Check if this is a /login command
    if (inputText.trim() === '/login') {
      // Clear input field
      setInputText('');
      if (inputFieldRef.current) {
        inputFieldRef.current.textContent = '';
      }
      // Send login command to extension
      vscode.postMessage({
        type: 'login',
        data: {},
      });
      return;
    }

    // Set waiting state with random loading message
    setIsWaitingForResponse(true);
    setLoadingMessage(getRandomLoadingMessage());

    // Parse @file references from input text
    const context: Array<{
      type: string;
      name: string;
      value: string;
      startLine?: number;
      endLine?: number;
    }> = [];
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

    // Add active file selection context if present
    if (activeFilePath) {
      const fileName = activeFileName || 'current file';
      context.push({
        type: 'file',
        name: fileName,
        value: activeFilePath,
        startLine: activeSelection?.startLine,
        endLine: activeSelection?.endLine,
      });
    }

    // Build file context for the message
    let fileContextForMessage:
      | {
          fileName: string;
          filePath: string;
          startLine?: number;
          endLine?: number;
        }
      | undefined;

    if (activeFilePath && activeFileName) {
      fileContextForMessage = {
        fileName: activeFileName,
        filePath: activeFilePath,
        startLine: activeSelection?.startLine,
        endLine: activeSelection?.endLine,
      };
    }

    vscode.postMessage({
      type: 'sendMessage',
      data: {
        text: inputText,
        context: context.length > 0 ? context : undefined,
        fileContext: fileContextForMessage,
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

      <ChatHeader
        currentSessionTitle={currentSessionTitle}
        onLoadSessions={handleLoadQwenSessions}
        onSaveSession={() => setShowSaveDialog(true)}
        onNewSession={handleNewQwenSession}
      />

      <div
        className="flex-1 overflow-y-auto overflow-x-hidden pt-5 pr-5 pl-5 pb-[120px] flex flex-col relative min-w-0 focus:outline-none [&::-webkit-scrollbar]:w-2 [&::-webkit-scrollbar-track]:bg-transparent [&::-webkit-scrollbar-thumb]:bg-white/20 [&::-webkit-scrollbar-thumb]:rounded-sm [&::-webkit-scrollbar-thumb:hover]:bg-white/30 [&>*]:flex [&>*]:gap-0 [&>*]:items-start [&>*]:text-left [&>*]:py-2 [&>*]:px-0 [&>*]:flex-col [&>*]:relative [&>*]:animate-[fadeIn_0.2s_ease-in]"
        style={{ backgroundColor: 'var(--app-primary-background)' }}
      >
        {!hasContent ? (
          <EmptyState />
        ) : (
          <>
            {messages.map((msg, index) => {
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

            {/* In-Progress Tool Calls - show only pending/in_progress */}
            {Array.from(toolCalls.values())
              .filter(
                (toolCall) =>
                  toolCall.status === 'pending' ||
                  toolCall.status === 'in_progress',
              )
              .map((toolCall) => (
                <InProgressToolCall
                  key={toolCall.toolCallId}
                  toolCall={toolCall}
                />
              ))}

            {/* Completed Tool Calls - only show those with actual output */}
            {Array.from(toolCalls.values())
              .filter(
                (toolCall) =>
                  (toolCall.status === 'completed' ||
                    toolCall.status === 'failed') &&
                  hasToolCallOutput(toolCall),
              )
              .map((toolCall) => (
                <ToolCall key={toolCall.toolCallId} toolCall={toolCall} />
              ))}

            {/* Plan Display - shows task list when available */}
            {planEntries.length > 0 && <PlanDisplay entries={planEntries} />}

            {/* Loading/Waiting Message - in message list */}
            {isWaitingForResponse && loadingMessage && (
              <WaitingMessage loadingMessage={loadingMessage} />
            )}

            {/* Not Logged In Message with Login Button - COMMENTED OUT */}
            {/* {notLoggedInMessage && (
              <>
                {console.log('[App] Rendering NotLoggedInMessage with message:', notLoggedInMessage)}
                <NotLoggedInMessage
                  message={notLoggedInMessage}
                  onLoginClick={() => {
                    setNotLoggedInMessage(null);
                    vscode.postMessage({
                      type: 'login',
                      data: {},
                    });
                  }}
                  onDismiss={() => setNotLoggedInMessage(null)}
                />
              </>
            )} */}

            {isStreaming && currentStreamContent && (
              <StreamingMessage
                content={currentStreamContent}
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

      {/* Info Banner */}
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
        isStreaming={isStreaming}
        isComposing={isComposing}
        editMode={editMode}
        thinkingEnabled={thinkingEnabled}
        activeFileName={activeFileName}
        activeSelection={activeSelection}
        onInputChange={setInputText}
        onCompositionStart={() => setIsComposing(true)}
        onCompositionEnd={() => setIsComposing(false)}
        onKeyDown={() => {}}
        onSubmit={handleSubmit}
        onToggleEditMode={handleToggleEditMode}
        onToggleThinking={handleToggleThinking}
        onFocusActiveEditor={() => {
          vscode.postMessage({
            type: 'focusActiveEditor',
            data: {},
          });
        }}
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

      {/* Save Session Dialog */}
      <SaveSessionDialog
        isOpen={showSaveDialog}
        onClose={() => setShowSaveDialog(false)}
        onSave={handleSaveSession}
        existingTags={savedSessionTags}
      />

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
