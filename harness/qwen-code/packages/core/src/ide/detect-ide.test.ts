/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { detectIde, IDE_DEFINITIONS } from './detect-ide.js';

describe('detectIde', () => {
  // Clear all IDE-related environment variables before each test
  beforeEach(() => {
    vi.stubEnv('__COG_BASHRC_SOURCED', '');
    vi.stubEnv('REPLIT_USER', '');
    vi.stubEnv('CURSOR_TRACE_ID', '');
    vi.stubEnv('CODESPACES', '');
    vi.stubEnv('EDITOR_IN_CLOUD_SHELL', '');
    vi.stubEnv('CLOUD_SHELL', '');
    vi.stubEnv('TERM_PRODUCT', '');
    vi.stubEnv('MONOSPACE_ENV', '');
  });

  afterEach(() => {
    vi.unstubAllEnvs();
  });

  it('should return undefined if TERM_PROGRAM is not vscode', () => {
    vi.stubEnv('TERM_PROGRAM', '');
    expect(detectIde()).toBeUndefined();
  });

  it('should detect Devin', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('__COG_BASHRC_SOURCED', '1');
    expect(detectIde()).toBe(IDE_DEFINITIONS.devin);
  });

  it('should detect Replit', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('REPLIT_USER', 'testuser');
    expect(detectIde()).toBe(IDE_DEFINITIONS.replit);
  });

  it('should detect Cursor', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('CURSOR_TRACE_ID', 'some-id');
    expect(detectIde()).toBe(IDE_DEFINITIONS.cursor);
  });

  it('should detect Codespaces', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('CODESPACES', 'true');
    expect(detectIde()).toBe(IDE_DEFINITIONS.codespaces);
  });

  it('should detect Cloud Shell via EDITOR_IN_CLOUD_SHELL', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('EDITOR_IN_CLOUD_SHELL', 'true');
    expect(detectIde()).toBe(IDE_DEFINITIONS.cloudshell);
  });

  it('should detect Cloud Shell via CLOUD_SHELL', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('CLOUD_SHELL', 'true');
    expect(detectIde()).toBe(IDE_DEFINITIONS.cloudshell);
  });

  it('should detect Trae', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('TERM_PRODUCT', 'Trae');
    expect(detectIde()).toBe(IDE_DEFINITIONS.trae);
  });

  it('should detect Firebase Studio via MONOSPACE_ENV', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('MONOSPACE_ENV', 'true');
    expect(detectIde()).toBe(IDE_DEFINITIONS.firebasestudio);
  });

  it('should detect VSCodeFork when no other IDE is detected and no process info provided', () => {
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    vi.stubEnv('MONOSPACE_ENV', '');
    expect(detectIde()).toBe(IDE_DEFINITIONS.vscodefork);
  });
});

describe('detectIde with ideInfoFromFile', () => {
  beforeEach(() => {
    vi.stubEnv('__COG_BASHRC_SOURCED', '');
    vi.stubEnv('REPLIT_USER', '');
    vi.stubEnv('CURSOR_TRACE_ID', '');
    vi.stubEnv('CODESPACES', '');
    vi.stubEnv('EDITOR_IN_CLOUD_SHELL', '');
    vi.stubEnv('CLOUD_SHELL', '');
    vi.stubEnv('TERM_PRODUCT', '');
    vi.stubEnv('MONOSPACE_ENV', '');
  });

  afterEach(() => {
    vi.unstubAllEnvs();
  });

  it('should use the name and displayName from the file', () => {
    const ideInfoFromFile = {
      name: 'custom-ide',
      displayName: 'Custom IDE',
    };
    expect(detectIde(undefined, ideInfoFromFile)).toEqual(ideInfoFromFile);
  });

  it('should fall back to env detection if name is missing', () => {
    const ideInfoFromFile = { displayName: 'Custom IDE' };
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    expect(detectIde(undefined, ideInfoFromFile)).toBe(
      IDE_DEFINITIONS.vscodefork,
    );
  });

  it('should fall back to env detection if displayName is missing', () => {
    const ideInfoFromFile = { name: 'custom-ide' };
    vi.stubEnv('TERM_PROGRAM', 'vscode');
    expect(detectIde(undefined, ideInfoFromFile)).toBe(
      IDE_DEFINITIONS.vscodefork,
    );
  });
});
