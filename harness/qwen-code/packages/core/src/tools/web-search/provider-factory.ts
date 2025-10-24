/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import type { WebSearchProvider, WebSearchProviderConfig } from './types.js';
import {
  TavilyProvider,
  type TavilyConfig,
} from './providers/tavily-provider.js';
import {
  GoogleProvider,
  type GoogleConfig,
} from './providers/google-provider.js';
import {
  DashScopeProvider,
  type DashScopeConfig,
} from './providers/dashscope-provider.js';

/**
 * Factory for creating web search providers based on configuration.
 */
export class WebSearchProviderFactory {
  /**
   * Create a web search provider from configuration.
   * @param config Provider configuration
   * @returns Web search provider instance
   */
  static createProvider(config: WebSearchProviderConfig): WebSearchProvider {
    switch (config.type) {
      case 'tavily': {
        const tavilyConfig = config.config as unknown as TavilyConfig;
        if (!tavilyConfig?.apiKey) {
          throw new Error('Tavily provider requires apiKey in configuration');
        }
        return new TavilyProvider(tavilyConfig);
      }

      case 'google': {
        const googleConfig = config.config as unknown as GoogleConfig;
        if (!googleConfig?.apiKey || !googleConfig?.searchEngineId) {
          throw new Error(
            'Google provider requires apiKey and searchEngineId in configuration',
          );
        }
        return new GoogleProvider(googleConfig);
      }

      case 'dashscope': {
        const dashscopeConfig = config.config as unknown as DashScopeConfig;
        return new DashScopeProvider(dashscopeConfig);
      }

      default:
        throw new Error(
          `Unsupported web search provider type: ${(config as WebSearchProviderConfig).type}`,
        );
    }
  }

  /**
   * Create multiple providers from configuration list.
   * @param configs List of provider configurations
   * @returns Map of provider name to provider instance
   */
  static createProviders(
    configs: WebSearchProviderConfig[],
  ): Map<string, WebSearchProvider> {
    const providers = new Map<string, WebSearchProvider>();

    for (const config of configs) {
      try {
        const provider = this.createProvider(config);
        providers.set(config.type, provider);
      } catch (error) {
        console.warn(`Failed to create ${config.type} provider:`, error);
      }
    }

    return providers;
  }

  /**
   * Get the default provider from a list of providers.
   * @param providers Map of available providers
   * @param defaultProviderName Name of the default provider
   * @returns Default provider or the first available provider
   */
  static getDefaultProvider(
    providers: Map<string, WebSearchProvider>,
    defaultProviderName?: string,
  ): WebSearchProvider | null {
    if (defaultProviderName && providers.has(defaultProviderName)) {
      const provider = providers.get(defaultProviderName)!;
      if (provider.isAvailable()) {
        return provider;
      }
    }

    // Fallback to first available provider
    for (const provider of providers.values()) {
      if (provider.isAvailable()) {
        return provider;
      }
    }

    return null;
  }
}
