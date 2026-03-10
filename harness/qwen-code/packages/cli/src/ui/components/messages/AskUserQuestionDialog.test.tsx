/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi } from 'vitest';
import { AskUserQuestionDialog } from './AskUserQuestionDialog.js';
import type { ToolAskUserQuestionConfirmationDetails } from '@qwen-code/qwen-code-core';
import { ToolConfirmationOutcome } from '@qwen-code/qwen-code-core';
import { renderWithProviders } from '../../../test-utils/render.js';

const wait = (ms = 50) => new Promise((resolve) => setTimeout(resolve, ms));

const createSingleQuestion = (
  overrides: Partial<
    ToolAskUserQuestionConfirmationDetails['questions'][0]
  > = {},
): ToolAskUserQuestionConfirmationDetails['questions'][0] => ({
  question: 'What is your favorite color?',
  header: 'Color',
  options: [
    { label: 'Red', description: 'A warm color' },
    { label: 'Blue', description: 'A cool color' },
    { label: 'Green', description: '' },
  ],
  multiSelect: false,
  ...overrides,
});

const createConfirmationDetails = (
  overrides: Partial<ToolAskUserQuestionConfirmationDetails> = {},
): ToolAskUserQuestionConfirmationDetails => ({
  type: 'ask_user_question',
  title: 'Question',
  questions: [createSingleQuestion()],
  onConfirm: vi.fn(),
  ...overrides,
});

