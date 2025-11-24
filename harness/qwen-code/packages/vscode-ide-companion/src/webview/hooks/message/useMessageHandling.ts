/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import { useState, useRef, useCallback } from 'react';

export interface TextMessage {
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

/**
 * Message handling Hook
 * Manages message list, streaming responses, and loading state
 */
export const useMessageHandling = () => {
  const [messages, setMessages] = useState<TextMessage[]>([]);
  const [isStreaming, setIsStreaming] = useState(false);
  const [isWaitingForResponse, setIsWaitingForResponse] = useState(false);
  const [loadingMessage, setLoadingMessage] = useState('');
  const [currentStreamContent, setCurrentStreamContent] = useState('');

  // Use ref to store current stream content, avoiding useEffect dependency issues
  const currentStreamContentRef = useRef<string>('');

  /**
   * Add message
   */
  const addMessage = useCallback((message: TextMessage) => {
    setMessages((prev) => [...prev, message]);
  }, []);

  /**
   * Clear messages
   */
  const clearMessages = useCallback(() => {
    setMessages([]);
  }, []);

  /**
   * Start streaming response
   */
  const startStreaming = useCallback(() => {
    setIsStreaming(true);
    setCurrentStreamContent('');
    currentStreamContentRef.current = '';
  }, []);

  /**
   * Add stream chunk
   */
  const appendStreamChunk = useCallback((chunk: string) => {
    setCurrentStreamContent((prev) => {
      const newContent = prev + chunk;
      currentStreamContentRef.current = newContent;
      return newContent;
    });
  }, []);

  /**
   * End streaming response
   */
  const endStreaming = useCallback(() => {
    // If there is streaming content, add it as complete assistant message
    if (currentStreamContentRef.current) {
      const assistantMessage: TextMessage = {
        role: 'assistant',
        content: currentStreamContentRef.current,
        timestamp: Date.now(),
      };
      setMessages((prev) => [...prev, assistantMessage]);
    }

    setIsStreaming(false);
    setIsWaitingForResponse(false);
    setCurrentStreamContent('');
    currentStreamContentRef.current = '';
  }, []);

  /**
   * Set waiting for response state
   */
  const setWaitingForResponse = useCallback((message: string) => {
    setIsWaitingForResponse(true);
    setLoadingMessage(message);
  }, []);

  /**
   * Clear waiting for response state
   */
  const clearWaitingForResponse = useCallback(() => {
    setIsWaitingForResponse(false);
    setLoadingMessage('');
  }, []);

  return {
    // State
    messages,
    isStreaming,
    isWaitingForResponse,
    loadingMessage,
    currentStreamContent,

    // Operations
    addMessage,
    clearMessages,
    startStreaming,
    appendStreamChunk,
    endStreaming,
    setWaitingForResponse,
    clearWaitingForResponse,
    setMessages,
  };
};
