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
import { WebSearchError } from './errors.js';

/**
 * Base implementation for web search providers.
 * Provides common functionality for error handling and result formatting.
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
      throw new WebSearchError(
        this.name,
        'Provider is not available. Please check your configuration.',
      );
    }

    try {
      return await this.performSearch(query, signal);
    } catch (error: unknown) {
      if (error instanceof WebSearchError) {
        throw error;
      }
      throw new WebSearchError(this.name, 'Search failed', error);
    }
  }

  /**
   * Format search results into a consistent format.
   * @param results Raw results from the provider
   * @param query The original search query
   * @param answer Optional answer from the provider
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
}
