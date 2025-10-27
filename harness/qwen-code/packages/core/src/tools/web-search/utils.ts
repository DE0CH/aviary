/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Utility functions for web search formatting and processing.
 */

/**
 * Format sources into a numbered list with titles and URLs.
 * @param sources Array of source objects with title and url
 * @returns Formatted source list string
 */
export function formatSources(
  sources: Array<{ title: string; url: string }>,
): string {
  return sources
    .map((s, i) => `[${i + 1}] ${s.title || 'Untitled'} (${s.url})`)
    .join('\n');
}

/**
 * Build content string with appended sources section.
 * @param content Main content text
 * @param sources Array of source objects
 * @returns Combined content with sources
 */
export function buildContentWithSources(
  content: string,
  sources: Array<{ title: string; url: string }>,
): string {
  if (!sources.length) return content;
  return `${content}\n\nSources:\n${formatSources(sources)}`;
}

/**
 * Build a concise summary from top search results.
 * @param sources Array of source objects
 * @param maxResults Maximum number of results to include
 * @returns Concise summary string
 */
export function buildSummary(
  sources: Array<{ title: string; url: string }>,
  maxResults: number = 3,
): string {
  return sources
    .slice(0, maxResults)
    .map((s, i) => `${i + 1}. ${s.title} - ${s.url}`)
    .join('\n');
}
