/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * MessageContent component - renders message with code highlighting and clickable file paths
 */

import type React from 'react';
import './MessageContent.css';

interface MessageContentProps {
  content: string;
  onFileClick?: (filePath: string) => void;
}

/**
 * Regular expressions for parsing content
 */
const FILE_PATH_REGEX =
  /([a-zA-Z]:)?([/\\][\w\-. ]+)+\.(tsx?|jsx?|css|scss|json|md|py|java|go|rs|c|cpp|h|hpp|sh|yaml|yml|toml|xml|html|vue|svelte)/gi;
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

      // Add code block
      parts.push(
        <pre key={`code-${matchIndex}`} className="message-code-block">
          <code className={`language-${language || 'plaintext'}`}>{code}</code>
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

      // Add inline code
      parts.push(
        <code key={`inline-${matchIndex}`} className="message-inline-code">
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

    const filePathMatches = Array.from(text.matchAll(FILE_PATH_REGEX));

    filePathMatches.forEach((match) => {
      const fullMatch = match[0];
      const startIdx = match.index!;

      // Add text before file path
      if (startIdx > lastIndex) {
        parts.push(text.slice(lastIndex, startIdx));
      }

      // Add file path link
      parts.push(
        <button
          key={`path-${matchIndex}`}
          className="message-file-path"
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
