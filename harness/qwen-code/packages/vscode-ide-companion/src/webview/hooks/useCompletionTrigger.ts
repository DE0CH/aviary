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

      console.log(
        '[useCompletionTrigger] handleInput - text:',
        JSON.stringify(text),
        'length:',
        text.length,
      );

      if (!selection || selection.rangeCount === 0) {
        console.log('[useCompletionTrigger] No selection or rangeCount === 0');
        return;
      }

      const range = selection.getRangeAt(0);
      console.log(
        '[useCompletionTrigger] range.startContainer:',
        range.startContainer,
        'startOffset:',
        range.startOffset,
      );
      console.log(
        '[useCompletionTrigger] startContainer === inputElement:',
        range.startContainer === inputElement,
      );
      console.log(
        '[useCompletionTrigger] startContainer.nodeType:',
        range.startContainer.nodeType,
        'TEXT_NODE:',
        Node.TEXT_NODE,
      );

      // Get cursor position more reliably
      // For contentEditable, we need to calculate the actual text offset
      let cursorPosition = text.length; // Default to end of text

      if (range.startContainer === inputElement) {
        // Cursor is directly in the container (e.g., empty or at boundary)
        // Use childNodes to determine position
        const childIndex = range.startOffset;
        let offset = 0;
        for (
          let i = 0;
          i < childIndex && i < inputElement.childNodes.length;
          i++
        ) {
          offset += inputElement.childNodes[i].textContent?.length || 0;
        }
        cursorPosition = offset || text.length;
        console.log(
          '[useCompletionTrigger] Container mode - childIndex:',
          childIndex,
          'offset:',
          offset,
          'cursorPosition:',
          cursorPosition,
        );
      } else if (range.startContainer.nodeType === Node.TEXT_NODE) {
        // Cursor is in a text node - calculate offset from start of input
        const walker = document.createTreeWalker(
          inputElement,
          NodeFilter.SHOW_TEXT,
          null,
        );

        let offset = 0;
        let found = false;
        let node: Node | null = walker.nextNode();
        while (node) {
          if (node === range.startContainer) {
            offset += range.startOffset;
            found = true;
            break;
          }
          offset += node.textContent?.length || 0;
          node = walker.nextNode();
        }
        // If we found the node, use the calculated offset; otherwise use text length
        cursorPosition = found ? offset : text.length;
        console.log(
          '[useCompletionTrigger] Text node mode - found:',
          found,
          'offset:',
          offset,
          'cursorPosition:',
          cursorPosition,
        );
      }

      // Find trigger character before cursor
      // Use text length if cursorPosition is 0 but we have text (edge case for first character)
      const effectiveCursorPosition =
        cursorPosition === 0 && text.length > 0 ? text.length : cursorPosition;
      console.log(
        '[useCompletionTrigger] cursorPosition:',
        cursorPosition,
        'effectiveCursorPosition:',
        effectiveCursorPosition,
      );

      const textBeforeCursor = text.substring(0, effectiveCursorPosition);
      const lastAtMatch = textBeforeCursor.lastIndexOf('@');
      const lastSlashMatch = textBeforeCursor.lastIndexOf('/');

      console.log(
        '[useCompletionTrigger] textBeforeCursor:',
        JSON.stringify(textBeforeCursor),
        'lastAtMatch:',
        lastAtMatch,
        'lastSlashMatch:',
        lastSlashMatch,
      );

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

      console.log(
        '[useCompletionTrigger] triggerPos:',
        triggerPos,
        'triggerChar:',
        triggerChar,
      );

      // Check if trigger is at word boundary (start of line or after space)
      if (triggerPos >= 0 && triggerChar) {
        const charBefore = triggerPos > 0 ? text[triggerPos - 1] : ' ';
        const isValidTrigger =
          charBefore === ' ' || charBefore === '\n' || triggerPos === 0;

        console.log(
          '[useCompletionTrigger] charBefore:',
          JSON.stringify(charBefore),
          'isValidTrigger:',
          isValidTrigger,
        );

        if (isValidTrigger) {
          const query = text.substring(triggerPos + 1, effectiveCursorPosition);

          console.log(
            '[useCompletionTrigger] query:',
            JSON.stringify(query),
            'hasSpace:',
            query.includes(' '),
            'hasNewline:',
            query.includes('\n'),
          );

          // Only show if query doesn't contain spaces (still typing the reference)
          if (!query.includes(' ') && !query.includes('\n')) {
            // Get precise cursor position for menu
            const cursorPos = getCursorPosition();
            console.log(
              '[useCompletionTrigger] Opening completion - cursorPos:',
              cursorPos,
            );
            if (cursorPos) {
              await openCompletion(triggerChar, query, cursorPos);
              return;
            }
          }
        }
      }

      // Close if no valid trigger
      console.log(
        '[useCompletionTrigger] No valid trigger, state.isOpen:',
        state.isOpen,
      );
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
