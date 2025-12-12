/**
 * @license
 * Copyright 2025 Qwen Code
 * SPDX-License-Identifier: Apache-2.0
 */

import { useState, useEffect, useCallback, useRef } from 'react';
import { Box, Text, useInput } from 'ink';
import {
  SessionService,
  type SessionListItem,
  type ListSessionsResult,
  getGitBranch,
} from '@qwen-code/qwen-code-core';
import { theme } from '../semantic-colors.js';
import { formatRelativeTime } from '../utils/formatters.js';

const PAGE_SIZE = 20;

export interface ResumeSessionDialogProps {
  cwd: string;
  onSelect: (sessionId: string) => void;
  onCancel: () => void;
  availableTerminalHeight?: number;
}

/**
 * Truncates text to fit within a given width, adding ellipsis if needed.
 */
function truncateText(text: string, maxWidth: number): string {
  if (text.length <= maxWidth) return text;
  if (maxWidth <= 3) return text.slice(0, maxWidth);
  return text.slice(0, maxWidth - 3) + '...';
}

export function ResumeSessionDialog({
  cwd,
  onSelect,
  onCancel,
  availableTerminalHeight,
}: ResumeSessionDialogProps): React.JSX.Element {
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [sessionState, setSessionState] = useState<{
    sessions: SessionListItem[];
    hasMore: boolean;
    nextCursor?: number;
  }>({
    sessions: [],
    hasMore: false,
    nextCursor: undefined,
  });
  const [filterByBranch, setFilterByBranch] = useState(false);
  const [currentBranch, setCurrentBranch] = useState<string | undefined>();
  const [isLoading, setIsLoading] = useState(true);
  const [scrollOffset, setScrollOffset] = useState(0);

  const sessionServiceRef = useRef<SessionService | null>(null);
  const isLoadingMoreRef = useRef(false);

  // Calculate visible items based on terminal height
  const maxVisibleItems = availableTerminalHeight
    ? Math.max(3, Math.floor((availableTerminalHeight - 6) / 3))
    : 5;

  // Initialize session service and load sessions
  useEffect(() => {
    const sessionService = new SessionService(cwd);
    sessionServiceRef.current = sessionService;

    const branch = getGitBranch(cwd);
    setCurrentBranch(branch);

    const loadInitialSessions = async () => {
      try {
        const result: ListSessionsResult = await sessionService.listSessions({
          size: PAGE_SIZE,
        });
        setSessionState({
          sessions: result.items,
          hasMore: result.hasMore,
          nextCursor: result.nextCursor,
        });
      } finally {
        setIsLoading(false);
      }
    };

    loadInitialSessions();
  }, [cwd]);

  // Filter sessions: exclude empty sessions (0 messages) and optionally by branch
  const filteredSessions = sessionState.sessions.filter((session) => {
    // Always exclude sessions with no messages
    if (session.messageCount === 0) {
      return false;
    }
    // Apply branch filter if enabled
    if (filterByBranch && currentBranch) {
      return session.gitBranch === currentBranch;
    }
    return true;
  });

  // Load more sessions when scrolling near the end
  const loadMoreSessions = useCallback(async () => {
    if (
      !sessionState.hasMore ||
      isLoadingMoreRef.current ||
      !sessionServiceRef.current
    ) {
      return;
    }

    isLoadingMoreRef.current = true;
    try {
      const result: ListSessionsResult =
        await sessionServiceRef.current.listSessions({
          size: PAGE_SIZE,
          cursor: sessionState.nextCursor,
        });
      setSessionState((prev) => ({
        sessions: [...prev.sessions, ...result.items],
        hasMore: result.hasMore,
        nextCursor: result.nextCursor,
      }));
    } finally {
      isLoadingMoreRef.current = false;
    }
  }, [sessionState.hasMore, sessionState.nextCursor]);

  // Handle keyboard input
  useInput((input, key) => {
    // Escape to cancel
    if (key.escape) {
      onCancel();
      return;
    }

    // Enter to select
    if (key.return) {
      const session = filteredSessions[selectedIndex];
      if (session) {
        onSelect(session.sessionId);
      }
      return;
    }

    // Navigation
    if (key.upArrow || input === 'k') {
      setSelectedIndex((prev) => {
        const newIndex = Math.max(0, prev - 1);
        // Adjust scroll offset if needed
        if (newIndex < scrollOffset) {
          setScrollOffset(newIndex);
        }
        return newIndex;
      });
      return;
    }

    if (key.downArrow || input === 'j') {
      if (filteredSessions.length === 0) {
        return;
      }
      setSelectedIndex((prev) => {
        const newIndex = Math.min(filteredSessions.length - 1, prev + 1);
        // Adjust scroll offset if needed
        if (newIndex >= scrollOffset + maxVisibleItems) {
          setScrollOffset(newIndex - maxVisibleItems + 1);
        }
        // Load more if near the end
        if (newIndex >= filteredSessions.length - 3 && sessionState.hasMore) {
          loadMoreSessions();
        }
        return newIndex;
      });
      return;
    }

    // Toggle branch filter
    if (input === 'b' || input === 'B') {
      if (currentBranch) {
        setFilterByBranch((prev) => !prev);
        setSelectedIndex(0);
        setScrollOffset(0);
      }
      return;
    }
  });

  // Reset selection when filter changes
  useEffect(() => {
    setSelectedIndex(0);
    setScrollOffset(0);
  }, [filterByBranch]);

  // Get visible sessions for rendering
  const visibleSessions = filteredSessions.slice(
    scrollOffset,
    scrollOffset + maxVisibleItems,
  );
  const showScrollUp = scrollOffset > 0;
  const showScrollDown =
    scrollOffset + maxVisibleItems < filteredSessions.length;

  if (isLoading) {
    return (
      <Box
        flexDirection="column"
        borderStyle="round"
        borderColor={theme.border.default}
        padding={1}
      >
        <Text color={theme.text.primary} bold>
          Resume Session
        </Text>
        <Box paddingY={1}>
          <Text color={theme.text.secondary}>Loading sessions...</Text>
        </Box>
      </Box>
    );
  }

  return (
    <Box
      flexDirection="column"
      borderStyle="round"
      borderColor={theme.border.default}
      padding={1}
    >
      {/* Header */}
      <Box marginBottom={1}>
        <Text color={theme.text.primary} bold>
          Resume Session
        </Text>
        {filterByBranch && currentBranch && (
          <Text color={theme.text.secondary}> (branch: {currentBranch})</Text>
        )}
      </Box>

      {/* Session List */}
      <Box flexDirection="column" paddingX={1}>
        {filteredSessions.length === 0 ? (
          <Box paddingY={1}>
            <Text color={theme.text.secondary}>
              {filterByBranch
                ? `No sessions found for branch "${currentBranch}"`
                : 'No sessions found'}
            </Text>
          </Box>
        ) : (
          visibleSessions.map((session, visibleIndex) => {
            const actualIndex = scrollOffset + visibleIndex;
            const isSelected = actualIndex === selectedIndex;
            const isFirst = visibleIndex === 0;
            const isLast = visibleIndex === visibleSessions.length - 1;
            const timeAgo = formatRelativeTime(session.mtime);
            const messageText =
              session.messageCount === 1
                ? '1 message'
                : `${session.messageCount} messages`;

            // Show scroll indicator on first/last visible items
            const showUpIndicator = isFirst && showScrollUp;
            const showDownIndicator = isLast && showScrollDown;

            // Determine the prefix
            const prefix = isSelected
              ? '> '
              : showUpIndicator
                ? '^ '
                : showDownIndicator
                  ? 'v '
                  : '  ';

            const promptText = session.prompt || '(empty prompt)';
            const truncatedPrompt = truncateText(
              promptText,
              (process.stdout.columns || 80) - 10,
            );

            return (
              <Box
                key={session.sessionId}
                flexDirection="column"
                marginBottom={isLast ? 0 : 1}
              >
                {/* First line: prefix + prompt text */}
                <Box>
                  <Text
                    color={
                      isSelected
                        ? theme.text.accent
                        : showUpIndicator || showDownIndicator
                          ? theme.text.secondary
                          : undefined
                    }
                    bold={isSelected}
                  >
                    {prefix}
                  </Text>
                  <Text
                    color={isSelected ? theme.text.accent : theme.text.primary}
                    bold={isSelected}
                  >
                    {truncatedPrompt}
                  </Text>
                </Box>
                {/* Second line: metadata */}
                <Box paddingLeft={2}>
                  <Text color={theme.text.secondary}>
                    {timeAgo} · {messageText}
                    {session.gitBranch && ` · ${session.gitBranch}`}
                  </Text>
                </Box>
              </Box>
            );
          })
        )}
      </Box>

      {/* Footer */}
      <Box
        marginTop={1}
        borderStyle="single"
        borderTop
        borderBottom={false}
        borderLeft={false}
        borderRight={false}
        paddingTop={1}
      >
        <Text color={theme.text.secondary}>
          {currentBranch && (
            <>
              <Text color={theme.text.accent} bold>
                B
              </Text>
              {' to toggle branch · '}
            </>
          )}
          {'↑↓ to navigate · Enter to select · Esc to cancel'}
        </Text>
      </Box>
    </Box>
  );
}
