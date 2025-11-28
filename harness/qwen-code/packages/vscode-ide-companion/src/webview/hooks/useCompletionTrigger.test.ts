/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import { renderHook, act } from '@testing-library/react';
import { useCompletionTrigger } from './useCompletionTrigger';

// Mock CompletionItem type
interface CompletionItem {
  id: string;
  label: string;
  description?: string;
  icon?: React.ReactNode;
  type: 'file' | 'symbol' | 'command' | 'variable';
  value?: unknown;
}

describe('useCompletionTrigger', () => {
  let mockInputRef: React.RefObject<HTMLDivElement>;
  let mockGetCompletionItems: (
    trigger: '@' | '/',
    query: string,
  ) => Promise<CompletionItem[]>;

  beforeEach(() => {
    mockInputRef = {
      current: document.createElement('div'),
    };

    mockGetCompletionItems = jest.fn();
  });

  afterEach(() => {
    jest.clearAllTimers();
  });

  it('should trigger completion when @ is typed at word boundary', async () => {
    mockGetCompletionItems.mockResolvedValue([
      { id: '1', label: 'test.txt', type: 'file' },
    ]);

    const { result } = renderHook(() =>
      useCompletionTrigger(mockInputRef, mockGetCompletionItems),
    );

    // Simulate typing @ at the beginning
    mockInputRef.current.textContent = '@';

    // Mock window.getSelection to return a valid range
    const mockRange = {
      getBoundingClientRect: () => ({ top: 100, left: 50 }),
    };

    window.getSelection = jest.fn().mockReturnValue({
      rangeCount: 1,
      getRangeAt: () => mockRange,
    } as unknown as Selection);

    // Trigger input event
    await act(async () => {
      const event = new Event('input', { bubbles: true });
      mockInputRef.current.dispatchEvent(event);
      // Wait for async operations
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(result.current.isOpen).toBe(true);
    expect(result.current.triggerChar).toBe('@');
    expect(mockGetCompletionItems).toHaveBeenCalledWith('@', '');
  });

  it('should show loading state initially', async () => {
    // Simulate slow file loading
    mockGetCompletionItems.mockImplementation(
      () =>
        new Promise((resolve) =>
          setTimeout(
            () => resolve([{ id: '1', label: 'test.txt', type: 'file' }]),
            100,
          ),
        ),
    );

    const { result } = renderHook(() =>
      useCompletionTrigger(mockInputRef, mockGetCompletionItems),
    );

    // Simulate typing @ at the beginning
    mockInputRef.current.textContent = '@';

    const mockRange = {
      getBoundingClientRect: () => ({ top: 100, left: 50 }),
    };

    window.getSelection = jest.fn().mockReturnValue({
      rangeCount: 1,
      getRangeAt: () => mockRange,
    } as unknown as Selection);

    // Trigger input event
    await act(async () => {
      const event = new Event('input', { bubbles: true });
      mockInputRef.current.dispatchEvent(event);
      // Wait for async operations but not for the slow promise
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    // Should show loading state immediately
    expect(result.current.isOpen).toBe(true);
    expect(result.current.items).toHaveLength(1);
    expect(result.current.items[0].id).toBe('loading');
  });

  it('should timeout if loading takes too long', async () => {
    // Simulate very slow file loading
    mockGetCompletionItems.mockImplementation(
      () =>
        new Promise(
          (resolve) =>
            setTimeout(
              () => resolve([{ id: '1', label: 'test.txt', type: 'file' }]),
              10000,
            ), // 10 seconds
        ),
    );

    const { result } = renderHook(() =>
      useCompletionTrigger(mockInputRef, mockGetCompletionItems),
    );

    // Simulate typing @ at the beginning
    mockInputRef.current.textContent = '@';

    const mockRange = {
      getBoundingClientRect: () => ({ top: 100, left: 50 }),
    };

    window.getSelection = jest.fn().mockReturnValue({
      rangeCount: 1,
      getRangeAt: () => mockRange,
    } as unknown as Selection);

    // Trigger input event
    await act(async () => {
      const event = new Event('input', { bubbles: true });
      mockInputRef.current.dispatchEvent(event);
      // Wait for async operations
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    // Should show loading state initially
    expect(result.current.isOpen).toBe(true);
    expect(result.current.items).toHaveLength(1);
    expect(result.current.items[0].id).toBe('loading');

    // Wait for timeout (5 seconds)
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 5100)); // 5.1 seconds
    });

    // Should show timeout message
    expect(result.current.items).toHaveLength(1);
    expect(result.current.items[0].id).toBe('timeout');
    expect(result.current.items[0].label).toBe('Timeout');
  });

  it('should close completion when cursor moves away from trigger', async () => {
    mockGetCompletionItems.mockResolvedValue([
      { id: '1', label: 'test.txt', type: 'file' },
    ]);

    const { result } = renderHook(() =>
      useCompletionTrigger(mockInputRef, mockGetCompletionItems),
    );

    // Simulate typing @ at the beginning
    mockInputRef.current.textContent = '@';

    const mockRange = {
      getBoundingClientRect: () => ({ top: 100, left: 50 }),
    };

    window.getSelection = jest.fn().mockReturnValue({
      rangeCount: 1,
      getRangeAt: () => mockRange,
    } as unknown as Selection);

    // Trigger input event to open completion
    await act(async () => {
      const event = new Event('input', { bubbles: true });
      mockInputRef.current.dispatchEvent(event);
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(result.current.isOpen).toBe(true);

    // Simulate moving cursor away (typing space after @)
    mockInputRef.current.textContent = '@ ';

    // Trigger input event to close completion
    await act(async () => {
      const event = new Event('input', { bubbles: true });
      mockInputRef.current.dispatchEvent(event);
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    // Should close completion when query contains space
    expect(result.current.isOpen).toBe(false);
  });
});
