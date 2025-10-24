/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import type { ToolResult } from '../tools.js';

/**
 * Common interface for all web search providers.
 */
export interface WebSearchProvider {
  /**
   * The name of the provider.
   */
  readonly name: string;

  /**
   * Whether the provider is available (has required configuration).
   */
  isAvailable(): boolean;

  /**
   * Perform a web search with the given query.
   * @param query The search query
   * @param signal Abort signal for cancellation
   * @returns Promise resolving to search results
   */
  search(query: string, signal: AbortSignal): Promise<WebSearchResult>;
}

/**
 * Result item from a web search.
 */
export interface WebSearchResultItem {
  title: string;
  url: string;
  content?: string;
  score?: number;
  publishedDate?: string;
}

/**
 * Result from a web search operation.
 */
export interface WebSearchResult {
  /**
   * The search query that was executed.
   */
  query: string;

  /**
   * A concise answer if available from the provider.
   */
  answer?: string;

  /**
   * List of search result items.
   */
  results: WebSearchResultItem[];

  /**
   * Provider-specific metadata.
   */
  metadata?: Record<string, unknown>;
}

/**
 * Extended tool result that includes sources for web search.
 */
export interface WebSearchToolResult extends ToolResult {
  sources?: Array<{ title: string; url: string }>;
}

/**
 * Parameters for the WebSearchTool.
 */
export interface WebSearchToolParams {
  /**
   * The search query.
   */
  query: string;

  /**
   * Optional provider to use for the search.
   * If not specified, the default provider will be used.
   */
  provider?: string;
}

/**
 * Configuration for web search providers.
 */
export interface WebSearchConfig {
  /**
   * List of available providers with their configurations.
   */
  providers: WebSearchProviderConfig[];

  /**
   * The default provider to use.
   */
  default: string;
}

/**
 * Base configuration for a web search provider.
 */
export interface WebSearchProviderConfig {
  /**
   * The type of provider.
   */
  type: 'tavily' | 'google' | 'dashscope';

  /**
   * Provider-specific configuration.
   */
  config?: Record<string, unknown>;
}
