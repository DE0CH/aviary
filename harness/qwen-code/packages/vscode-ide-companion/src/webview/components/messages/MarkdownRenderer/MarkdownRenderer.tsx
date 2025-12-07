/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * MarkdownRenderer component - renders markdown content with syntax highlighting and clickable file paths
 */

import type React from 'react';
import MarkdownIt from 'markdown-it';
import type { Options as MarkdownItOptions } from 'markdown-it';
import './MarkdownRenderer.css';

interface MarkdownRendererProps {
  content: string;
  onFileClick?: (filePath: string) => void;
}

/**
 * Regular expressions for parsing content
 */
// Match absolute file paths like: /path/to/file.ts or C:\path\to\file.ts
const FILE_PATH_REGEX =
  /(?:[a-zA-Z]:)?[/\\](?:[\w\-. ]+[/\\])+[\w\-. ]+\.(tsx?|jsx?|css|scss|json|md|py|java|go|rs|c|cpp|h|hpp|sh|yaml|yml|toml|xml|html|vue|svelte)/gi;
// Match file paths with optional line numbers like: /path/to/file.ts#7-14 or C:\path\to\file.ts#7
const FILE_PATH_WITH_LINES_REGEX =
  /(?:[a-zA-Z]:)?[/\\](?:[\w\-. ]+[/\\])+[\w\-. ]+\.(tsx?|jsx?|css|scss|json|md|py|java|go|rs|c|cpp|h|hpp|sh|yaml|yml|toml|xml|html|vue|svelte)#(\d+)(?:-(\d+))?/gi;

/**
 * MarkdownRenderer component - renders markdown content with enhanced features
 */
