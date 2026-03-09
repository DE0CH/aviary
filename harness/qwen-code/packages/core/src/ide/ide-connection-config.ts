/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import * as fs from 'node:fs';
import * as path from 'node:path';
import type { IdeInfo } from './detect-ide.js';
import { createDebugLogger } from '../utils/debugLogger.js';

const debugLogger = createDebugLogger('IDE');
const STALE_LOCK_MAX_AGE_MS = 7 * 24 * 60 * 60 * 1000;

export type StdioConfig = {
  command: string;
  args: string[];
};

export type ConnectionConfig = {
  port?: string;
  authToken?: string;
  stdio?: StdioConfig;
};

export type IdeConnectionConfig = ConnectionConfig & {
  workspacePath?: string;
  ideInfo?: IdeInfo;
  ppid?: number;
};

type ParsedConnectionLockFile = {
  file: string;
  fullPath: string;
  mtimeMs: number;
  parsed: IdeConnectionConfig;
};

export async function readConnectionConfigFromLockFile(
  ideDir: string,
  port: string,
): Promise<IdeConnectionConfig | undefined> {
  try {
    const lockFile = path.join(ideDir, `${port}.lock`);
    const lockFileContents = await fs.promises.readFile(lockFile, 'utf8');
    return JSON.parse(lockFileContents) as IdeConnectionConfig;
  } catch {
    return undefined;
  }
}

export async function getAllConnectionConfigs(
  ideDir: string,
): Promise<IdeConnectionConfig[]> {
  const parsedLockFiles = await getParsedConnectionLockFiles(ideDir);
  const activeLockFiles = await filterActiveLockFiles(parsedLockFiles);

  return activeLockFiles
    .sort((a, b) => b.mtimeMs - a.mtimeMs)
    .map(({ parsed }) => parsed);
}

export function getWorkspaceMatchingConnectionConfig(
  configs: IdeConnectionConfig[],
  cwd: string,
  matchesWorkspace: (workspacePath: string, cwd: string) => boolean,
): IdeConnectionConfig | undefined {
  return configs.find(
    (config) =>
      config.workspacePath !== undefined &&
      matchesWorkspace(config.workspacePath, cwd),
  );
}

export function getFallbackConnectionConfigs(
  configs: IdeConnectionConfig[],
  {
    cwd,
    currentPort,
    matchesWorkspace,
  }: {
    cwd: string;
    currentPort?: string;
    matchesWorkspace: (workspacePath: string, cwd: string) => boolean;
  },
): IdeConnectionConfig[] {
  const candidates = configs.filter(
    (config) => config.port !== undefined && config.port !== currentPort,
  );

  const workspaceMatches = candidates.filter(
    (config) =>
      config.workspacePath !== undefined &&
      matchesWorkspace(config.workspacePath, cwd),
  );
  const matchedPorts = new Set(workspaceMatches.map((config) => config.port));

  return [
    ...workspaceMatches,
    ...candidates.filter((config) => !matchedPorts.has(config.port)),
  ];
}

async function getParsedConnectionLockFiles(
  ideDir: string,
): Promise<ParsedConnectionLockFile[]> {
  const fileRegex = /^\d+\.lock$/;
  let lockFiles: string[];
  try {
    lockFiles = (await fs.promises.readdir(ideDir))
      .map((file) => file.toString())
      .filter((file) => fileRegex.test(file));
  } catch (error) {
    debugLogger.debug('Failed to read IDE connection directory:', error);
    return [];
  }

  const parsedLockFiles = await Promise.all(
    lockFiles.map(async (file) => {
      const fullPath = path.join(ideDir, file);
      try {
        const stat = await fs.promises.stat(fullPath);
        const content = await fs.promises.readFile(fullPath, 'utf8');
        try {
          return {
            file,
            fullPath,
            mtimeMs: stat.mtimeMs,
            parsed: JSON.parse(content) as IdeConnectionConfig,
          };
        } catch (error) {
          debugLogger.debug('Failed to parse JSON from lock file: ', error);
          return undefined;
        }
      } catch (error) {
        debugLogger.debug('Failed to read/stat IDE lock file:', error);
        return undefined;
      }
    }),
  );

  return parsedLockFiles.filter(
    (lockFile): lockFile is ParsedConnectionLockFile => lockFile !== undefined,
  );
}

async function filterActiveLockFiles(
  lockFiles: ParsedConnectionLockFile[],
): Promise<ParsedConnectionLockFile[]> {
  const activeResults = await Promise.all(
    lockFiles.map(async (lockFile) => ({
      lockFile,
      isStale: await cleanupStaleLockFile(lockFile),
    })),
  );

  const staleCount = activeResults.filter(({ isStale }) => isStale).length;
  if (staleCount > 0) {
    debugLogger.debug(
      `[cleanupStaleLockFiles] Cleaned up ${staleCount} stale lock file(s)`,
    );
  }

  return activeResults
    .filter(({ isStale }) => !isStale)
    .map(({ lockFile }) => lockFile);
}

async function cleanupStaleLockFile({
  file,
  fullPath,
  mtimeMs,
  parsed,
}: ParsedConnectionLockFile): Promise<boolean> {
  try {
    if (parsed.ppid) {
      try {
        process.kill(parsed.ppid, 0);
        return false;
      } catch {
        debugLogger.debug(
          `[cleanupStaleLockFiles] Removing lock file "${file}" - ppid ${parsed.ppid} no longer exists`,
        );
        await fs.promises.unlink(fullPath);
        return true;
      }
    }

    if (parsed.workspacePath) {
      if (fs.existsSync(parsed.workspacePath)) {
        return false;
      }

      debugLogger.debug(
        `[cleanupStaleLockFiles] Removing lock file "${file}" - workspace doesn't exist`,
      );
      await fs.promises.unlink(fullPath);
      return true;
    }

    if (Date.now() - mtimeMs > STALE_LOCK_MAX_AGE_MS) {
      debugLogger.debug(
        `[cleanupStaleLockFiles] Removing lock file "${file}" - older than 7 days`,
      );
      await fs.promises.unlink(fullPath);
      return true;
    }

    return false;
  } catch (error) {
    debugLogger.debug(
      `[cleanupStaleLockFiles] Error checking lock file "${file}":`,
      error,
    );
    return false;
  }
}
