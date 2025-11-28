/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * MessageContent component - renders message with code highlighting and clickable file paths
 */

import type React from 'react';

interface MessageContentProps {
  content: string;
  onFileClick?: (filePath: string) => void;
}

/**
 * Regular expressions for parsing content
 */
const FILE_PATH_REGEX =
  /([a-zA-Z]:)?([/\\][\w\-. ]+)+\.(tsx?|jsx?|css|scss|json|md|py|java|go|rs|c|cpp|h|hpp|sh|yaml|yml|toml|xml|html|vue|svelte)/gi;
// Match file paths with optional line numbers like: path/file.ts#7-14 or path/file.ts#7
const FILE_PATH_WITH_LINES_REGEX =
  /([a-zA-Z]:)?([/\\][\w\-. ]+)+\.(tsx?|jsx?|css|scss|json|md|py|java|go|rs|c|cpp|h|hpp|sh|yaml|yml|toml|xml|html|vue|svelte)#(\d+)(?:-(\d+))?/gi;
const CODE_BLOCK_REGEX = /```(\w+)?\n([\s\S]*?)```/g;
const INLINE_CODE_REGEX = /`([^`]+)`/g;

/**
 * Parses message content and renders with syntax highlighting and clickable file paths
 */
export const MessageContent: React.FC<MessageContentProps> = ({
  content,
  onFileClick,
}) => {
  /**
   * Parse and render content with special handling for code blocks, inline code, and file paths
   */
  const renderContent = () => {
    const parts: React.ReactNode[] = [];
    let lastIndex = 0;
    let matchIndex = 0;

    // First, handle code blocks
    const codeBlockMatches = Array.from(content.matchAll(CODE_BLOCK_REGEX));

    codeBlockMatches.forEach((match) => {
      const [fullMatch, language, code] = match;
      const startIndex = match.index!;

      // Add text before code block
      if (startIndex > lastIndex) {
        const textBefore = content.slice(lastIndex, startIndex);
        parts.push(...renderTextWithInlineCodeAndPaths(textBefore, matchIndex));
        matchIndex++;
      }

      // Add code block with Tailwind CSS
      parts.push(
        <pre
          key={`code-${matchIndex}`}
          className="my-2 overflow-x-auto rounded p-3 leading-[1.5]"
          style={{
            backgroundColor: 'var(--app-code-background, rgba(0, 0, 0, 0.05))',
            border: '1px solid var(--app-primary-border-color)',
            borderRadius: 'var(--corner-radius-small, 4px)',
            fontFamily:
              "var(--app-monospace-font-family, 'SF Mono', Monaco, 'Cascadia Code', 'Roboto Mono', Consolas, 'Courier New', monospace)",
            fontSize: '13px',
          }}
        >
          <code
            className={`language-${language || 'plaintext'}`}
            style={{
              background: 'none',
              padding: 0,
              fontFamily: 'inherit',
              color: 'var(--app-primary-foreground)',
            }}
          >
            {code}
          </code>
        </pre>,
      );
      matchIndex++;

      lastIndex = startIndex + fullMatch.length;
    });

    // Add remaining text
    if (lastIndex < content.length) {
      const remainingText = content.slice(lastIndex);
      parts.push(
        ...renderTextWithInlineCodeAndPaths(remainingText, matchIndex),
      );
    }

    return parts.length > 0 ? parts : content;
  };

  /**
   * Render text with inline code and file paths
   */
  const renderTextWithInlineCodeAndPaths = (
    text: string,
    startIndex: number,
  ) => {
    const parts: React.ReactNode[] = [];
    let lastIndex = 0;
    let matchIndex = startIndex;

    // Split by inline code first
    const inlineCodeMatches = Array.from(text.matchAll(INLINE_CODE_REGEX));

    if (inlineCodeMatches.length === 0) {
      // No inline code, just check for file paths
      return renderTextWithFilePaths(text, matchIndex);
    }

    inlineCodeMatches.forEach((match) => {
      const [fullMatch, code] = match;
      const startIdx = match.index!;

      // Add text before inline code (may contain file paths)
      if (startIdx > lastIndex) {
        parts.push(
          ...renderTextWithFilePaths(
            text.slice(lastIndex, startIdx),
            matchIndex,
          ),
        );
        matchIndex++;
      }

      // Add inline code with Tailwind CSS
      parts.push(
        <code
          key={`inline-${matchIndex}`}
          className="rounded px-1.5 py-0.5 whitespace-nowrap text-[0.9em] inline-block max-w-full overflow-hidden text-ellipsis align-baseline"
          style={{
            backgroundColor: 'var(--app-code-background, rgba(0, 0, 0, 0.05))',
            border: '1px solid var(--app-primary-border-color)',
            fontFamily:
              "var(--app-monospace-font-family, 'SF Mono', Monaco, 'Cascadia Code', 'Roboto Mono', Consolas, 'Courier New', monospace)",
            color: 'var(--app-primary-foreground)',
          }}
        >
          {code}
        </code>,
      );
      matchIndex++;

      lastIndex = startIdx + fullMatch.length;
    });

    // Add remaining text
    if (lastIndex < text.length) {
      parts.push(...renderTextWithFilePaths(text.slice(lastIndex), matchIndex));
    }

    return parts.length > 0 ? parts : [text];
  };

  /**
   * Render text with file paths
   */
  const renderTextWithFilePaths = (text: string, startIndex: number) => {
    const parts: React.ReactNode[] = [];
    let lastIndex = 0;
    let matchIndex = startIndex;

    // First, try to match file paths with line numbers
    const filePathWithLinesMatches = Array.from(
      text.matchAll(FILE_PATH_WITH_LINES_REGEX),
    );
    const processedRanges: Array<{ start: number; end: number }> = [];

    filePathWithLinesMatches.forEach((match) => {
      const fullMatch = match[0];
      const startIdx = match.index!;
      const filePath = fullMatch.split('#')[0]; // Get path without line numbers
      const startLine = match[4]; // Capture group 4 is the start line
      const endLine = match[5]; // Capture group 5 is the end line (optional)

      processedRanges.push({
        start: startIdx,
        end: startIdx + fullMatch.length,
      });

      // Add text before file path
      if (startIdx > lastIndex) {
        parts.push(text.slice(lastIndex, startIdx));
      }

      // Display text with line numbers
      const displayText = endLine
        ? `${filePath}#${startLine}-${endLine}`
        : `${filePath}#${startLine}`;

      // Add file path link with line numbers
      parts.push(
        <button
          key={`path-${matchIndex}`}
          className="bg-transparent border-0 p-0 underline cursor-pointer transition-colors text-[0.95em]"
          style={{
            fontFamily:
              "var(--app-monospace-font-family, 'SF Mono', Monaco, 'Cascadia Code', 'Roboto Mono', Consolas, 'Courier New', monospace)",
            color: 'var(--app-link-foreground, #007ACC)',
          }}
          onMouseEnter={(e) => {
            e.currentTarget.style.color =
              'var(--app-link-active-foreground, #005A9E)';
          }}
          onMouseLeave={(e) => {
            e.currentTarget.style.color = 'var(--app-link-foreground, #007ACC)';
          }}
          onClick={() => onFileClick?.(filePath)}
          title={`Open ${displayText}`}
        >
          {displayText}
        </button>,
      );

      matchIndex++;
      lastIndex = startIdx + fullMatch.length;
    });

    // Now match regular file paths (without line numbers) that weren't already matched
    const filePathMatches = Array.from(text.matchAll(FILE_PATH_REGEX));

    filePathMatches.forEach((match) => {
      const fullMatch = match[0];
      const startIdx = match.index!;

      // Skip if this range was already processed as a path with line numbers
      const isProcessed = processedRanges.some(
        (range) => startIdx >= range.start && startIdx < range.end,
      );
      if (isProcessed) {
        return;
      }

      // Add text before file path
      if (startIdx > lastIndex) {
        parts.push(text.slice(lastIndex, startIdx));
      }

      // Add file path link with Tailwind CSS
      parts.push(
        <button
          key={`path-${matchIndex}`}
          className="bg-transparent border-0 p-0 underline cursor-pointer transition-colors text-[0.95em]"
          style={{
            fontFamily:
              "var(--app-monospace-font-family, 'SF Mono', Monaco, 'Cascadia Code', 'Roboto Mono', Consolas, 'Courier New', monospace)",
            color: 'var(--app-link-foreground, #007ACC)',
          }}
          onMouseEnter={(e) => {
            e.currentTarget.style.color =
              'var(--app-link-active-foreground, #005A9E)';
          }}
          onMouseLeave={(e) => {
            e.currentTarget.style.color = 'var(--app-link-foreground, #007ACC)';
          }}
          onClick={() => onFileClick?.(fullMatch)}
          title={`Open ${fullMatch}`}
        >
          {fullMatch}
        </button>,
      );

      matchIndex++;
      lastIndex = startIdx + fullMatch.length;
    });

    // Add remaining text
    if (lastIndex < text.length) {
      parts.push(text.slice(lastIndex));
    }

    return parts.length > 0 ? parts : [text];
  };

  return <>{renderContent()}</>;
};
