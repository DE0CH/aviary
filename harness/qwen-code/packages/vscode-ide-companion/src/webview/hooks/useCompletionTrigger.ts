/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type { RefObject } from 'react';
import { useState, useEffect, useCallback } from 'react';
import type { CompletionItem } from '../components/CompletionMenu.js';

interface CompletionTriggerState {
  isOpen: boolean;
  triggerChar: '@' | '/' | null;
  query: string;
  position: { top: number; left: number };
  items: CompletionItem[];
}

/**
 * Hook to handle @ and / completion triggers in contentEditable
 * Based on vscode-copilot-chat's AttachContextAction
 */
export function useCompletionTrigger(
  inputRef: RefObject<HTMLDivElement>,
  getCompletionItems: (
    trigger: '@' | '/',
    query: string,
  ) => Promise<CompletionItem[]>,
) {
  const [state, setState] = useState<CompletionTriggerState>({
    isOpen: false,
    triggerChar: null,
    query: '',
    position: { top: 0, left: 0 },
    items: [],
  });

  const closeCompletion = useCallback(() => {
    setState({
      isOpen: false,
      triggerChar: null,
      query: '',
      position: { top: 0, left: 0 },
      items: [],
    });
  }, []);

  const openCompletion = useCallback(
    async (
      trigger: '@' | '/',
      query: string,
      position: { top: number; left: number },
    ) => {
      const items = await getCompletionItems(trigger, query);
      setState({
        isOpen: true,
        triggerChar: trigger,
        query,
        position,
        items,
      });
    },
    [getCompletionItems],
  );

  useEffect(() => {
    const inputElement = inputRef.current;
    if (!inputElement) {
      return;
    }

    const getCursorPosition = (): { top: number; left: number } | null => {
      const selection = window.getSelection();
      if (!selection || selection.rangeCount === 0) {
        return null;
      }

      try {
        const range = selection.getRangeAt(0);
        const rect = range.getBoundingClientRect();

        // If the range has a valid position, use it
        if (rect.top > 0 && rect.left > 0) {
          return {
            top: rect.top,
            left: rect.left,
          };
        }

        // Fallback: use input element's position
        const inputRect = inputElement.getBoundingClientRect();
        return {
          top: inputRect.top,
          left: inputRect.left,
        };
      } catch (error) {
        console.error(
          '[useCompletionTrigger] Error getting cursor position:',
          error,
        );
        const inputRect = inputElement.getBoundingClientRect();
        return {
          top: inputRect.top,
          left: inputRect.left,
        };
      }
    };

    const handleInput = async () => {
      const text = inputElement.textContent || '';
      const selection = window.getSelection();
      if (!selection || selection.rangeCount === 0) {
        return;
      }

      const range = selection.getRangeAt(0);
      const cursorPosition = range.startOffset;

      // Find trigger character before cursor
      const textBeforeCursor = text.substring(0, cursorPosition);
      const lastAtMatch = textBeforeCursor.lastIndexOf('@');
      const lastSlashMatch = textBeforeCursor.lastIndexOf('/');

      // Check if we're in a trigger context
      let triggerPos = -1;
      let triggerChar: '@' | '/' | null = null;

      if (lastAtMatch > lastSlashMatch) {
        triggerPos = lastAtMatch;
        triggerChar = '@';
      } else if (lastSlashMatch > lastAtMatch) {
        triggerPos = lastSlashMatch;
        triggerChar = '/';
      }

      // Check if trigger is at word boundary (start of line or after space)
      if (triggerPos >= 0 && triggerChar) {
        const charBefore = triggerPos > 0 ? text[triggerPos - 1] : ' ';
        const isValidTrigger =
          charBefore === ' ' || charBefore === '\n' || triggerPos === 0;

        if (isValidTrigger) {
          const query = text.substring(triggerPos + 1, cursorPosition);

          // Only show if query doesn't contain spaces (still typing the reference)
          if (!query.includes(' ') && !query.includes('\n')) {
            // Get precise cursor position for menu
            const cursorPos = getCursorPosition();
            if (cursorPos) {
              await openCompletion(triggerChar, query, cursorPos);
              return;
            }
          }
        }
      }

      // Close if no valid trigger
      if (state.isOpen) {
        closeCompletion();
      }
    };

    inputElement.addEventListener('input', handleInput);
    return () => inputElement.removeEventListener('input', handleInput);
  }, [inputRef, state.isOpen, openCompletion, closeCompletion]);

  return {
    isOpen: state.isOpen,
    triggerChar: state.triggerChar,
    query: state.query,
    position: state.position,
    items: state.items,
    closeCompletion,
    openCompletion,
  };
}
