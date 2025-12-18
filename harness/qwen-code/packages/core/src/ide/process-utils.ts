/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { exec, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import os from 'node:os';
import path from 'node:path';

const execAsync = promisify(exec);
const execFileAsync = promisify(execFile);

const MAX_TRAVERSAL_DEPTH = 32;

/**
 * Fetches the parent process ID, name, and command for a given process ID.
 *
 * @param pid The process ID to inspect.
 * @returns A promise that resolves to the parent's PID, name, and command.
 */
async function getProcessInfo(pid: number): Promise<{
  parentPid: number;
  name: string;
  command: string;
}> {
  try {
    const platform = os.platform();
    if (platform === 'win32') {
      const powershellCommand = [
        '$p = Get-CimInstance Win32_Process',
        `-Filter 'ProcessId=${pid}'`,
        '-ErrorAction SilentlyContinue;',
        'if ($p) {',
        '@{Name=$p.Name;ParentProcessId=$p.ParentProcessId;CommandLine=$p.CommandLine}',
        '| ConvertTo-Json',
        '}',
      ].join(' ');

      const { stdout } = await execFileAsync('powershell', [
        '-NoProfile',
        '-NonInteractive',
        '-Command',
        powershellCommand,
      ]);
      const output = stdout.trim();
      if (!output) return { parentPid: 0, name: '', command: '' };
      const {
        Name = '',
        ParentProcessId = 0,
        CommandLine = '',
      } = JSON.parse(output);
      return {
        parentPid: ParentProcessId,
        name: Name,
        command: CommandLine ?? '',
      };
    } else {
      const command = `ps -o ppid=,command= -p ${pid}`;
      const { stdout } = await execAsync(command);
      const trimmedStdout = stdout.trim();
      if (!trimmedStdout) {
        return { parentPid: 0, name: '', command: '' };
      }
      const ppidString = trimmedStdout.split(/\s+/)[0];
      const parentPid = parseInt(ppidString, 10);
      const fullCommand = trimmedStdout.substring(ppidString.length).trim();
      const processName = path.basename(fullCommand.split(' ')[0]);
      return {
        parentPid: isNaN(parentPid) ? 1 : parentPid,
        name: processName,
        command: fullCommand,
      };
    }
  } catch (_e) {
    console.debug(`Failed to get process info for pid ${pid}:`, _e);
    return { parentPid: 0, name: '', command: '' };
  }
}

/**
 * Finds the IDE process info on Unix-like systems.
 *
 * The strategy is to find the shell process that spawned the CLI, and then
 * find that shell's parent process (the IDE). To get the true IDE process,
 * we traverse one level higher to get the grandparent.
 *
 * @returns A promise that resolves to the PID and command of the IDE process.
 */
async function getIdeProcessInfoForUnix(): Promise<{
  pid: number;
  command: string;
}> {
  const shells = ['zsh', 'bash', 'sh', 'tcsh', 'csh', 'ksh', 'fish', 'dash'];
  let currentPid = process.pid;

  for (let i = 0; i < MAX_TRAVERSAL_DEPTH; i++) {
    try {
      const { parentPid, name } = await getProcessInfo(currentPid);

      const isShell = shells.some((shell) => name === shell);
      if (isShell) {
        // The direct parent of the shell is often a utility process (e.g. VS
        // Code's `ptyhost` process). To get the true IDE process, we need to
        // traverse one level higher to get the grandparent.
        let idePid = parentPid;
        try {
          const { parentPid: grandParentPid } = await getProcessInfo(parentPid);
          if (grandParentPid > 1) {
            idePid = grandParentPid;
          }
        } catch {
          // Ignore if getting grandparent fails, we'll just use the parent pid.
        }
        const { command: ideCommand } = await getProcessInfo(idePid);
        return { pid: idePid, command: ideCommand };
      }

      if (parentPid <= 1) {
        break; // Reached the root
      }
      currentPid = parentPid;
    } catch (_e) {
      // Process in chain died
      break;
    }
  }

  const { command } = await getProcessInfo(currentPid);
  return { pid: currentPid, command };
}

interface ProcessInfo {
  pid: number;
  parentPid: number;
  name: string;
  command: string;
}

interface RawProcessInfo {
  ProcessId?: number;
  ParentProcessId?: number;
  Name?: string;
  CommandLine?: string;
}

/**
 * Fetches the entire process table on Windows.
 */
async function getProcessTableWindows(): Promise<Map<number, ProcessInfo>> {
  const processMap = new Map<number, ProcessInfo>();
  try {
    const powershellCommand =
      'Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,CommandLine | ConvertTo-Json -Compress';
    const { stdout } = await execFileAsync(
      'powershell',
      ['-NoProfile', '-NonInteractive', '-Command', powershellCommand],
      { maxBuffer: 10 * 1024 * 1024 },
    );

    if (!stdout.trim()) {
      return processMap;
    }

    let processes: RawProcessInfo | RawProcessInfo[];
    try {
      processes = JSON.parse(stdout);
    } catch (_e) {
      return processMap;
    }

    if (!Array.isArray(processes)) {
      processes = [processes];
    }

    for (const p of processes) {
      if (p && typeof p.ProcessId === 'number') {
        processMap.set(p.ProcessId, {
          pid: p.ProcessId,
          parentPid: p.ParentProcessId || 0,
          name: p.Name || '',
          command: p.CommandLine || '',
        });
      }
    }
  } catch (_e) {
    // Fallback or error handling if PowerShell fails
  }
  return processMap;
}

/**
 * Finds the IDE process info on Windows using a snapshot approach.
 *
 * The strategy is to find the IDE process by looking for known IDE executables
 * in the process chain, with fallback to heuristics.
 *
 * @returns A promise that resolves to the PID and command of the IDE process.
 */
async function getIdeProcessInfoForWindows(): Promise<{
  pid: number;
  command: string;
}> {
  // Fetch the entire process table in one go.
  const processMap = await getProcessTableWindows();

  const myPid = process.pid;
  const myProc = processMap.get(myPid);

  if (!myProc) {
    // Fallback: return current process info if snapshot fails
    return { pid: myPid, command: '' };
  }

  // Known IDE process names (lowercase for case-insensitive comparison)
  const ideProcessNames = [
    'code.exe', // VS Code
    'code - insiders.exe', // VS Code Insiders
    'cursor.exe', // Cursor
    'windsurf.exe', // Windsurf
    'devenv.exe', // Visual Studio
    'rider64.exe', // JetBrains Rider
    'idea64.exe', // IntelliJ IDEA
    'pycharm64.exe', // PyCharm
    'webstorm64.exe', // WebStorm
  ];

  // Perform tree traversal in memory
  const ancestors: ProcessInfo[] = [];
  let curr: ProcessInfo | undefined = myProc;

  for (let i = 0; i < MAX_TRAVERSAL_DEPTH && curr; i++) {
    ancestors.push(curr);

    if (curr.parentPid === 0 || !processMap.has(curr.parentPid)) {
      // Try to get info about the missing parent
      if (curr.parentPid !== 0) {
        try {
          const parentInfo = await getProcessInfo(curr.parentPid);
          if (parentInfo.name) {
            ancestors.push({
              pid: curr.parentPid,
              parentPid: parentInfo.parentPid,
              name: parentInfo.name,
              command: parentInfo.command,
            });
          }
        } catch (_e) {
          // Ignore if query fails
        }
      }
      break;
    }
    curr = processMap.get(curr.parentPid);
  }

  // Strategy 1: Look for known IDE process names in the chain
  for (let i = ancestors.length - 1; i >= 0; i--) {
    const proc = ancestors[i];
    const nameLower = proc.name.toLowerCase();

    if (
      ideProcessNames.some((ideName) => nameLower === ideName.toLowerCase())
    ) {
      return { pid: proc.pid, command: proc.command };
    }
  }

  // Strategy 2: Special handling for Git Bash (sh.exe/bash.exe) with missing parent
  // Check this first before general shell handling
  const gitBashNames = ['sh.exe', 'bash.exe'];
  const gitBashProc = ancestors.find((p) =>
    gitBashNames.some((name) => p.name.toLowerCase() === name.toLowerCase()),
  );

  if (gitBashProc) {
    // Check if parent exists in process table
    const parentExists =
      gitBashProc.parentPid !== 0 && processMap.has(gitBashProc.parentPid);

    if (!parentExists && gitBashProc.parentPid !== 0) {
      // Look for IDE processes in the entire process table
      const ideProcesses: ProcessInfo[] = [];
      for (const [, proc] of processMap) {
        const nameLower = proc.name.toLowerCase();
        if (
          ideProcessNames.some((ideName) => nameLower === ideName.toLowerCase())
        ) {
          ideProcesses.push(proc);
        }
      }

      if (ideProcesses.length > 0) {
        // Prefer main process (without --type= parameter) over utility processes
        const mainProcesses = ideProcesses.filter(
          (p) => !p.command.includes('--type='),
        );
        const targetProcesses =
          mainProcesses.length > 0 ? mainProcesses : ideProcesses;

        // Sort by PID and pick the one with lowest PID
        targetProcesses.sort((a, b) => a.pid - b.pid);
        return {
          pid: targetProcesses[0].pid,
          command: targetProcesses[0].command,
        };
      }
    } else if (parentExists) {
      // Git Bash parent exists, use it
      const gitBashIndex = ancestors.indexOf(gitBashProc);
      if (gitBashIndex >= 0 && gitBashIndex + 1 < ancestors.length) {
        const parentProc = ancestors[gitBashIndex + 1];
        return { pid: parentProc.pid, command: parentProc.command };
      }
    }
  }

  // Strategy 3: Look for other shell processes (cmd.exe, powershell.exe, etc.) and use their parent
  const otherShellNames = ['cmd.exe', 'powershell.exe', 'pwsh.exe'];
  for (let i = 0; i < ancestors.length; i++) {
    const proc = ancestors[i];
    const nameLower = proc.name.toLowerCase();

    if (otherShellNames.some((shell) => nameLower === shell.toLowerCase())) {
      // The parent of the shell is likely closer to the IDE
      if (i + 1 < ancestors.length) {
        const parentProc = ancestors[i + 1];
        return { pid: parentProc.pid, command: parentProc.command };
      }
      break;
    }
  }

  // Strategy 4: Use ancestors[length-3] as fallback (original logic)
  if (ancestors.length >= 3) {
    const target = ancestors[ancestors.length - 3];
    return { pid: target.pid, command: target.command };
  } else if (ancestors.length > 0) {
    const target = ancestors[ancestors.length - 1];
    return { pid: target.pid, command: target.command };
  }

  return { pid: myPid, command: myProc.command };
}

/**
 * Traverses up the process tree to find the process ID and command of the IDE.
 *
 * This function uses different strategies depending on the operating system
 * to identify the main application process (e.g., the main VS Code window
 * process).
 *
 * If the IDE process cannot be reliably identified, it will return the
 * top-level ancestor process ID and command as a fallback.
 *
 * @returns A promise that resolves to the PID and command of the IDE process.
 */
export async function getIdeProcessInfo(): Promise<{
  pid: number;
  command: string;
}> {
  const platform = os.platform();

  if (platform === 'win32') {
    return getIdeProcessInfoForWindows();
  }

  return getIdeProcessInfoForUnix();
}
