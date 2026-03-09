/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import * as fs from 'node:fs';
import type { FileHandle } from 'node:fs/promises';
import {
  getAllConnectionConfigs,
  getFallbackConnectionConfigs,
  type IdeConnectionConfig,
} from './ide-connection-config.js';

vi.mock('node:fs', async (importOriginal) => {
  const actual = await importOriginal<typeof fs>();
  return {
    ...(actual as object),
    promises: {
      ...actual.promises,
      readFile: vi.fn(),
      readdir: vi.fn(),
      stat: vi.fn(),
      unlink: vi.fn(),
    },
    existsSync: vi.fn().mockReturnValue(false),
  };
});

describe('ide-connection-config', () => {
  const mockFs = vi.mocked(fs.promises);
  const mockExistsSync = vi.mocked(fs.existsSync);

  beforeEach(() => {
    mockFs.readFile.mockReset();
    mockFs.readdir.mockReset();
    mockFs.stat.mockReset();
    mockFs.unlink.mockReset();
    mockExistsSync.mockReset();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  describe('getAllConnectionConfigs', () => {
    it('returns an empty array when the IDE lock directory does not exist', async () => {
      mockFs.readdir.mockRejectedValue(new Error('ENOENT: no such directory'));

      const result = await getAllConnectionConfigs('/home/test/.qwen/ide');

      expect(result).toEqual([]);
      expect(mockFs.unlink).not.toHaveBeenCalled();
    });

    it('returns active lock files sorted by mtime and skips invalid JSON', async () => {
      mockFs.readdir.mockResolvedValue([
        '1000.lock',
        '2000.lock',
        '3000.lock',
      ] as never);
      mockFs.readFile.mockImplementation(
        async (filePath: fs.PathLike | FileHandle) => {
          const file = String(filePath);
          if (file.endsWith('1000.lock')) {
            return JSON.stringify({
              port: '1000',
              workspacePath: '/workspace/1',
            });
          }
          if (file.endsWith('2000.lock')) {
            return JSON.stringify({
              port: '2000',
              workspacePath: '/workspace/2',
            });
          }
          if (file.endsWith('3000.lock')) {
            return 'not-json';
          }
          throw new Error(`unexpected path: ${file}`);
        },
      );
      const now = Date.now();
      mockFs.stat.mockImplementation(async (filePath: fs.PathLike) => {
        const file = String(filePath);
        return {
          mtimeMs: file.endsWith('2000.lock') ? now : now - 1000,
        } as fs.Stats;
      });
      mockExistsSync.mockReturnValue(true);

      const result = await getAllConnectionConfigs('/home/test/.qwen/ide');

      expect(result).toEqual([
        { port: '2000', workspacePath: '/workspace/2' },
        { port: '1000', workspacePath: '/workspace/1' },
      ]);
    });

    it('keeps an old lock file when its IDE process is still running', async () => {
      const oldTime = Date.now() - 8 * 24 * 60 * 60 * 1000;

      mockFs.readdir.mockResolvedValue(['1000.lock'] as never);
      mockFs.readFile.mockResolvedValue(
        JSON.stringify({
          port: '1000',
          workspacePath: '/workspace/live',
          ppid: 4242,
        }),
      );
      mockFs.stat.mockResolvedValue({ mtimeMs: oldTime } as never);
      vi.spyOn(process, 'kill').mockImplementation(() => true);

      const result = await getAllConnectionConfigs('/home/test/.qwen/ide');

      expect(mockFs.unlink).not.toHaveBeenCalled();
      expect(result).toEqual([
        { port: '1000', workspacePath: '/workspace/live', ppid: 4242 },
      ]);
    });

    it('removes incomplete lock files older than 7 days and excludes them from results', async () => {
      const now = Date.now();
      const staleTime = now - 7 * 24 * 60 * 60 * 1000 - 1000;
      const recentTime = now - 1000;

      mockFs.readdir.mockResolvedValue(['1000.lock', '2000.lock'] as never);
      mockFs.readFile.mockImplementation(
        async (filePath: fs.PathLike | FileHandle) => {
          const file = String(filePath);
          if (file.endsWith('1000.lock')) {
            return JSON.stringify({ port: '1000' });
          }
          if (file.endsWith('2000.lock')) {
            return JSON.stringify({
              port: '2000',
              workspacePath: '/workspace/new',
            });
          }
          throw new Error(`unexpected path: ${file}`);
        },
      );
      mockFs.stat.mockImplementation(async (filePath: fs.PathLike) => {
        const file = String(filePath);
        return {
          mtimeMs: file.endsWith('1000.lock') ? staleTime : recentTime,
        } as fs.Stats;
      });
      mockExistsSync.mockReturnValue(true);

      const result = await getAllConnectionConfigs('/home/test/.qwen/ide');

      expect(mockFs.unlink).toHaveBeenCalledWith(
        '/home/test/.qwen/ide/1000.lock',
      );
      expect(result).toEqual([
        { port: '2000', workspacePath: '/workspace/new' },
      ]);
    });
  });

  describe('getFallbackConnectionConfigs', () => {
    it('prioritizes workspace matches and excludes the current port', () => {
      const configs: IdeConnectionConfig[] = [
        { port: '1111', workspacePath: '/workspace/other' },
        { port: '2222', workspacePath: '/test/workspace' },
        { port: '3333', workspacePath: '/workspace/another' },
        { workspacePath: '/test/workspace' },
      ];

      const result = getFallbackConnectionConfigs(configs, {
        cwd: '/test/workspace/subdir',
        currentPort: '1111',
        matchesWorkspace: (workspacePath, cwd) => cwd.startsWith(workspacePath),
      });

      expect(result).toEqual([
        { port: '2222', workspacePath: '/test/workspace' },
        { port: '3333', workspacePath: '/workspace/another' },
      ]);
    });
  });
});
