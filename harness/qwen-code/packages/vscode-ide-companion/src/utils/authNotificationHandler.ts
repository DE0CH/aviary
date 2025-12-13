/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import * as vscode from 'vscode';
import type { AuthenticateUpdateNotification } from '../types/acpTypes.js';

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

  // Show an information message with the auth URI and copy button
  vscode.window
    .showInformationMessage(
      `Qwen Code needs authentication. Click the button below to open the authentication page or copy the link to your browser.`,
      'Open in Browser',
      'Copy Link',
    )
    .then((selection) => {
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
    });
}