export const MarkdownRenderer: React.FC<MarkdownRendererProps> = ({
  content,
  onFileClick,
}) => {
  /**
   * Initialize markdown-it with plugins
   */
  const getMarkdownInstance = (): MarkdownIt => {
    // Create markdown-it instance with options
    const md = new MarkdownIt({
      html: false, // Disable HTML for security
      xhtmlOut: false,
      breaks: true,
      linkify: true,
      typographer: true,
    } as MarkdownItOptions);

    // Add syntax highlighting + a copy button for code blocks
    md.use((md) => {
      const renderWithCopy = (token: {
        info: string;
        content: string;
      }): string => {
        const lang = (token.info || 'plaintext').trim();
        const content = token.content;

        // Wrap in a container so we can position a copy button
        return (
          `<div class="code-block-wrapper">` +
          `<button class="copy-button" data-lang="${md.utils.escapeHtml(lang)}" aria-label="Copy code block">Copy</button>` +
          `<pre class="code-block language-${md.utils.escapeHtml(lang)}"><code class="language-${md.utils.escapeHtml(lang)}">${md.utils.escapeHtml(content)}</code></pre>` +
          `</div>`
        );
      };

      md.renderer.rules.code_block = function (
        tokens,
        idx: number,
        _options,
        _env,
      ) {
        const token = tokens[idx] as unknown as {
          info: string;
          content: string;
        };
        return renderWithCopy(token);
      };

      md.renderer.rules.fence = function (tokens, idx: number, _options, _env) {
        const token = tokens[idx] as unknown as {
          info: string;
          content: string;
        };
        return renderWithCopy(token);
      };
    });

    return md;
  };

  /**
   * Render markdown content to HTML
   */
  const renderMarkdown = (): string => {
    try {
      const md = getMarkdownInstance();

      // Process the markdown content
      let html = md.render(content);

      // Post-process to add file path click handlers
      html = processFilePaths(html);

      return html;
    } catch (error) {
      console.error('Error rendering markdown:', error);
      // Fallback to plain text if markdown rendering fails
      return escapeHtml(content);
    }
  };

  /**
   * Escape HTML characters for security
   */
  const escapeHtml = (unsafe: string): string =>
    unsafe
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#039;');

  /**
   * Process file paths in HTML to make them clickable
   */
  const processFilePaths = (html: string): string => {
    // If DOM is not available, bail out to avoid breaking SSR
    if (typeof document === 'undefined') {
      return html;
    }

    // Build non-global variants to avoid .test() statefulness
    const FILE_PATH_NO_G = new RegExp(
      FILE_PATH_REGEX.source,
      FILE_PATH_REGEX.flags.replace('g', ''),
    );
    const FILE_PATH_WITH_LINES_NO_G = new RegExp(
      FILE_PATH_WITH_LINES_REGEX.source,
      FILE_PATH_WITH_LINES_REGEX.flags.replace('g', ''),
    );
    // Match a bare file name like README.md (no leading slash)
    const BARE_FILE_REGEX =
      /[\w\-. ]+\.(tsx?|jsx?|css|scss|json|md|py|java|go|rs|c|cpp|h|hpp|sh|ya?ml|toml|xml|html|vue|svelte)/i;

    // Parse HTML into a DOM tree so we don't replace inside attributes
    const container = document.createElement('div');
    container.innerHTML = html;

    const union = new RegExp(
      `${FILE_PATH_WITH_LINES_REGEX.source}|${FILE_PATH_REGEX.source}`,
      'gi',
    );

    const makeLink = (text: string) => {
      const link = document.createElement('a');
      // Pass base path to the handler; keep the full text as label
      const filePath = text.split('#')[0];
      link.className = 'file-path-link';
      link.textContent = text;
      link.setAttribute('href', '#');
      link.setAttribute('title', `Open ${text}`);
      // Carry file path via data attribute; click handled by event delegation
      link.setAttribute('data-file-path', filePath);
      return link;
    };

    const upgradeAnchorIfFilePath = (a: HTMLAnchorElement) => {
      const href = a.getAttribute('href') || '';
      const text = (a.textContent || '').trim();

      // Helper function to check if a string looks like a code reference
      const isCodeReference = (str: string): boolean => {
        // Check if it looks like a code reference (e.g., module.property)
        // Patterns like "vscode.contribution", "module.submodule.function"
        const codeRefPattern = /^[a-zA-Z_$][\w$]*(\.[a-zA-Z_$][\w$]*)+$/;
        return codeRefPattern.test(str);
      };

      // If linkify turned a bare filename into http://<filename>, convert it back
      const httpMatch = href.match(/^https?:\/\/(.+)$/i);
      if (httpMatch && BARE_FILE_REGEX.test(text) && httpMatch[1] === text) {
        // Skip if it looks like a code reference
        if (isCodeReference(text)) {
          return;
        }

        // Treat as a file link instead of external URL
        const filePath = text; // no leading slash
        a.classList.add('file-path-link');
        a.setAttribute('href', '#');
        a.setAttribute('title', `Open ${text}`);
        a.setAttribute('data-file-path', filePath);
        return;
      }

      // Ignore other external protocols
      if (/^(https?|mailto|ftp|data):/i.test(href)) {
        return;
      }

      const candidate = href || text;

      // Skip if it looks like a code reference
      if (isCodeReference(candidate)) {
        return;
      }

      if (
        FILE_PATH_WITH_LINES_NO_G.test(candidate) ||
        FILE_PATH_NO_G.test(candidate)
      ) {
        const filePath = candidate.split('#')[0];
        a.classList.add('file-path-link');
        a.setAttribute('href', '#');
        a.setAttribute('title', `Open ${text || href}`);
        a.setAttribute('data-file-path', filePath);
      }
    };

    // Helper function to check if a string looks like a code reference
    const isCodeReference = (str: string): boolean => {
      // Check if it looks like a code reference (e.g., module.property)
      // Patterns like "vscode.contribution", "module.submodule.function"
      const codeRefPattern = /^[a-zA-Z_$][\w$]*(\.[a-zA-Z_$][\w$]*)+$/;
      return codeRefPattern.test(str);
    };

    const walk = (node: Node) => {
      // Do not transform inside existing anchors
      if (node.nodeType === Node.ELEMENT_NODE) {
        const el = node as HTMLElement;
        if (el.tagName.toLowerCase() === 'a') {
          upgradeAnchorIfFilePath(el as HTMLAnchorElement);
          return; // Don't descend into <a>
        }
      }

      for (let child = node.firstChild; child; ) {
        const next = child.nextSibling; // child may be replaced
        if (child.nodeType === Node.TEXT_NODE) {
          const text = child.nodeValue || '';
          union.lastIndex = 0;
          const hasMatch = union.test(text);
          union.lastIndex = 0;
          if (hasMatch) {
            const frag = document.createDocumentFragment();
            let lastIndex = 0;
            let m: RegExpExecArray | null;
            while ((m = union.exec(text))) {
              const matchText = m[0];
              const idx = m.index;

              // Skip if it looks like a code reference
              if (isCodeReference(matchText)) {
                // Just add the text as-is without creating a link
                if (idx > lastIndex) {
                  frag.appendChild(
                    document.createTextNode(text.slice(lastIndex, idx)),
                  );
                }
                frag.appendChild(document.createTextNode(matchText));
                lastIndex = idx + matchText.length;
                continue;
              }

              if (idx > lastIndex) {
                frag.appendChild(
                  document.createTextNode(text.slice(lastIndex, idx)),
                );
              }
              frag.appendChild(makeLink(matchText));
              lastIndex = idx + matchText.length;
            }
            if (lastIndex < text.length) {
              frag.appendChild(document.createTextNode(text.slice(lastIndex)));
            }
            node.replaceChild(frag, child);
          }
        } else if (child.nodeType === Node.ELEMENT_NODE) {
          walk(child);
        }
        child = next;
      }
    };

    walk(container);
    return container.innerHTML;
  };

  // Event delegation: intercept clicks on copy buttons and generated file-path links
  const handleContainerClick = (
    e: React.MouseEvent<HTMLDivElement, MouseEvent>,
  ) => {
    const target = e.target as HTMLElement | null;
    if (!target) {
      return;
    }

    // Handle copy button clicks for fenced code blocks
    const copyBtn = (target.closest &&
      target.closest('button.copy-button')) as HTMLButtonElement | null;
    if (copyBtn) {
      e.preventDefault();
      e.stopPropagation();

      try {
        const wrapper = copyBtn.closest('.code-block-wrapper');
        const codeEl = wrapper?.querySelector('pre code');
        const text = codeEl?.textContent ?? '';
        if (text) {
          void navigator.clipboard.writeText(text);
          // Quick feedback
          const original = copyBtn.textContent || 'Copy';
          copyBtn.textContent = 'Copied';
          copyBtn.disabled = true;
          setTimeout(() => {
            copyBtn.textContent = original;
            copyBtn.disabled = false;
          }, 1200);
        }
      } catch (err) {
        console.warn('Copy failed:', err);
      }
      return;
    }

    // Find nearest anchor with our marker class
    const anchor = (target.closest &&
      target.closest('a.file-path-link')) as HTMLAnchorElement | null;
    if (!anchor) {
      return;
    }

    const filePath = anchor.getAttribute('data-file-path');
    if (!filePath) {
      return;
    }

    e.preventDefault();
    e.stopPropagation();
    onFileClick?.(filePath);
  };

  return (
    <div
      className="markdown-content"
      onClick={handleContainerClick}
      dangerouslySetInnerHTML={{ __html: renderMarkdown() }}
      style={{
        wordWrap: 'break-word',
        overflowWrap: 'break-word',
        whiteSpace: 'normal',
      }}
    />
  );
};
