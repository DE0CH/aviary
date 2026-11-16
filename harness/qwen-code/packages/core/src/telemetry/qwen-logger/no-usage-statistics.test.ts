/**
 * DE0CH: this fork sends no usage statistics (de0ch/README.md). These replace
 * upstream's qwen-logger.test.ts, which tested the upload.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import * as http from 'node:http';
import * as https from 'node:https';
import { QwenLogger } from './qwen-logger.js';
import type { Config } from '../../config/config.js';
import { AuthType } from '../../core/contentGenerator.js';
import { StartSessionEvent, EndSessionEvent } from '../types.js';
import type { RumEvent } from './event-types.js';

vi.mock('../../utils/user_id.js', () => ({
  getInstallationId: vi.fn(() => 'test-installation-id'),
}));

// the upload was an https.request; any way out counts
vi.mock('https', () => ({ request: vi.fn() }));
vi.mock('node:https', () => ({ request: vi.fn() }));
vi.mock('http', () => ({ request: vi.fn() }));
vi.mock('node:http', () => ({ request: vi.fn() }));

const makeFakeConfig = (overrides: Partial<Config> = {}): Config => {
  const defaults = {
    getUsageStatisticsEnabled: () => true,
    getDebugMode: () => false,
    getSessionId: () => 'test-session-id',
    getCliVersion: () => '1.0.0',
    getProxy: () => undefined,
    getContentGeneratorConfig: () => ({ authType: 'test-auth' }),
    getAuthType: () => AuthType.QWEN_OAUTH,
    getMcpServers: () => ({}),
    getModel: () => 'test-model',
    getEmbeddingModel: () => 'test-embedding',
    getSandbox: () => false,
    getCoreTools: () => [],
    getApprovalMode: () => 'auto',
    getTelemetryEnabled: () => true,
    getTelemetryLogPromptsEnabled: () => false,
    getFileFilteringRespectGitIgnore: () => true,
    getOutputFormat: () => 'text',
    getToolRegistry: () => undefined,
    getTruncateToolOutputThreshold: () => 25000,
    getTruncateToolOutputLines: () => 0,
    getIdeMode: () => false,
    getShouldUseNodePtyShell: () => false,
    getHookSystem: () => undefined,
    ...overrides,
  };
  return defaults as Config;
};

describe('QwenLogger in this fork', () => {
  let fetchSpy: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    fetchSpy = vi.fn(() => Promise.reject(new Error('no network in tests')));
    vi.stubGlobal('fetch', fetchSpy);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  const expectNothingSent = () => {
    expect(https.request).not.toHaveBeenCalled();
    expect(http.request).not.toHaveBeenCalled();
    expect(fetchSpy).not.toHaveBeenCalled();
  };

  it('hands out no logger, with usage statistics on, off or no config', () => {
    expect(QwenLogger.getInstance(makeFakeConfig())).toBeUndefined();
    expect(
      QwenLogger.getInstance(
        makeFakeConfig({ getUsageStatisticsEnabled: () => false }),
      ),
    ).toBeUndefined();
    expect(QwenLogger.getInstance()).toBeUndefined();
  });

  it('sends nothing even from a logger built past getInstance', async () => {
    const config = makeFakeConfig();
    // the constructor is private; a merge from upstream that brings the
    // singleton back must still find no way out
    const logger = new (QwenLogger as unknown as {
      new (config: Config): QwenLogger;
    })(config);
    const event: RumEvent = {
      timestamp: Date.now(),
      event_type: 'action',
      type: 'test',
      name: 'test-event',
    };

    await logger.logStartSessionEvent(new StartSessionEvent(config));
    logger.enqueueLogEvent(event);
    logger.logEndSessionEvent(new EndSessionEvent(config));
    await logger.flushToRum();
    // a flush upstream starts without awaiting it
    await new Promise((resolve) => setImmediate(resolve));

    expectNothingSent();
    expect(logger['events'].size).toBe(0);
  });
});
