/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import * as fsPromises from 'fs/promises';
import path from 'path';
import {
  type SlashCommand,
  CommandKind,
  type SlashCommandActionReturn,
} from './types.js';
import { getProjectSummaryPrompt } from '@qwen-code/qwen-code-core';
import type { HistoryItemSummary } from '../types.js';
import { t } from '../../i18n/index.js';

export const summaryCommand: SlashCommand = {
  name: 'summary',
  get description() {
    return t(
      'Generate a project summary and save it to .qwen/PROJECT_SUMMARY.md',
    );
  },
  kind: CommandKind.BUILT_IN,
  action: async (context): Promise<SlashCommandActionReturn> => {
    const { config } = context.services;
    const { ui } = context;
    const executionMode = context.executionMode ?? 'interactive';

    if (!config) {
      return {
        type: 'message',
        messageType: 'error',
        content: t('Config not loaded.'),
      };
    }

    const geminiClient = config.getGeminiClient();
    if (!geminiClient) {
      return {
        type: 'message',
        messageType: 'error',
        content: t('No chat client available to generate summary.'),
      };
    }

    // Check if already generating summary (interactive UI only)
    if (executionMode === 'interactive' && ui.pendingItem) {
      ui.addItem(
        {
          type: 'error' as const,
          text: t(
            'Already generating summary, wait for previous request to complete',
          ),
        },
        Date.now(),
      );
      return {
        type: 'message',
        messageType: 'error',
        content: t(
          'Already generating summary, wait for previous request to complete',
        ),
      };
    }

    const generateSummaryMarkdown = async (): Promise<string> => {
      // Get the current chat history
      const chat = geminiClient.getChat();
      const history = chat.getHistory();

      if (history.length <= 2) {
        throw new Error(t('No conversation found to summarize.'));
      }

      // Build the conversation context for summary generation
      const conversationContext = history.map((message) => ({
        role: message.role,
        parts: message.parts,
      }));

      // Use generateContent with chat history as context
      const response = await geminiClient.generateContent(
        [
          ...conversationContext,
          {
            role: 'user',
            parts: [
              {
                text: getProjectSummaryPrompt(),
              },
            ],
          },
        ],
        {},
        new AbortController().signal,
        config.getModel(),
      );

      // Extract text from response
      const parts = response.candidates?.[0]?.content?.parts;

      const markdownSummary =
        parts
          ?.map((part) => part.text)
          .filter((text): text is string => typeof text === 'string')
          .join('') || '';

      if (!markdownSummary) {
        throw new Error(
          t(
            'Failed to generate summary - no text content received from LLM response',
          ),
        );
      }

      return markdownSummary;
    };

    const saveSummaryToDisk = async (
      markdownSummary: string,
    ): Promise<{
      filePathForDisplay: string;
      fullPath: string;
    }> => {
      // Ensure .qwen directory exists
      const projectRoot = config.getProjectRoot();
      const qwenDir = path.join(projectRoot, '.qwen');
      try {
        await fsPromises.mkdir(qwenDir, { recursive: true });
      } catch (_err) {
        // Directory might already exist, ignore error
      }

      // Save the summary to PROJECT_SUMMARY.md
      const summaryPath = path.join(qwenDir, 'PROJECT_SUMMARY.md');
      const summaryContent = `${markdownSummary}

---

## Summary Metadata
**Update time**: ${new Date().toISOString()} 
`;

      await fsPromises.writeFile(summaryPath, summaryContent, 'utf8');

      return {
        filePathForDisplay: '.qwen/PROJECT_SUMMARY.md',
        fullPath: summaryPath,
      };
    };

    const emitInteractivePending = (stage: 'generating' | 'saving') => {
      if (executionMode !== 'interactive') {
        return;
      }
      const pendingMessage: HistoryItemSummary = {
        type: 'summary',
        summary: {
          isPending: true,
          stage,
        },
      };
      ui.setPendingItem(pendingMessage);
    };

    const completeInteractive = (filePathForDisplay: string) => {
      if (executionMode !== 'interactive') {
        return;
      }
      ui.setPendingItem(null);
      const completedSummaryItem: HistoryItemSummary = {
        type: 'summary',
        summary: {
          isPending: false,
          stage: 'completed',
          filePath: filePathForDisplay,
        },
      };
      ui.addItem(completedSummaryItem, Date.now());
    };

    const failInteractive = (error: unknown) => {
      if (executionMode !== 'interactive') {
        return;
      }
      ui.setPendingItem(null);
      ui.addItem(
        {
          type: 'error' as const,
          text: `❌ ${t(
            'Failed to generate project context summary: {{error}}',
            {
              error: error instanceof Error ? error.message : String(error),
            },
          )}`,
        },
        Date.now(),
      );
    };

    if (executionMode === 'acp') {
      const messages = async function* () {
        try {
          emitInteractivePending('generating');
          yield {
            messageType: 'info' as const,
            content: t('Generating project summary...'),
          };

          const markdownSummary = await generateSummaryMarkdown();

          yield {
            messageType: 'info' as const,
            content: t('Saving project summary...'),
          };
          const { filePathForDisplay } =
            await saveSummaryToDisk(markdownSummary);

          completeInteractive(filePathForDisplay);
          yield {
            messageType: 'info' as const,
            content: t('Saved project summary to {{filePathForDisplay}}.', {
              filePathForDisplay,
            }),
          };
        } catch (error) {
          failInteractive(error);
          yield {
            messageType: 'error' as const,
            content: t(
              'Failed to generate project context summary: {{error}}',
              {
                error: error instanceof Error ? error.message : String(error),
              },
            ),
          };
        }
      };

      return {
        type: 'stream_messages',
        messages: messages(),
      };
    }

    try {
      emitInteractivePending('generating');
      const markdownSummary = await generateSummaryMarkdown();
      emitInteractivePending('saving');
      const { filePathForDisplay } = await saveSummaryToDisk(markdownSummary);
      completeInteractive(filePathForDisplay);

      if (executionMode === 'non_interactive') {
        return {
          type: 'message',
          messageType: 'info',
          content: `Saved project summary to ${filePathForDisplay}.`,
        };
      }

      // Interactive mode: UI components already display progress and completion.
      return {
        type: 'message',
        messageType: 'info',
        content: '',
      };
    } catch (error) {
      // Convert "no conversation" into a clean info message for non-interactive / interactive modes.
      const msg =
        error instanceof Error ? error.message : t('Unknown error occurred.');

      if (msg === t('No conversation found to summarize.')) {
        if (executionMode === 'interactive') {
          // Keep interactive behavior: show as a normal message.
          return {
            type: 'message',
            messageType: 'info',
            content: msg,
          };
        }
        return {
          type: 'message',
          messageType: 'info',
          content: msg,
        };
      }

      failInteractive(error);

      return {
        type: 'message',
        messageType: 'error',
        content: t('Failed to generate project context summary: {{error}}', {
          error: error instanceof Error ? error.message : String(error),
        }),
      };
    }
  },
};