describe('<AskUserQuestionDialog />', () => {
  describe('rendering', () => {
    it('renders single question with options', () => {
      const details = createConfirmationDetails();
      const onConfirm = vi.fn();

      const { lastFrame } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );

      const output = lastFrame();
      expect(output).toContain('What is your favorite color?');
      expect(output).toContain('Red');
      expect(output).toContain('Blue');
      expect(output).toContain('Green');
      expect(output).toContain('A warm color');
      expect(output).toContain('A cool color');
    });

    it('renders header for single question', () => {
      const details = createConfirmationDetails();
      const onConfirm = vi.fn();

      const { lastFrame } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );

      expect(lastFrame()).toContain('Color');
    });

    it('renders "Type something..." custom input option', () => {
      const details = createConfirmationDetails();
      const onConfirm = vi.fn();

      const { lastFrame } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );

      expect(lastFrame()).toContain('Type something...');
    });

    it('renders help text for single select', () => {
      const details = createConfirmationDetails();
      const onConfirm = vi.fn();

      const { lastFrame } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );

      expect(lastFrame()).toContain('Enter: Select');
      expect(lastFrame()).toContain('Esc: Cancel');
      expect(lastFrame()).not.toContain('Switch tabs');
    });

    it('renders tabs for multiple questions', () => {
      const details = createConfirmationDetails({
        questions: [
          createSingleQuestion({ header: 'Q1' }),
          createSingleQuestion({
            header: 'Q2',
            question: 'Second question?',
          }),
        ],
      });
      const onConfirm = vi.fn();

      const { lastFrame } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );

      const output = lastFrame();
      expect(output).toContain('Q1');
      expect(output).toContain('Q2');
      expect(output).toContain('Submit');
      expect(output).toContain('Switch tabs');
    });

    it('renders multi-select with checkboxes', () => {
      const details = createConfirmationDetails({
        questions: [createSingleQuestion({ multiSelect: true })],
      });
      const onConfirm = vi.fn();

      const { lastFrame } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );

      const output = lastFrame();
      expect(output).toContain('[ ]');
      expect(output).toContain('Space: Toggle');
      expect(output).toContain('Enter: Confirm');
    });
  });

  describe('single-select interaction', () => {
    it('selects an option with Enter and submits immediately for single question', async () => {
      const onConfirm = vi.fn();
      const details = createConfirmationDetails();

      const { stdin, unmount } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );
      await wait();

      // Press Enter to select the first option (Red)
      stdin.write('\r');
      await wait();

      expect(onConfirm).toHaveBeenCalledWith(
        ToolConfirmationOutcome.ProceedOnce,
        { answers: { 0: 'Red' } },
      );
      unmount();
    });

    it('navigates down with arrow key and selects', async () => {
      const onConfirm = vi.fn();
      const details = createConfirmationDetails();

      const { stdin, unmount } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );
      await wait();

      // Navigate down to "Blue"
      stdin.write('\u001B[B'); // Down arrow
      await wait();

      // Press Enter
      stdin.write('\r');
      await wait();

      expect(onConfirm).toHaveBeenCalledWith(
        ToolConfirmationOutcome.ProceedOnce,
        { answers: { 0: 'Blue' } },
      );
      unmount();
    });

    it('navigates with number keys', async () => {
      const onConfirm = vi.fn();
      const details = createConfirmationDetails();

      const { stdin, unmount } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );
      await wait();

      // Press '2' to select Blue
      stdin.write('2');
      await wait();

      // Press Enter
      stdin.write('\r');
      await wait();

      expect(onConfirm).toHaveBeenCalledWith(
        ToolConfirmationOutcome.ProceedOnce,
        { answers: { 0: 'Blue' } },
      );
      unmount();
    });

    it('cancels with Escape', async () => {
      const onConfirm = vi.fn();
      const details = createConfirmationDetails();

      const { stdin, unmount } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );
      await wait();

      stdin.write('\u001B'); // Escape
      await wait();

      expect(onConfirm).toHaveBeenCalledWith(ToolConfirmationOutcome.Cancel);
      unmount();
    });

    it('does not navigate above first option', async () => {
      const onConfirm = vi.fn();
      const details = createConfirmationDetails();

      const { stdin, unmount } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );
      await wait();

      // Try to go up from first option
      stdin.write('\u001B[A'); // Up arrow
      await wait();

      // Should still select the first option
      stdin.write('\r');
      await wait();

      expect(onConfirm).toHaveBeenCalledWith(
        ToolConfirmationOutcome.ProceedOnce,
        { answers: { 0: 'Red' } },
      );
      unmount();
    });

    it('does not navigate below last option', async () => {
      const onConfirm = vi.fn();
      const details = createConfirmationDetails();

      const { stdin, lastFrame, unmount } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );
      await wait();

      // Navigate way past the last option (3 options + 1 custom input = 4 total)
      for (let i = 0; i < 10; i++) {
        stdin.write('\u001B[B'); // Down arrow
        await wait();
      }

      // Should still render without crashing
      expect(lastFrame()).toContain('What is your favorite color?');
      unmount();
    });
  });

  describe('multi-select interaction', () => {
    it('toggles options with Space', async () => {
      const onConfirm = vi.fn();
      const details = createConfirmationDetails({
        questions: [createSingleQuestion({ multiSelect: true })],
      });

      const { stdin, lastFrame, unmount } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );
      await wait();

      // Space to toggle first option
      stdin.write(' ');
      await wait();

      // Should show checked state
      expect(lastFrame()).toContain('[✓]');
      unmount();
    });

    it('submits multi-select with Space to toggle then Enter to confirm', async () => {
      const onConfirm = vi.fn();
      const details = createConfirmationDetails({
        questions: [createSingleQuestion({ multiSelect: true })],
      });

      const { stdin, unmount } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );
      await wait();

      // Space to toggle first option
      stdin.write(' ');
      await wait();

      // Enter to confirm and submit
      stdin.write('\r');
      await wait();

      expect(onConfirm).toHaveBeenCalledWith(
        ToolConfirmationOutcome.ProceedOnce,
        { answers: { 0: 'Red' } },
      );
      unmount();
    });

    it('shows typed custom input text in frame for multi-select question', async () => {
      const onConfirm = vi.fn();
      const details = createConfirmationDetails({
        questions: [createSingleQuestion({ multiSelect: true })],
      });

      const { stdin, lastFrame, unmount } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );
      await wait();

      // Move to "Type something..." input
      for (let i = 0; i < 3; i++) {
        stdin.write('\u001B[B');
        await wait();
      }

      stdin.write('Orange');
      await wait();

      expect(lastFrame()).toContain('Orange');
      unmount();
    });
  });

  describe('multiple questions', () => {
    it('auto-advances to next question after selecting an option', async () => {
      const onConfirm = vi.fn();
      const details = createConfirmationDetails({
        questions: [
          createSingleQuestion({ header: 'Q1' }),
          createSingleQuestion({
            header: 'Q2',
            question: 'Second question?',
          }),
        ],
      });

      const { stdin, lastFrame, unmount } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );
      await wait();

      // Select first option in Q1
      stdin.write('\r');
      // Wait for auto-advance (150ms timeout) with extra buffer for Windows CI
      await wait(250);

      // Should now show Q2
      expect(lastFrame()).toContain('Second question?');
      unmount();
    });

    it('navigates between tabs with left/right arrows', async () => {
      const onConfirm = vi.fn();
      const details = createConfirmationDetails({
        questions: [
          createSingleQuestion({ header: 'Q1' }),
          createSingleQuestion({
            header: 'Q2',
            question: 'Second question?',
          }),
        ],
      });

      const { stdin, lastFrame, unmount } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );
      await wait();

      // Navigate right to Q2
      stdin.write('\u001B[C'); // Right arrow
      await wait();

      expect(lastFrame()).toContain('Second question?');

      // Navigate left back to Q1
      stdin.write('\u001B[D'); // Left arrow
      await wait();

      expect(lastFrame()).toContain('What is your favorite color?');
      unmount();
    });

    it('shows Submit tab for multiple questions', async () => {
      const onConfirm = vi.fn();
      const details = createConfirmationDetails({
        questions: [
          createSingleQuestion({ header: 'Q1' }),
          createSingleQuestion({ header: 'Q2' }),
        ],
      });

      const { stdin, lastFrame, unmount } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );
      await wait();

      // Navigate to submit tab (right arrow twice: Q1 -> Q2 -> Submit)
      stdin.write('\u001B[C'); // Right
      await wait();
      stdin.write('\u001B[C'); // Right
      await wait();

      const output = lastFrame();
      expect(output).toContain('Submit answers');
      expect(output).toContain('Cancel');
      expect(output).toContain('Your answers');
      unmount();
    });

    it('submits all answers from Submit tab', async () => {
      const onConfirm = vi.fn();
      const details = createConfirmationDetails({
        questions: [
          createSingleQuestion({ header: 'Q1' }),
          createSingleQuestion({
            header: 'Q2',
            question: 'Second question?',
          }),
        ],
      });

      const { stdin, unmount } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );
      await wait();

      // Answer Q1
      stdin.write('\r'); // Select Red
      await wait(200); // Wait for auto-advance (150ms timeout)

      // Answer Q2
      stdin.write('\r'); // Select Red
      // Wait longer for auto-advance to Submit tab to complete
      // Windows CI can have timing issues with setTimeout precision
      await wait(300);

      // Now on Submit tab, press Enter to submit
      stdin.write('\r');
      await wait();

      expect(onConfirm).toHaveBeenCalledWith(
        ToolConfirmationOutcome.ProceedOnce,
        { answers: { 0: 'Red', 1: 'Red' } },
      );
      unmount();
    });

    it('cancels from Submit tab', async () => {
      const onConfirm = vi.fn();
      const details = createConfirmationDetails({
        questions: [
          createSingleQuestion({ header: 'Q1' }),
          createSingleQuestion({ header: 'Q2' }),
        ],
      });

      const { stdin, unmount } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );
      await wait();

      // Navigate to submit tab
      stdin.write('\u001B[C'); // Right
      await wait();
      stdin.write('\u001B[C'); // Right
      await wait();

      // Navigate down to Cancel option
      stdin.write('\u001B[B'); // Down
      await wait();

      // Press Enter
      stdin.write('\r');
      await wait();

      expect(onConfirm).toHaveBeenCalledWith(ToolConfirmationOutcome.Cancel);
      unmount();
    });

    it('shows unanswered questions as (not answered) in Submit tab', async () => {
      const onConfirm = vi.fn();
      const details = createConfirmationDetails({
        questions: [
          createSingleQuestion({ header: 'Q1' }),
          createSingleQuestion({ header: 'Q2' }),
        ],
      });

      const { stdin, lastFrame, unmount } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );
      await wait();

      // Navigate directly to submit tab without answering anything
      stdin.write('\u001B[C'); // Right
      await wait();
      stdin.write('\u001B[C'); // Right
      await wait();

      expect(lastFrame()).toContain('(not answered)');
      unmount();
    });
  });

  describe('focus behavior', () => {
    it('does not respond to keys when isFocused is false', async () => {
      const onConfirm = vi.fn();
      const details = createConfirmationDetails();

      const { stdin, unmount } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          isFocused={false}
          onConfirm={onConfirm}
        />,
      );
      await wait();

      stdin.write('\r'); // Enter
      await wait();
      stdin.write('\u001B'); // Escape
      await wait();

      expect(onConfirm).not.toHaveBeenCalled();
      unmount();
    });
  });

  describe('escape from custom input', () => {
    it('cancels from custom input with Escape', async () => {
      const onConfirm = vi.fn();
      const details = createConfirmationDetails();

      const { stdin, unmount } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );
      await wait();

      // Navigate to custom input (3 options, so index 3 is custom input)
      stdin.write('\u001B[B'); // Down
      await wait();
      stdin.write('\u001B[B'); // Down
      await wait();
      stdin.write('\u001B[B'); // Down - now at custom input
      await wait();

      // Press Escape
      stdin.write('\u001B');
      await wait();

      expect(onConfirm).toHaveBeenCalledWith(ToolConfirmationOutcome.Cancel);
      unmount();
    });
  });

  describe('answered question marker', () => {
    it('shows check mark on answered question tab', async () => {
      const onConfirm = vi.fn();
      const details = createConfirmationDetails({
        questions: [
          createSingleQuestion({ header: 'Q1' }),
          createSingleQuestion({ header: 'Q2' }),
        ],
      });

      const { stdin, lastFrame, unmount } = renderWithProviders(
        <AskUserQuestionDialog
          confirmationDetails={details}
          onConfirm={onConfirm}
        />,
      );
      await wait();

      // Answer Q1
      stdin.write('\r'); // Select Red
      // Wait for auto-advance (150ms timeout) with extra buffer for Windows CI
      await wait(250);

      // Q2 is now active; check that Q1 shows ✓
      expect(lastFrame()).toContain('Q1');
      expect(lastFrame()).toContain('✓');
      unmount();
    });
  });
});
