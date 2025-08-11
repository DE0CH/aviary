/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { Buffer } from 'buffer';
import * as https from 'https';
import { HttpsProxyAgent } from 'https-proxy-agent';
import { randomUUID } from 'crypto';

import {
  StartSessionEvent,
  EndSessionEvent,
  UserPromptEvent,
  ToolCallEvent,
  ApiRequestEvent,
  ApiResponseEvent,
  ApiErrorEvent,
  FlashFallbackEvent,
  LoopDetectedEvent,
  NextSpeakerCheckEvent,
  SlashCommandEvent,
  MalformedJsonResponseEvent,
} from '../types.js';
import {
  RumEvent,
  RumViewEvent,
  RumActionEvent,
  RumResourceEvent,
  RumExceptionEvent,
  RumPayload,
} from './event-types.js';
// Removed unused EventMetadataKey import
import { Config } from '../../config/config.js';
import { safeJsonStringify } from '../../utils/safeJsonStringify.js';
// Removed unused import
import { HttpError, retryWithBackoff } from '../../utils/retry.js';
import { getInstallationId } from '../../utils/user_id.js';

// Usage statistics collection endpoint
const USAGE_STATS_HOSTNAME = 'gb4w8c3ygj-default-sea.rum.aliyuncs.com';
const USAGE_STATS_PATH = '/';

const RUN_APP_ID = 'gb4w8c3ygj@851d5d500f08f92';

export interface LogResponse {
  nextRequestWaitMs?: number;
}

// Singleton class for batch posting log events to RUM. When a new event comes in, the elapsed time
// is checked and events are flushed to RUM if at least a minute has passed since the last flush.
export class QwenLogger {
  private static instance: QwenLogger;
  private config?: Config;
  private readonly events: RumEvent[] = [];
  private last_flush_time: number = Date.now();
  private flush_interval_ms: number = 1000 * 60; // Wait at least a minute before flushing events.
  private userId: string;
  private sessionId: string;
  private viewId: string;
  private isFlushInProgress: boolean = false;

  private constructor(config?: Config) {
    this.config = config;
    this.userId = this.generateUserId();
    this.sessionId = this.config?.getSessionId() ?? '';
    this.viewId = randomUUID();
  }

  private generateUserId(): string {
    // Use installation ID as user ID for consistency
    return `user-${getInstallationId()}`;
  }

  static getInstance(config?: Config): QwenLogger | undefined {
    if (config === undefined || !config?.getUsageStatisticsEnabled())
      return undefined;
    if (!QwenLogger.instance) {
      QwenLogger.instance = new QwenLogger(config);
    }
    return QwenLogger.instance;
  }

  enqueueLogEvent(event: RumEvent): void {
    this.events.push(event);
  }

  createRumEvent(
    eventType: 'view' | 'action' | 'exception' | 'resource',
    properties: RumEvent,
  ): RumEvent {
    return {
      timestamp: Date.now(),
      event_type: eventType,
      ...properties,
    };
  }

  createViewEvent(properties: RumViewEvent): RumEvent {
    return this.createRumEvent('view', properties);
  }

  createActionEvent(properties: RumActionEvent): RumEvent {
    return this.createRumEvent('action', properties);
  }

  createResourceEvent(properties: RumResourceEvent): RumEvent {
    return this.createRumEvent('resource', properties);
  }

  createExceptionEvent(properties: RumExceptionEvent): RumEvent {
    return this.createRumEvent('exception', properties);
  }

  createRumPayload(): RumPayload {
    const version = process.env.CLI_VERSION || process.version;

    return {
      app: {
        id: RUN_APP_ID,
        env: process.env.DEBUG ? 'dev' : 'prod',
        version,
        type: 'cli',
      },
      user: {
        id: this.userId,
      },
      session: {
        id: this.sessionId,
      },
      view: {
        id: this.viewId,
        name: 'qwen-code-cli',
      },
      events: [...this.events],
      _v: `qwen-code@${version}`,
    };
  }

  flushIfNeeded(): void {
    if (Date.now() - this.last_flush_time < this.flush_interval_ms) {
      return;
    }

    // Prevent concurrent flush operations
    if (this.isFlushInProgress) {
      return;
    }

    this.flushToRum().catch((error) => {
      console.debug('Error flushing to RUM:', error);
    });
  }

