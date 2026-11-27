// Copyright 2025 OpenAI. Licensed under the Apache License, Version 2.0.
// Modified by Grapt: Node process ownership and bounded exit confirmation (grace, group kill, then join).
import { execFile, spawn } from 'node:child_process';
import path from 'node:path';
import { promisify } from 'node:util';

const execute = promisify(execFile);
const EXPIRED = Symbol('expired');

export function spawnOwned(program, args, options = {}) {
  return spawn(program, args, { ...options, stdio: ['pipe', 'pipe', 'pipe'],
    detached: process.platform !== 'win32', windowsHide: true });
}

async function waitFor(exit, milliseconds) {
  let timer;
  try {
    return await Promise.race([exit, new Promise(resolve => {
      timer = setTimeout(() => resolve(EXPIRED), milliseconds);
    })]);
  } finally { clearTimeout(timer); }
}

function systemProgram(...parts) {
  if (!process.env.SystemRoot) throw new Error('Windows system directory is unavailable');
  return path.join(process.env.SystemRoot, 'System32', ...parts);
}

async function processes() {
  let result;
  if (process.platform === 'win32') {
    const script = 'Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId | ConvertTo-Json -Compress';
    const { stdout } = await execute(systemProgram('WindowsPowerShell', 'v1.0', 'powershell.exe'),
      ['-NoProfile', '-NonInteractive', '-Command', script], { timeout: 5000, maxBuffer: 2 * 1024 * 1024, windowsHide: true });
    const rows = JSON.parse(stdout);
    result = (Array.isArray(rows) ? rows : [rows]).map(row => ({ id: row.ProcessId, parent: row.ParentProcessId }));
  } else {
    const { stdout } = await execute('ps', ['-eo', 'pid=,ppid=,pgid=,stat='],
      { timeout: 5000, maxBuffer: 2 * 1024 * 1024 });
    result = stdout.trim().split('\n').map(line => {
      const [id, parent, group, state] = line.trim().split(/\s+/);
      return { id: Number(id), parent: Number(parent), group: Number(group), zombie: state.startsWith('Z') };
    });
  }
  if (result.length > 16_384 || result.some(row => !Number.isSafeInteger(row.id) || !Number.isSafeInteger(row.parent))) {
    throw new Error('Process observation is invalid or excessive');
  }
  return result;
}

function descendants(rows, root) {
  const children = new Map();
  for (const row of rows) {
    if (!children.has(row.parent)) children.set(row.parent, []);
    children.get(row.parent).push(row.id);
  }
  const owned = new Set([root]);
  for (const id of owned) {
    for (const child of children.get(id) ?? []) owned.add(child);
    if (owned.size > 256) throw new Error('Owned process tree exceeds the cleanup bound');
  }
  return owned;
}

// The existing ACP owner has two ten-second Product cleanup windows. The kit
// allows both plus margin. Tests may narrow this allowance for forced cleanup.
export async function stopOwned(child, closed, grace = 25_000) {
  child.stdin.end();
  const result = await waitFor(closed, grace);
  if (result !== EXPIRED) {
    if (result !== 0) throw new Error('ACP exited unsuccessfully');
    return;
  }
  let observed;
  try { observed = descendants(await processes(), child.pid); } catch { /* Refuse confirmation below. */ }
  let killed = true;
  if (process.platform !== 'win32' || (child.exitCode === null && child.signalCode === null)) {
    try {
      if (process.platform === 'win32') {
        await execute(systemProgram('taskkill.exe'), ['/PID', String(child.pid), '/T', '/F'],
          { timeout: 5000, maxBuffer: 65536, windowsHide: true });
      } else {
        process.kill(-child.pid, 'SIGKILL');
      }
    } catch (error) { killed = process.platform !== 'win32' && error.code === 'ESRCH'; }
  }
  if (await waitFor(closed, 10_000) === EXPIRED) {
    throw new Error('Owned ACP did not exit; process cleanup is unconfirmed');
  }
  let remaining;
  try {
    remaining = (await processes()).filter(row => !row.zombie
      && (observed?.has(row.id) || row.group === child.pid));
  } catch { /* An unreadable process list cannot prove shutdown. */ }
  if (!killed || !observed || !remaining || remaining.length !== 0) {
    throw new Error('Owned process tree cleanup is unconfirmed');
  }
  throw new Error('ACP required forced cleanup; owned process tree stopped');
}
