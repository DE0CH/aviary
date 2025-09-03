/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { MessageType } from '../types.js';
import {
  CommandKind,
  SlashCommand,
  SlashCommandActionReturn,
  OpenDialogActionReturn,
} from './types.js';

export const agentsCommand: SlashCommand = {
  name: 'agents',
  description: 'Manage subagents for specialized task delegation.',
  kind: CommandKind.BUILT_IN,
  subCommands: [
    {
      name: 'create',
      description: 'Create a new subagent with guided setup.',
      kind: CommandKind.BUILT_IN,
      action: (): OpenDialogActionReturn => ({
        type: 'dialog',
        dialog: 'subagent_create',
      }),
    },
    {
      name: 'list',
      description: 'List all available subagents.',
      kind: CommandKind.BUILT_IN,
      action: async (context): Promise<SlashCommandActionReturn | void> => {
        context.ui.addItem(
          {
            type: MessageType.INFO,
            text: 'Listing subagents... (not implemented yet)',
          },
          Date.now(),
        );
      },
    },
    {
      name: 'show',
      description: 'Show detailed information about a subagent.',
      kind: CommandKind.BUILT_IN,
      action: async (
        context,
        args,
      ): Promise<SlashCommandActionReturn | void> => {
        if (!args || args.trim() === '') {
          return {
            type: 'message',
            messageType: 'error',
            content: 'Usage: /agents show <subagent-name>',
          };
        }

        context.ui.addItem(
          {
            type: MessageType.INFO,
            text: `Showing details for subagent: ${args.trim()} (not implemented yet)`,
          },
          Date.now(),
        );
      },
    },
    {
      name: 'edit',
      description: 'Edit an existing subagent configuration.',
      kind: CommandKind.BUILT_IN,
      action: async (
        context,
        args,
      ): Promise<SlashCommandActionReturn | void> => {
        if (!args || args.trim() === '') {
          return {
            type: 'message',
            messageType: 'error',
            content: 'Usage: /agents edit <subagent-name>',
          };
        }

        context.ui.addItem(
          {
            type: MessageType.INFO,
            text: `Editing subagent: ${args.trim()} (not implemented yet)`,
          },
          Date.now(),
        );
      },
    },
    {
      name: 'delete',
      description: 'Delete a subagent configuration.',
      kind: CommandKind.BUILT_IN,
      action: async (
        context,
        args,
      ): Promise<SlashCommandActionReturn | void> => {
        if (!args || args.trim() === '') {
          return {
            type: 'message',
            messageType: 'error',
            content: 'Usage: /agents delete <subagent-name>',
          };
        }

        context.ui.addItem(
          {
            type: MessageType.INFO,
            text: `Deleting subagent: ${args.trim()} (not implemented yet)`,
          },
          Date.now(),
        );
      },
    },
  ],
};