  async flushToRum(): Promise<LogResponse> {
    if (this.config?.getDebugMode()) {
      console.log('Flushing log events to RUM.');
    }
    if (this.events.length === 0) {
      return {};
    }

    this.isFlushInProgress = true;

    const rumPayload = this.createRumPayload();
    const flushFn = () =>
      new Promise<Buffer>((resolve, reject) => {
        const body = safeJsonStringify(rumPayload);
        const options = {
          hostname: USAGE_STATS_HOSTNAME,
          path: USAGE_STATS_PATH,
          method: 'POST',
          headers: {
            'Content-Length': Buffer.byteLength(body),
            'Content-Type': 'text/plain;charset=UTF-8',
          },
        };
        const bufs: Buffer[] = [];
        const req = https.request(
          {
            ...options,
            agent: this.getProxyAgent(),
          },
          (res) => {
            if (
              res.statusCode &&
              (res.statusCode < 200 || res.statusCode >= 300)
            ) {
              const err: HttpError = new Error(
                `Request failed with status ${res.statusCode}`,
              );
              err.status = res.statusCode;
              res.resume();
              return reject(err);
            }
            res.on('data', (buf) => bufs.push(buf));
            res.on('end', () => resolve(Buffer.concat(bufs)));
          },
        );
        req.on('error', reject);
        req.end(body);
      });

    try {
      await retryWithBackoff(flushFn, {
        maxAttempts: 3,
        initialDelayMs: 200,
        shouldRetry: (err: unknown) => {
          if (!(err instanceof Error)) return false;
          const status = (err as HttpError).status as number | undefined;
          // If status is not available, it's likely a network error
          if (status === undefined) return true;

          // Retry on 429 (Too many Requests) and 5xx server errors.
          return status === 429 || (status >= 500 && status < 600);
        },
      });

      this.events.splice(0, this.events.length);
      this.last_flush_time = Date.now();
      return {};
    } catch (error) {
      if (this.config?.getDebugMode()) {
        console.error('RUM flush failed after multiple retries.', error);
      }
      return {};
    } finally {
      this.isFlushInProgress = false;
    }
  }

  // Visible for testing. Decodes protobuf-encoded response from Qwen server.
  decodeLogResponse(buf: Buffer): LogResponse | undefined {
    // TODO(obrienowen): return specific errors to facilitate debugging.
    if (buf.length < 1) {
      return undefined;
    }

    // The first byte of the buffer is `field<<3 | type`. We're looking for field
    // 1, with type varint, represented by type=0. If the first byte isn't 8, that
    // means field 1 is missing or the message is corrupted. Either way, we return
    // undefined.
    if (buf.readUInt8(0) !== 8) {
      return undefined;
    }

    let ms = BigInt(0);
    let cont = true;

    // In each byte, the most significant bit is the continuation bit. If it's
    // set, we keep going. The lowest 7 bits, are data bits. They are concatenated
    // in reverse order to form the final number.
    for (let i = 1; cont && i < buf.length; i++) {
      const byte = buf.readUInt8(i);
      ms |= BigInt(byte & 0x7f) << BigInt(7 * (i - 1));
      cont = (byte & 0x80) !== 0;
    }

    if (cont) {
      // We have fallen off the buffer without seeing a terminating byte. The
      // message is corrupted.
      return undefined;
    }

    const returnVal = {
      nextRequestWaitMs: Number(ms),
    };
    return returnVal;
  }

  logStartSessionEvent(event: StartSessionEvent): void {
    const applicationEvent = this.createViewEvent({
      type: 'session',
      name: 'session_start',
      snapshots: JSON.stringify({
        model: event.model,
        embedding_model: event.embedding_model,
        sandbox_enabled: event.sandbox_enabled,
        core_tools_enabled: event.core_tools_enabled,
        approval_mode: event.approval_mode,
        api_key_enabled: event.api_key_enabled,
        vertex_ai_enabled: event.vertex_ai_enabled,
        debug_enabled: event.debug_enabled,
        mcp_servers: event.mcp_servers,
        telemetry_enabled: event.telemetry_enabled,
        telemetry_log_user_prompts_enabled:
          event.telemetry_log_user_prompts_enabled,
        file_filtering_respect_git_ignore:
          event.file_filtering_respect_git_ignore,
      }),
    });

    // Flush start event immediately
    this.enqueueLogEvent(applicationEvent);
    this.flushToRum().catch((error: unknown) => {
      console.debug('Error flushing to RUM:', error);
    });
  }

  logNewPromptEvent(event: UserPromptEvent): void {
    const rumEvent = this.createActionEvent({
      type: 'user_prompt',
      name: 'user_prompt',
      snapshots: JSON.stringify({
        prompt_length: event.prompt_length,
        prompt_id: event.prompt_id,
        auth_type: event.auth_type,
      }),
    });

    this.enqueueLogEvent(rumEvent);
    this.flushIfNeeded();
  }

  logToolCallEvent(event: ToolCallEvent): void {
    const rumEvent = this.createActionEvent({
      type: 'tool_call',
      name: `tool_call#${event.function_name}`,
      snapshots: JSON.stringify({
        function_name: event.function_name,
        prompt_id: event.prompt_id,
        decision: event.decision,
        success: event.success,
        duration_ms: event.duration_ms,
        error: event.error,
        error_type: event.error_type,
      }),
    });

    this.enqueueLogEvent(rumEvent);
    this.flushIfNeeded();
  }

