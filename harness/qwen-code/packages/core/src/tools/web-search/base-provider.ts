/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import type {
  WebSearchProvider,
  WebSearchResult,
  WebSearchResultItem,
} from './types.js';
import { getErrorMessage } from '../../utils/errors.js';

/**
 * Base implementation for web search providers.
 */
export abstract class BaseWebSearchProvider implements WebSearchProvider {
  abstract readonly name: string;

  /**
   * Check if the provider is available (has required configuration).
   */
  abstract isAvailable(): boolean;

  /**
   * Perform the actual search implementation.
   * @param query The search query
   * @param signal Abort signal for cancellation
   * @returns Promise resolving to search results
   */
  protected abstract performSearch(
    query: string,
    signal: AbortSignal,
  ): Promise<WebSearchResult>;

  /**
   * Execute a web search with error handling.
   * @param query The search query
   * @param signal Abort signal for cancellation
   * @returns Promise resolving to search results
   */
  async search(query: string, signal: AbortSignal): Promise<WebSearchResult> {
    if (!this.isAvailable()) {
      throw new Error(
        `${this.name} provider is not available. Please check your configuration.`,
      );
    }

    try {
      return await this.performSearch(query, signal);
    } catch (error: unknown) {
      const errorMessage = getErrorMessage(error);
      throw new Error(`Error during ${this.name} search: ${errorMessage}`);
    }
  }

  /**
   * Format search results into a consistent format.
   * @param results Raw results from the provider
   * @param query The original search query
   * @returns Formatted search results
   */
  protected formatResults(
    results: WebSearchResultItem[],
    query: string,
    answer?: string,
  ): WebSearchResult {
    return {
      query,
      answer,
      results,
    };
  }

  /**
   * Create a formatted source list for display.
   * @param results Search result items
   * @returns Formatted source list
   */
  protected createSourceList(results: WebSearchResultItem[]): string {
    return results
      .map((r, i) => `[${i + 1}] ${r.title || 'Untitled'} (${r.url})`)
      .join('\n');
  }

  /**
   * Build a concise summary from search results.
   * @param results Search result items
   * @param maxResults Maximum number of results to include
   * @returns Concise summary string
   */
  protected buildSummary(
    results: WebSearchResultItem[],
    maxResults: number = 3,
  ): string {
    return results
      .slice(0, maxResults)
      .map((r, i) => `${i + 1}. ${r.title} - ${r.url}`)
      .join('\n');
  }
}
