/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { BaseWebSearchProvider } from '../base-provider.js';
import type { WebSearchResult, WebSearchResultItem } from '../types.js';

interface DashScopeSearchItem {
  _id: string;
  snippet: string;
  title: string;
  url: string;
  timestamp: number;
  timestamp_format: string;
  hostname: string;
  hostlogo?: string;
  web_main_body?: string;
  _score?: number;
}

interface DashScopeSearchResponse {
  headers: Record<string, unknown>;
  rid: string;
  status: number;
  message: string | null;
  data: {
    total: number;
    totalDistinct: number;
    docs: DashScopeSearchItem[];
    keywords?: string[];
    qpInfos?: Array<{
      query: string;
      cleanQuery: string;
      sensitive: boolean;
      spellchecked: string;
      spellcheck: boolean;
      tokenized: string[];
      stopWords: string[];
      synonymWords: string[];
      recognitions: unknown[];
      rewrite: string;
      operator: string;
    }>;
    aggs?: unknown;
    extras?: Record<string, unknown>;
  };
  debug?: unknown;
  success: boolean;
}

/**
 * Configuration for DashScope provider.
 */
export interface DashScopeConfig {
  apiKey: string;
  uid: string;
  appId: string;
  maxResults?: number;
  scene?: string;
  timeout?: number;
}

/**
 * Web search provider using Alibaba Cloud DashScope API.
 */
export class DashScopeProvider extends BaseWebSearchProvider {
  readonly name = 'DashScope';

  constructor(private readonly config: DashScopeConfig) {
    super();
  }

  isAvailable(): boolean {
    return !!(this.config.apiKey && this.config.uid && this.config.appId);
  }

  protected async performSearch(
    query: string,
    signal: AbortSignal,
  ): Promise<WebSearchResult> {
    const requestBody = {
      rid: '',
      uid: this.config.uid,
      scene: this.config.scene || 'dolphin_search_inner_turbo',
      uq: query,
      fields: [],
      page: 1,
      rows: this.config.maxResults || 10,
      customConfigInfo: {},
      headers: {
        __d_head_qto: this.config.timeout || 8000,
        __d_head_app: this.config.appId,
      },
    };

    const response = await fetch(
      'https://dashscope.aliyuncs.com/api/v1/indices/plugin/web_search',
      {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          Authorization: this.config.apiKey,
        },
        body: JSON.stringify(requestBody),
        signal,
      },
    );

    if (!response.ok) {
      const text = await response.text().catch(() => '');
      throw new Error(
        `DashScope API error: ${response.status} ${response.statusText}${text ? ` - ${text}` : ''}`,
      );
    }

    const data = (await response.json()) as DashScopeSearchResponse;

    if (data.status !== 0) {
      throw new Error(
        `DashScope API error: ${data.message || 'Unknown error'}`,
      );
    }

    const results: WebSearchResultItem[] = (data.data?.docs || []).map(
      (item) => ({
        title: item.title,
        url: item.url,
        content: item.snippet,
        score: item._score,
        publishedDate: item.timestamp_format,
      }),
    );

    return this.formatResults(results, query, undefined);
  }
}
