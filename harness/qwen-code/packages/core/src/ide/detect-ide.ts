/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

export const IDE_DEFINITIONS = {
  devin: { name: 'devin', displayName: 'Devin' },
  replit: { name: 'replit', displayName: 'Replit' },
  cursor: { name: 'cursor', displayName: 'Cursor' },
  cloudshell: { name: 'cloudshell', displayName: 'Cloud Shell' },
  codespaces: { name: 'codespaces', displayName: 'GitHub Codespaces' },
  firebasestudio: { name: 'firebasestudio', displayName: 'Firebase Studio' },
  trae: { name: 'trae', displayName: 'Trae' },
  vscode: { name: 'vscode', displayName: 'VS Code' },
  vscodefork: { name: 'vscodefork', displayName: 'IDE' },
} as const;

export const IDE_SERVER_HOSTS = {
  local: '127.0.0.1',
  container: 'host.docker.internal',
} as const;

const CLOUD_IDE_RUNTIME_ENV_LABELS = [
  { key: 'CODESPACES', label: 'GitHub Codespaces' },
  { key: 'CLOUD_SHELL', label: 'Cloud Shell' },
  { key: 'EDITOR_IN_CLOUD_SHELL', label: 'Cloud Shell' },
  { key: 'DEVCONTAINER', label: 'Dev Container' },
] as const;

export interface IdeInfo {
  name: string;
  displayName: string;
}

function isEnabledEnvVar(name: string): boolean {
  const value = process.env[name];
  return (
    value !== undefined && value !== '' && value !== 'false' && value !== '0'
  );
}

export function isCloudShell(): boolean {
  return (
    isEnabledEnvVar('EDITOR_IN_CLOUD_SHELL') || isEnabledEnvVar('CLOUD_SHELL')
  );
}

export function isCloudIdeRuntime(): boolean {
  return CLOUD_IDE_RUNTIME_ENV_LABELS.some((env) => isEnabledEnvVar(env.key));
}

export function getCloudIdeEnvironmentLabels(): string[] {
  const labels = new Set<string>();
  for (const env of CLOUD_IDE_RUNTIME_ENV_LABELS) {
    if (isEnabledEnvVar(env.key)) {
      labels.add(env.label);
    }
  }
  return [...labels];
}

export function detectIdeFromEnv(): IdeInfo {
  if (process.env['__COG_BASHRC_SOURCED']) {
    return IDE_DEFINITIONS.devin;
  }
  if (process.env['REPLIT_USER']) {
    return IDE_DEFINITIONS.replit;
  }
  if (process.env['CURSOR_TRACE_ID']) {
    return IDE_DEFINITIONS.cursor;
  }
  if (isEnabledEnvVar('CODESPACES')) {
    return IDE_DEFINITIONS.codespaces;
  }
  if (isCloudShell()) {
    return IDE_DEFINITIONS.cloudshell;
  }
  if (process.env['TERM_PRODUCT'] === 'Trae') {
    return IDE_DEFINITIONS.trae;
  }
  if (process.env['MONOSPACE_ENV']) {
    return IDE_DEFINITIONS.firebasestudio;
  }
  return IDE_DEFINITIONS.vscode;
}

function verifyVSCode(
  ide: IdeInfo,
  ideProcessInfo: {
    pid: number;
    command: string;
  },
): IdeInfo {
  if (ide.name !== IDE_DEFINITIONS.vscode.name) {
    return ide;
  }
  if (
    ideProcessInfo.command &&
    ideProcessInfo.command.toLowerCase().includes('code')
  ) {
    return IDE_DEFINITIONS.vscode;
  }
  return IDE_DEFINITIONS.vscodefork;
}

export function detectIde(
  ideProcessInfo: {
    pid: number;
    command: string;
  },
  ideInfoFromFile?: { name?: string; displayName?: string },
): IdeInfo | undefined {
  if (ideInfoFromFile?.name && ideInfoFromFile.displayName) {
    return {
      name: ideInfoFromFile.name,
      displayName: ideInfoFromFile.displayName,
    };
  }

  if (isEnabledEnvVar('CODESPACES')) {
    return IDE_DEFINITIONS.codespaces;
  }
  if (isCloudShell()) {
    return IDE_DEFINITIONS.cloudshell;
  }
  if (isEnabledEnvVar('DEVCONTAINER')) {
    // Dev container could be VS Code based
    if (process.env['TERM_PROGRAM'] === 'vscode') {
      const ide = detectIdeFromEnv();
      return verifyVSCode(ide, ideProcessInfo);
    }
    return undefined;
  }

  // Only VSCode-based integrations are currently supported.
  if (process.env['TERM_PROGRAM'] !== 'vscode') {
    return undefined;
  }

  const ide = detectIdeFromEnv();
  return verifyVSCode(ide, ideProcessInfo);
}
