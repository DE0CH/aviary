/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import {
  BaseDeclarativeTool,
  BaseToolInvocation,
  Kind,
  type ToolInvocation,
  type ToolCallConfirmationDetails,
  type ToolInfoConfirmationDetails,
  ToolConfirmationOutcome,
} from '../tools.js';
import { ToolErrorType } from '../tool-error.js';

import type { Config } from '../../config/config.js';
import { ApprovalMode } from '../../config/config.js';
import { getErrorMessage } from '../../utils/errors.js';
import { buildContentWithSources, buildSummary } from './utils.js';
import { TavilyProvider } from './providers/tavily-provider.js';
import { GoogleProvider } from './providers/google-provider.js';
import { DashScopeProvider } from './providers/dashscope-provider.js';
import type {
  WebSearchToolParams,
  WebSearchToolResult,
  WebSearchProvider,
  WebSearchResultItem,
  WebSearchProviderConfig,
} from './types.js';

class WebSearchToolInvocation extends BaseToolInvocation<
  WebSearchToolParams,
  WebSearchToolResult
> {
  constructor(
    private readonly config: Config,
    params: WebSearchToolParams,
  ) {
    super(params);
  }

  override getDescription(): string {
    const webSearchConfig = this.config.getWebSearchConfig();
    const provider =
      this.params.provider || webSearchConfig?.default || 'tavily';
    return ` (Searching the web via ${provider})`;
  }

  override async shouldConfirmExecute(
    _abortSignal: AbortSignal,
  ): Promise<ToolCallConfirmationDetails | false> {
    if (this.config.getApprovalMode() === ApprovalMode.AUTO_EDIT) {
      return false;
    }

    const confirmationDetails: ToolInfoConfirmationDetails = {
      type: 'info',
      title: 'Confirm Web Search',
      prompt: `Search the web for: "${this.params.query}"`,
      onConfirm: async (outcome: ToolConfirmationOutcome) => {
        if (outcome === ToolConfirmationOutcome.ProceedAlways) {
          this.config.setApprovalMode(ApprovalMode.AUTO_EDIT);
        }
      },
    };
    return confirmationDetails;
  }

  /**
   * Create a provider instance from configuration.
   */
  private createProvider(config: WebSearchProviderConfig): WebSearchProvider {
    switch (config.type) {
      case 'tavily':
        return new TavilyProvider(config);
      case 'google':
        return new GoogleProvider(config);
      case 'dashscope':
        return new DashScopeProvider(config);
      default:
        throw new Error('Unknown provider type');
    }
  }

  /**
   * Create all configured providers.
   */
  private createProviders(
    configs: WebSearchProviderConfig[],
  ): Map<string, WebSearchProvider> {
    const providers = new Map<string, WebSearchProvider>();

    for (const config of configs) {
      try {
        const provider = this.createProvider(config);
        if (provider.isAvailable()) {
          providers.set(config.type, provider);
        }
      } catch (error) {
        console.warn(`Failed to create ${config.type} provider:`, error);
      }
    }

    return providers;
  }

  /**
   * Select the appropriate provider based on configuration and parameters.
   */
  private selectProvider(
    providers: Map<string, WebSearchProvider>,
    requestedProvider?: string,
    defaultProvider?: string,
  ): { provider: WebSearchProvider | null; error?: string } {
    // Use requested provider if specified
    if (requestedProvider) {
      const provider = providers.get(requestedProvider);
      if (!provider) {
        const availableProviders = Array.from(providers.keys()).join(', ');
        return {
          provider: null,
          error: `The specified provider "${requestedProvider}" is not available or not configured. Available providers: ${availableProviders}`,
        };
      }
      return { provider };
    }

    // Use default provider if specified and available
    if (defaultProvider && providers.has(defaultProvider)) {
      const provider = providers.get(defaultProvider)!;
      return { provider };
    }

    // Fallback to first available provider
    const firstProvider = providers.values().next().value;
    return { provider: firstProvider || null };
  }

  /**
   * Format search results into a content string.
   */
  private formatSearchResults(searchResult: {
    answer?: string;
    results: WebSearchResultItem[];
  }): {
    content: string;
    sources: Array<{ title: string; url: string }>;
  } {
    const sources = searchResult.results.map((r) => ({
      title: r.title,
      url: r.url,
    }));

    let content = searchResult.answer?.trim() || '';
    if (!content) {
      // Fallback: build a concise summary from top results
      content = buildSummary(sources, 3);
    }

    // Add sources section
    content = buildContentWithSources(content, sources);

    return { content, sources };
  }

  async execute(signal: AbortSignal): Promise<WebSearchToolResult> {
    const webSearchConfig = this.config.getWebSearchConfig();
    if (!webSearchConfig) {
      return {
        llmContent:
          'Web search is disabled because no web search configuration is available. Please configure web search providers in your settings.json.',
        returnDisplay:
          'Web search disabled. Configure web search providers to enable search.',
      };
    }

    const providers = this.createProviders(webSearchConfig.provider);

    const { provider: selectedProvider, error } = this.selectProvider(
      providers,
      this.params.provider,
      webSearchConfig.default,
    );

    if (error) {
      return {
        llmContent: error,
        returnDisplay: `Provider "${this.params.provider}" not available.`,
        error: {
          message: error,
          type: ToolErrorType.INVALID_TOOL_PARAMS,
        },
      };
    }

    if (!selectedProvider) {
      const errorMsg =
        'Web search is disabled because no web search providers are available. Please check your configuration.';
      return {
        llmContent: errorMsg,
        returnDisplay: 'Web search disabled. No available providers.',
        error: {
          message: errorMsg,
          type: ToolErrorType.EXECUTION_FAILED,
        },
      };
    }

    try {
      const searchResult = await selectedProvider.search(
        this.params.query,
        signal,
      );

      const { content, sources } = this.formatSearchResults(searchResult);

      if (!content.trim()) {
        return {
          llmContent: `No search results or information found for query: "${this.params.query}" (searched via ${selectedProvider.name})`,
          returnDisplay: `No information found for "${this.params.query}".`,
        };
      }

      return {
        llmContent: `Web search results for "${this.params.query}" (via ${selectedProvider.name}):\n\n${content}`,
        returnDisplay: `Search results for "${this.params.query}".`,
        sources,
      };
    } catch (error: unknown) {
      const errorMessage = `Error during web search for query "${this.params.query}": ${getErrorMessage(
        error,
      )}`;
      console.error(errorMessage, error);
      return {
        llmContent: `Error: ${errorMessage}`,
        returnDisplay: `Error performing web search.`,
        error: {
          message: errorMessage,
          type: ToolErrorType.EXECUTION_FAILED,
        },
      };
    }
  }
}

