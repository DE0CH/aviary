/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import * as vscode from 'vscode';
import type { AuthenticateUpdateNotification } from '../types/acpTypes.js';

// Store reference to the authentication notification to allow auto-closing
let authNotificationDisposable: { dispose: () => void } | null = null;

/**
 * Handle authentication update notifications by showing a VS Code notification
 * with the authentication URI and a copy button.
 *
 * @param data - Authentication update notification data containing the auth URI
 */
export function handleAuthenticateUpdate(
  data: AuthenticateUpdateNotification,
): void {
  const authUri = data._meta.authUri;

  // Dismiss any existing authentication notification
  if (authNotificationDisposable) {
    authNotificationDisposable.dispose();
    authNotificationDisposable = null;
  }

  // Show an information message with the auth URI and copy button
  const notificationPromise = vscode.window.showInformationMessage(
    `Qwen Code needs authentication. Click the button below to open the authentication page or copy the link to your browser.`,
    'Open in Browser',
    'Copy Link',
  );

  // Create a simple disposable object
  authNotificationDisposable = {
    dispose: () => {
      // We can't actually cancel the promise, but we can clear our reference
    },
  };

  notificationPromise.then((selection) => {
    if (selection === 'Open in Browser') {
      // Open the authentication URI in the default browser
      vscode.env.openExternal(vscode.Uri.parse(authUri));
    } else if (selection === 'Copy Link') {
      // Copy the authentication URI to clipboard
      vscode.env.clipboard.writeText(authUri);
      vscode.window.showInformationMessage(
        'Authentication link copied to clipboard!',
      );
    }

    // Clear the notification reference after user interaction
    authNotificationDisposable = null;
  });
}

/**
 * Dismiss the authentication notification if it's currently shown
 */
export function dismissAuthenticateUpdate(): void {
  if (authNotificationDisposable) {
    authNotificationDisposable.dispose();
    authNotificationDisposable = null;
  }
}
