/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import { mkdir, readdir, rm } from 'node:fs/promises';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const rootDir = join(__dirname, '../..');
const e2eTestsDir = join(rootDir, '.integration-tests');
let runDir = '';

export async function setup() {
  runDir = join(e2eTestsDir, `sdk-e2e-${Date.now()}`);
  await mkdir(runDir, { recursive: true });

  // Clean up old test runs, but keep the latest few for debugging
  try {
    const testRuns = await readdir(e2eTestsDir);
    const sdkTestRuns = testRuns.filter((run) => run.startsWith('sdk-e2e-'));
    if (sdkTestRuns.length > 5) {
      const oldRuns = sdkTestRuns.sort().slice(0, sdkTestRuns.length - 5);
      await Promise.all(
        oldRuns.map((oldRun) =>
          rm(join(e2eTestsDir, oldRun), {
            recursive: true,
            force: true,
          }),
        ),
      );
    }
  } catch (e) {
    console.error('Error cleaning up old test runs:', e);
  }

  process.env['E2E_TEST_FILE_DIR'] = runDir;
  process.env['QWEN_CLI_E2E_TEST'] = 'true';
  process.env['TEST_CLI_PATH'] = join(rootDir, '../../dist/cli.js');

  if (process.env['KEEP_OUTPUT']) {
    console.log(`Keeping output for test run in: ${runDir}`);
  }
  process.env['VERBOSE'] = process.env['VERBOSE'] ?? 'false';

  console.log(`\nSDK E2E test output directory: ${runDir}`);
  console.log(`CLI path: ${process.env['TEST_CLI_PATH']}`);
}

export async function teardown() {
  // Cleanup the test run directory unless KEEP_OUTPUT is set
  if (process.env['KEEP_OUTPUT'] !== 'true' && runDir) {
    await rm(runDir, { recursive: true, force: true });
  }
}