  logApiRequestEvent(event: ApiRequestEvent): void {
    const rumEvent = this.createResourceEvent({
      type: 'api',
      name: 'api_request',
      snapshots: JSON.stringify({
        model: event.model,
        prompt_id: event.prompt_id,
      }),
    });

    this.enqueueLogEvent(rumEvent);
    this.flushIfNeeded();
  }

  logApiResponseEvent(event: ApiResponseEvent): void {
    const rumEvent = this.createResourceEvent({
      type: 'api',
      name: 'api_response',
      status_code: event.status_code?.toString() ?? '',
      duration: event.duration_ms,
      success: event.status_code === 200 ? 1 : 0,
      message: event.error,
      snapshots: JSON.stringify({
        model: event.model,
        prompt_id: event.prompt_id,
        input_token_count: event.input_token_count,
        output_token_count: event.output_token_count,
        cached_content_token_count: event.cached_content_token_count,
        thoughts_token_count: event.thoughts_token_count,
        tool_token_count: event.tool_token_count,
        auth_type: event.auth_type,
      }),
    });

    this.enqueueLogEvent(rumEvent);
    this.flushIfNeeded();
  }

  logApiErrorEvent(event: ApiErrorEvent): void {
    const rumEvent = this.createExceptionEvent({
      type: 'error',
      subtype: 'api_error',
      message: event.error,
      snapshots: JSON.stringify({
        model: event.model,
        prompt_id: event.prompt_id,
        error_type: event.error_type,
        status_code: event.status_code,
        duration_ms: event.duration_ms,
        auth_type: event.auth_type,
      }),
    });

    this.enqueueLogEvent(rumEvent);
    this.flushIfNeeded();
  }

  logFlashFallbackEvent(event: FlashFallbackEvent): void {
    const rumEvent = this.createActionEvent({
      type: 'fallback',
      name: 'flash_fallback',
      snapshots: JSON.stringify({
        auth_type: event.auth_type,
      }),
    });

    this.enqueueLogEvent(rumEvent);
    this.flushIfNeeded();
  }

  logLoopDetectedEvent(event: LoopDetectedEvent): void {
    const rumEvent = this.createExceptionEvent({
      type: 'error',
      subtype: 'loop_detected',
      snapshots: JSON.stringify({
        prompt_id: event.prompt_id,
        loop_type: event.loop_type,
      }),
    });

    this.enqueueLogEvent(rumEvent);
    this.flushIfNeeded();
  }

  logNextSpeakerCheck(event: NextSpeakerCheckEvent): void {
    const rumEvent = this.createActionEvent({
      type: 'check',
      name: 'next_speaker_check',
      snapshots: JSON.stringify({
        prompt_id: event.prompt_id,
        finish_reason: event.finish_reason,
        result: event.result,
      }),
    });

    this.enqueueLogEvent(rumEvent);
    this.flushIfNeeded();
  }

  logSlashCommandEvent(event: SlashCommandEvent): void {
    const rumEvent = this.createActionEvent({
      type: 'command',
      name: 'slash_command',
      snapshots: JSON.stringify({
        command: event.command,
        subcommand: event.subcommand,
      }),
    });

    this.enqueueLogEvent(rumEvent);
    this.flushIfNeeded();
  }

  logMalformedJsonResponseEvent(event: MalformedJsonResponseEvent): void {
    const rumEvent = this.createExceptionEvent({
      type: 'error',
      subtype: 'malformed_json_response',
      snapshots: JSON.stringify({
        model: event.model,
      }),
    });

    this.enqueueLogEvent(rumEvent);
    this.flushIfNeeded();
  }

  logEndSessionEvent(_event: EndSessionEvent): void {
    const applicationEvent = this.createViewEvent({
      type: 'session',
      name: 'session_end',
    });

    // Flush immediately on session end.
    this.enqueueLogEvent(applicationEvent);
    this.flushToRum().catch((error: unknown) => {
      console.debug('Error flushing to RUM:', error);
    });
  }

  getProxyAgent() {
    const proxyUrl = this.config?.getProxy();
    if (!proxyUrl) return undefined;
    // undici which is widely used in the repo can only support http & https proxy protocol,
    // https://github.com/nodejs/undici/issues/2224
    if (proxyUrl.startsWith('http')) {
      return new HttpsProxyAgent(proxyUrl);
    } else {
      throw new Error('Unsupported proxy type');
    }
  }

  shutdown() {
    const event = new EndSessionEvent(this.config);
    this.logEndSessionEvent(event);
  }
}
