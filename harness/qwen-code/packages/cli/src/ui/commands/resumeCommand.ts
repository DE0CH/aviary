/**
 * @license
 * Copyright 2025 Qwen Code
 * SPDX-License-Identifier: Apache-2.0
 */

import type {
  SlashCommand,
  SlashCommandActionReturn,
  CommandContext,
} from './types.js';
import { CommandKind } from './types.js';
import { t } from '../../i18n/index.js';
import { showResumeSessionPicker } from '../components/ResumeSessionPicker.js';
import {
  SessionService,
  buildApiHistoryFromConversation,
  replayUiTelemetryFromConversation,
  uiTelemetryService,
} from '@qwen-code/qwen-code-core';
import { buildResumedHistoryItems } from '../utils/resumeHistoryUtils.js';

export const resumeCommand: SlashCommand = {
  name: 'resume',
  kind: CommandKind.BUILT_IN,
  get description() {
    return t('Resume a previous session');
  },
  action: async (
    context: CommandContext,
  ): Promise<void | SlashCommandActionReturn> => {
    const { config } = context.services;

    if (!config) {
      return {
        type: 'message',
        messageType: 'error',
        content: 'Config not available',
      };
    }

    // Show the session picker
    const cwd = config.getTargetDir();
    const selectedSessionId = await showResumeSessionPicker(cwd);

    if (!selectedSessionId) {
      // User cancelled
      return;
    }

    // Load the session data
    const sessionService = new SessionService(cwd);
    const sessionData = await sessionService.loadSession(selectedSessionId);

    if (!sessionData) {
      return {
        type: 'message',
        messageType: 'error',
        content: `Could not load session: ${selectedSessionId}`,
      };
    }

    // Reset and replay UI telemetry to restore metrics
    uiTelemetryService.reset();
    replayUiTelemetryFromConversation(sessionData.conversation);

    // Build UI history items using existing utility
    const uiHistoryWithIds = buildResumedHistoryItems(sessionData, config);
    // Strip IDs for LoadHistoryActionReturn (IDs are re-assigned by loadHistory)
    const uiHistory = uiHistoryWithIds.map(({ id: _id, ...rest }) => rest);

    // Build API history for the LLM client
    const clientHistory = buildApiHistoryFromConversation(
      sessionData.conversation,
    );

    // Update session in config and context
    config.startNewSession(selectedSessionId);
    if (context.session.startNewSession) {
      context.session.startNewSession(selectedSessionId);
    }

    return {
      type: 'load_history',
      history: uiHistory,
      clientHistory,
    };
  },
};
