/**
 * SdkControlServerTransport - bridges MCP Server with Query's control plane
 *
 * Implements @modelcontextprotocol/sdk Transport interface to enable
 * SDK-embedded MCP servers. Messages flow bidirectionally:
 *
 * MCP Server → send() → Query → control_request (mcp_message) → CLI
 * CLI → control_request (mcp_message) → Query → handleMessage() → MCP Server
 */

import type { JSONRPCMessage } from '@modelcontextprotocol/sdk/types.js';

export type SendToQueryCallback = (message: JSONRPCMessage) => Promise<void>;

export interface SdkControlServerTransportOptions {
  sendToQuery: SendToQueryCallback;
  serverName: string;
}

export class SdkControlServerTransport {
  sendToQuery: SendToQueryCallback;
  private serverName: string;
  private started = false;

  onmessage?: (message: JSONRPCMessage) => void;
  onerror?: (error: Error) => void;
  onclose?: () => void;

  constructor(options: SdkControlServerTransportOptions) {
    this.sendToQuery = options.sendToQuery;
    this.serverName = options.serverName;
  }

  async start(): Promise<void> {
    this.started = true;
  }

  async send(message: JSONRPCMessage): Promise<void> {
    if (!this.started) {
      throw new Error(
        `SdkControlServerTransport (${this.serverName}) not started. Call start() first.`,
      );
    }

    try {
      // Send via Query's control plane
      await this.sendToQuery(message);
    } catch (error) {
      // Invoke error callback if set
      if (this.onerror) {
        this.onerror(error instanceof Error ? error : new Error(String(error)));
      }
      throw error;
    }
  }

  async close(): Promise<void> {
    if (!this.started) {
      return; // Already closed
    }

    this.started = false;

    // Notify MCP Server
    if (this.onclose) {
      this.onclose();
    }
  }

  handleMessage(message: JSONRPCMessage): void {
    if (!this.started) {
      console.warn(
        `[SdkControlServerTransport] Received message for closed transport (${this.serverName})`,
      );
      return;
    }

    if (this.onmessage) {
      this.onmessage(message);
    } else {
      console.warn(
        `[SdkControlServerTransport] No onmessage handler set for ${this.serverName}`,
      );
    }
  }

  handleError(error: Error): void {
    if (this.onerror) {
      this.onerror(error);
    } else {
      console.error(
        `[SdkControlServerTransport] Error for ${this.serverName}:`,
        error,
      );
    }
  }

  isStarted(): boolean {
    return this.started;
  }

  getServerName(): string {
    return this.serverName;
  }
}

export function createSdkControlServerTransport(
  options: SdkControlServerTransportOptions,
): SdkControlServerTransport {
  return new SdkControlServerTransport(options);
}