/**
 * A tool to perform web searches using configurable providers.
 */
export class WebSearchTool extends BaseDeclarativeTool<
  WebSearchToolParams,
  WebSearchToolResult
> {
  static readonly Name: string = 'web_search';

  constructor(private readonly config: Config) {
    super(
      WebSearchTool.Name,
      'WebSearch',
      'Performs a web search using configurable providers and returns a concise answer with sources.',
      Kind.Search,
      {
        type: 'object',
        properties: {
          query: {
            type: 'string',
            description: 'The search query to find information on the web.',
          },
          provider: {
            type: 'string',
            description:
              'Optional provider to use for the search (e.g., "tavily", "google", "dashscope"). If not specified, the default provider will be used.',
          },
        },
        required: ['query'],
      },
    );
  }

  /**
   * Validates the parameters for the WebSearchTool.
   * @param params The parameters to validate
   * @returns An error message string if validation fails, null if valid
   */
  protected override validateToolParamValues(
    params: WebSearchToolParams,
  ): string | null {
    if (!params.query || params.query.trim() === '') {
      return "The 'query' parameter cannot be empty.";
    }

    // Validate provider parameter if provided
    if (params.provider !== undefined && params.provider.trim() === '') {
      return "The 'provider' parameter cannot be empty if specified.";
    }

    return null;
  }

  protected createInvocation(
    params: WebSearchToolParams,
  ): ToolInvocation<WebSearchToolParams, WebSearchToolResult> {
    return new WebSearchToolInvocation(this.config, params);
  }
}

// Re-export types for external use
export type {
  WebSearchToolParams,
  WebSearchToolResult,
  WebSearchConfig,
  WebSearchProviderConfig,
} from './types.js';
