/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

// Extend Window interface to include __EXTENSION_URI__
declare global {
  interface Window {
    __EXTENSION_URI__?: string;
  }
}

/**
 * Get the extension URI from the body data attribute or window global
 * @returns Extension URI or undefined if not found
 */
function getExtensionUri(): string | undefined {
  // First try to get from window (for backwards compatibility)
  if (window.__EXTENSION_URI__) {
    return window.__EXTENSION_URI__;
  }

  // Then try to get from body data attribute (CSP-compliant method)
  const bodyUri = document.body?.getAttribute('data-extension-uri');
  if (bodyUri) {
    // Cache it in window for future use
    window.__EXTENSION_URI__ = bodyUri;
    return bodyUri;
  }

  return undefined;
}

/**
 * Generate a resource URL for webview access
 * Similar to the pattern used in other VSCode extensions
 *
 * @param relativePath - Relative path from extension root (e.g., 'assets/icon.png')
 * @returns Full webview-accessible URL
 *
 * @example
 * ```tsx
 * <img src={generateResourceUrl('assets/icon.png')} />
 * ```
 */
export function generateResourceUrl(relativePath: string): string {
  const extensionUri = getExtensionUri();

  if (!extensionUri) {
    console.warn('[resourceUrl] Extension URI not found in window or body');
    return '';
  }

  // Remove leading slash if present
  const cleanPath = relativePath.startsWith('/')
    ? relativePath.slice(1)
    : relativePath;

  // Ensure extension URI has trailing slash
  const baseUri = extensionUri.endsWith('/')
    ? extensionUri
    : `${extensionUri}/`;

  return `${baseUri}${cleanPath}`;
}

/**
 * Shorthand for generating icon URLs
 * @param iconPath - Path relative to assets directory
 */
export function generateIconUrl(iconPath: string): string {
  return generateResourceUrl(`assets/${iconPath}`);
}
