/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type * as vscode from 'vscode';

interface AuthState {
  isAuthenticated: boolean;
  authMethod: string;
  timestamp: number;
  workingDir?: string;
}

/**
 * Manages authentication state caching to avoid repeated logins
 */
export class AuthStateManager {
  private static readonly AUTH_STATE_KEY = 'qwen.authState';
  private static readonly AUTH_CACHE_DURATION = 24 * 60 * 60 * 1000; // 24 hours

  constructor(private context: vscode.ExtensionContext) {}

  /**
   * Check if there's a valid cached authentication
   */
  async hasValidAuth(workingDir: string, authMethod: string): Promise<boolean> {
    const state = await this.getAuthState();

    if (!state) {
      return false;
    }

    // Check if auth is still valid (within cache duration)
    const now = Date.now();
    const isExpired =
      now - state.timestamp > AuthStateManager.AUTH_CACHE_DURATION;

    if (isExpired) {
      console.log('[AuthStateManager] Cached auth expired');
      await this.clearAuthState();
      return false;
    }

    // Check if it's for the same working directory and auth method
    const isSameContext =
      state.workingDir === workingDir && state.authMethod === authMethod;

    if (!isSameContext) {
      console.log('[AuthStateManager] Working dir or auth method changed');
      return false;
    }

    console.log('[AuthStateManager] Valid cached auth found');
    return state.isAuthenticated;
  }

  /**
   * Save successful authentication state
   */
  async saveAuthState(workingDir: string, authMethod: string): Promise<void> {
    const state: AuthState = {
      isAuthenticated: true,
      authMethod,
      workingDir,
      timestamp: Date.now(),
    };

    await this.context.globalState.update(
      AuthStateManager.AUTH_STATE_KEY,
      state,
    );
    console.log('[AuthStateManager] Auth state saved');
  }

  /**
   * Clear authentication state
   */
  async clearAuthState(): Promise<void> {
    await this.context.globalState.update(
      AuthStateManager.AUTH_STATE_KEY,
      undefined,
    );
    console.log('[AuthStateManager] Auth state cleared');
  }

  /**
   * Get current auth state
   */
  private async getAuthState(): Promise<AuthState | undefined> {
    return this.context.globalState.get<AuthState>(
      AuthStateManager.AUTH_STATE_KEY,
    );
  }

  /**
   * Get auth state info for debugging
   */
  async getAuthInfo(): Promise<string> {
    const state = await this.getAuthState();
    if (!state) {
      return 'No cached auth';
    }

    const age = Math.floor((Date.now() - state.timestamp) / 1000 / 60);
    return `Auth cached ${age}m ago, method: ${state.authMethod}`;
  }
}
