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

import type { Config } from '../../config/config.js';
import { ApprovalMode } from '../../config/config.js';
import { getErrorMessage } from '../../utils/errors.js';
import { WebSearchProviderFactory } from './provider-factory.js';
import type {
  WebSearchToolParams,
  WebSearchToolResult,
  WebSearchProvider,
  WebSearchResultItem,
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
    // Try to determine which provider will be used
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

    const providers = WebSearchProviderFactory.createProviders(
      webSearchConfig.provider,
    );

    // Determine which provider to use
    let selectedProvider: WebSearchProvider | null = null;

    if (this.params.provider) {
      // Use the specified provider if available
      const provider = providers.get(this.params.provider);
      if (provider && provider.isAvailable()) {
        selectedProvider = provider;
      } else {
        return {
          llmContent: `The specified provider "${this.params.provider}" is not available or not configured. Available providers: ${Array.from(providers.keys()).join(', ')}`,
          returnDisplay: `The WebSearch Provider "${this.params.provider}" not available.`,
        };
      }
    } else {
      // Use default provider
      selectedProvider = WebSearchProviderFactory.getDefaultProvider(
        providers,
        webSearchConfig.default,
      );
    }

    if (!selectedProvider) {
      return {
        llmContent:
          'Web search is disabled because no web search providers are available. Please check your configuration.',
        returnDisplay: 'Web search disabled. No available providers.',
      };
    }

    try {
      const searchResult = await selectedProvider.search(
        this.params.query,
        signal,
      );

      const sources = searchResult.results.map((r: WebSearchResultItem) => ({
        title: r.title,
        url: r.url,
      }));

      const sourceListFormatted = sources.map(
        (s: { title: string; url: string }, i: number) =>
          `[${i + 1}] ${s.title || 'Untitled'} (${s.url})`,
      );

      let content = searchResult.answer?.trim() || '';
      if (!content) {
        // Fallback: build a concise summary from top results
        content = sources
          .slice(0, 3)
          .map(
            (s: { title: string; url: string }, i: number) =>
              `${i + 1}. ${s.title} - ${s.url}`,
          )
          .join('\n');
      }

      if (sourceListFormatted.length > 0) {
        content += `\n\nSources:\n${sourceListFormatted.join('\n')}`;
      }

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
