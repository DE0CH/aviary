/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import {
  detectIde,
  getCloudIdeEnvironmentLabels,
  IDE_DEFINITIONS,
  isCloudIdeRuntime,
} from './detect-ide.js';

describe('detectIde', () => {
  const ideProcessInfo = { pid: 123, command: 'some/path/to/code' };
  const ideProcessInfoNoCode = { pid: 123, command: 'some/path/to/fork' };

  // Clear all IDE-related environment variables before each test
  beforeEach(() => {
    vi.stubEnv('__COG_BASHRC_SOURCED', '');
    vi.stubEnv('REPLIT_USER', '');
    vi.stubEnv('CURSOR_TRACE_ID', '');
    vi.stubEnv('CODESPACES', '');
    vi.stubEnv('EDITOR_IN_CLOUD_SHELL', '');
    vi.stubEnv('CLOUD_SHELL', '');
    vi.stubEnv('DEVCONTAINER', '');
    vi.stubEnv('TERM_PRODUCT', '');
    vi.stubEnv('MONOSPACE_ENV', '');
    vi.stubEnv('TERM_PROGRAM', '');
  });

  afterEach(() => {
    vi.unstubAllEnvs();
  });

  it('should return undefined if TERM_PROGRAM is not vscode', () => {
    vi.stubEnv('TERM_PROGRAM', '');
    expect(detectIde(ideProcessInfo)).toBeUndefined();
  });

  it('should detect Devin', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('__COG_BASHRC_SOURCED', '1');
    expect(detectIde(ideProcessInfo)).toBe(IDE_DEFINITIONS.devin);
  });

  it('should detect Replit', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('REPLIT_USER', 'testuser');
    expect(detectIde(ideProcessInfo)).toBe(IDE_DEFINITIONS.replit);
  });

  it('should detect Cursor', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('CURSOR_TRACE_ID', 'some-id');
    expect(detectIde(ideProcessInfo)).toBe(IDE_DEFINITIONS.cursor);
  });

  it('should detect Codespaces', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('CODESPACES', 'true');
    expect(detectIde(ideProcessInfo)).toBe(IDE_DEFINITIONS.codespaces);
  });

  it('should detect Codespaces when env value is 1', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('CODESPACES', '1');
    expect(detectIde(ideProcessInfo)).toBe(IDE_DEFINITIONS.codespaces);
  });

  it('should detect Cloud Shell via EDITOR_IN_CLOUD_SHELL', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('EDITOR_IN_CLOUD_SHELL', 'true');
    expect(detectIde(ideProcessInfo)).toBe(IDE_DEFINITIONS.cloudshell);
  });

  it('should detect Cloud Shell via CLOUD_SHELL', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('CLOUD_SHELL', 'true');
    expect(detectIde(ideProcessInfo)).toBe(IDE_DEFINITIONS.cloudshell);
  });

  it('should detect Trae', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('TERM_PRODUCT', 'Trae');
    expect(detectIde(ideProcessInfo)).toBe(IDE_DEFINITIONS.trae);
  });

  it('should detect Firebase Studio via MONOSPACE_ENV', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('MONOSPACE_ENV', 'true');
    expect(detectIde(ideProcessInfo)).toBe(IDE_DEFINITIONS.firebasestudio);
  });

  it('should detect VSCode when no other IDE is detected and command includes "code"', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('MONOSPACE_ENV', '');
    expect(detectIde(ideProcessInfo)).toBe(IDE_DEFINITIONS.vscode);
  });

  it('should detect VSCodeFork when no other IDE is detected and command does not include "code"', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('MONOSPACE_ENV', '');
    expect(detectIde(ideProcessInfoNoCode)).toBe(IDE_DEFINITIONS.vscodefork);
  });

  it('should return undefined in a dev container when TERM_PROGRAM is not vscode', () => {
    vi.stubEnv('DEVCONTAINER', 'true');
    vi.stubEnv('TERM_PROGRAM', '');
    expect(detectIde(ideProcessInfo)).toBeUndefined();
  });

  it('should detect VSCode in a dev container when TERM_PROGRAM is vscode', () => {
    vi.stubEnv('DEVCONTAINER', 'true');
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    expect(detectIde(ideProcessInfo)).toBe(IDE_DEFINITIONS.vscode);
  });
});

describe('detectIde with ideInfoFromFile', () => {
  const ideProcessInfo = { pid: 123, command: 'some/path/to/code' };

  beforeEach(() => {
    vi.stubEnv('__COG_BASHRC_SOURCED', '');
    vi.stubEnv('REPLIT_USER', '');
    vi.stubEnv('CURSOR_TRACE_ID', '');
    vi.stubEnv('CODESPACES', '');
    vi.stubEnv('EDITOR_IN_CLOUD_SHELL', '');
    vi.stubEnv('CLOUD_SHELL', '');
    vi.stubEnv('DEVCONTAINER', '');
    vi.stubEnv('TERM_PRODUCT', '');
    vi.stubEnv('MONOSPACE_ENV', '');
    vi.stubEnv('TERM_PROGRAM', '');
  });

  afterEach(() => {
    vi.unstubAllEnvs();
  });

  it('should use the name and displayName from the file', () => {
    const ideInfoFromFile = {
      name: 'custom-ide',
      displayName: 'Custom IDE',
    };
    expect(detectIde(ideProcessInfo, ideInfoFromFile)).toEqual(ideInfoFromFile);
  });

  it('should fall back to env detection if name is missing', () => {
    const ideInfoFromFile = { displayName: 'Custom IDE' };
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    expect(detectIde(ideProcessInfo, ideInfoFromFile)).toBe(
      IDE_DEFINITIONS.vscode,
    );
  });

  it('should fall back to env detection if displayName is missing', () => {
    const ideInfoFromFile = { name: 'custom-ide' };
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    expect(detectIde(ideProcessInfo, ideInfoFromFile)).toBe(
      IDE_DEFINITIONS.vscode,
    );
  });
});

describe('cloud IDE runtime helpers', () => {
  beforeEach(() => {
    vi.stubEnv('CODESPACES', '');
    vi.stubEnv('CLOUD_SHELL', '');
    vi.stubEnv('EDITOR_IN_CLOUD_SHELL', '');
    vi.stubEnv('DEVCONTAINER', '');
  });

  afterEach(() => {
    vi.unstubAllEnvs();
  });

  it('should return false when all cloud runtime env vars are disabled', () => {
    vi.stubEnv('CODESPACES', '0');
    vi.stubEnv('CLOUD_SHELL', 'false');
    vi.stubEnv('EDITOR_IN_CLOUD_SHELL', '');
    vi.stubEnv('DEVCONTAINER', '');

    expect(isCloudIdeRuntime()).toBe(false);
    expect(getCloudIdeEnvironmentLabels()).toEqual([]);
  });

  it('should return cloud labels from enabled env vars', () => {
    vi.stubEnv('CODESPACES', '1');
    vi.stubEnv('CLOUD_SHELL', 'true');

    expect(isCloudIdeRuntime()).toBe(true);
    expect(getCloudIdeEnvironmentLabels()).toEqual([
      'GitHub Codespaces',
      'Cloud Shell',
    ]);
  });

  it('should deduplicate Cloud Shell label', () => {
    vi.stubEnv('CLOUD_SHELL', '1');
    vi.stubEnv('EDITOR_IN_CLOUD_SHELL', '1');

    expect(getCloudIdeEnvironmentLabels()).toEqual(['Cloud Shell']);
  });
});
