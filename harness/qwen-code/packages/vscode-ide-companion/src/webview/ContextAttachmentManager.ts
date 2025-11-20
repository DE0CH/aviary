/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Context attachment types
 * Based on vscode-copilot-chat implementation
 */
export interface ContextAttachment {
  id: string;
  type: 'file' | 'symbol' | 'selection' | 'variable';
  name: string;
  value: string | { uri: string; range?: { start: number; end: number } };
  icon?: string;
}

/**
 * Manages context attachments for the chat
 * Similar to ChatContextAttachments in vscode-copilot-chat
 */
export class ContextAttachmentManager {
  private attachments: Map<string, ContextAttachment> = new Map();
  private listeners: Array<(attachments: ContextAttachment[]) => void> = [];

  /**
   * Add a context attachment
   */
  addAttachment(attachment: ContextAttachment): void {
    this.attachments.set(attachment.id, attachment);
    this.notifyListeners();
  }

  /**
   * Remove a context attachment
   */
  removeAttachment(id: string): void {
    this.attachments.delete(id);
    this.notifyListeners();
  }

  /**
   * Get all attachments
   */
  getAttachments(): ContextAttachment[] {
    return Array.from(this.attachments.values());
  }

  /**
   * Check if an attachment exists
   */
  hasAttachment(id: string): boolean {
    return this.attachments.has(id);
  }

  /**
   * Clear all attachments
   */
  clearAttachments(): void {
    this.attachments.clear();
    this.notifyListeners();
  }

  /**
   * Subscribe to attachment changes
   */
  subscribe(listener: (attachments: ContextAttachment[]) => void): () => void {
    this.listeners.push(listener);
    return () => {
      const index = this.listeners.indexOf(listener);
      if (index > -1) {
        this.listeners.splice(index, 1);
      }
    };
  }

  /**
   * Notify all listeners of changes
   */
  private notifyListeners(): void {
    const attachments = this.getAttachments();
    this.listeners.forEach((listener) => listener(attachments));
  }

  /**
   * Get context for message sending
   */
  getContextForMessage(): Array<Record<string, unknown>> {
    return this.getAttachments().map((att) => ({
      id: att.id,
      type: att.type,
      name: att.name,
      value: att.value,
    }));
  }
}
