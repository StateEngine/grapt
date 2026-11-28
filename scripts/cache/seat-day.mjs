#!/usr/bin/env node
// No provider calls or service startup: index sessions read-only, export through Product.
import { execFile } from 'node:child_process';
import { lstat, readFile } from 'node:fs/promises';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { pathToFileURL } from 'node:url';
import { parseArgs, promisify } from 'node:util';
import { evaluateReleaseGates, summarize } from './seat-day-report.mjs';
import { readEvents } from './seat-day-events.mjs';
import { artifactReader } from './seat-day-artifacts.mjs';
import { operationalReport } from './seat-day-operations.mjs';
import { codexReport } from './seat-day-codex.mjs';

const execute = promisify(execFile);
const MAX_SESSIONS = 256;

// This is only a version-independent event index. Product owns usage decoding,
// reconciliation, compaction classification and exact-model pricing.
export function sessionsInWindow(database, window) {
  const db = new DatabaseSync(database, { readOnly: true, enableExtensions: false, timeout: 0 });
  try {
    const rows = db.prepare(`SELECT DISTINCT
      CASE WHEN length(CAST(aggregate_id AS BLOB)) <= 128 THEN aggregate_id END AS aggregate_id FROM event
      WHERE aggregate_family = 'session' AND created >= ? AND created < ?
      ORDER BY aggregate_id LIMIT ?`).all(window.from, window.until, MAX_SESSIONS + 1);
    if (rows.length > MAX_SESSIONS) throw new Error('Window exceeds 256 sessions; narrow it');
    return rows.map(row => row.aggregate_id);
  } finally { db.close(); }
}

export async function run(args) {
  const { values } = parseArgs({ args, options: {
    bundle: { type: 'string' }, service: { type: 'string' }, session: { type: 'string' },
    from: { type: 'string' }, until: { type: 'string' }, prices: { type: 'string' },
    compare: { type: 'string' },
    'codex-rollout': { type: 'string', multiple: true },
  } });
  if (!!values.from !== !!values.until) throw new Error('Supply both --from and --until');
  const timestamp = text => {
    if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{3})?Z$/.test(text)
        || !Number.isSafeInteger(Date.parse(text))
        || new Date(Date.parse(text)).toISOString().slice(0, 19) !== text.slice(0, 19)) throw new Error('Window requires ISO UTC timestamps');
    return Date.parse(text);
  };
  const window = values.from ? { from: timestamp(values.from), until: timestamp(values.until) } : null;
  if (window && window.from >= window.until) throw new Error('Window must have from < until');
  if (values['codex-rollout']) {
    if (values.bundle || values.service || values.session || values.prices) {
      throw new Error('--codex-rollout cannot be combined with Product profile options');
    }
    return codexReport(values['codex-rollout'], window);
  }
  if (!values.bundle || !values.service || (!!values.session === !!values.from)) {
    throw new Error('Use --bundle <package> --service <profile> with --session <id> OR --from <ISO UTC> --until <ISO UTC>, optionally --prices <policy.json>');
  }
  const service = path.resolve(values.service);
  const product = path.resolve(values.bundle, process.platform === 'win32' ? 'grapt-server.exe' : 'grapt-server');
  let sessions;
  if (values.session) sessions = [values.session];
  else {
    const database = path.join(service, 'database.sqlite3');
    if (!(await lstat(database)).isFile()) throw new Error('Accounting database is not a regular file');
    sessions = sessionsInWindow(database, window);
  }
  const reports = [];
  let exportedAttempts = 0;
  for (const session of sessions) {
    if (!/^ses_[A-Za-z0-9_-]{1,124}$/.test(session)) throw new Error('Invalid session identity');
    const command = ['usage', '--service', service, '--session', session];
    if (values.prices) command.push('--prices', path.resolve(values.prices));
    const { stdout } = await execute(product, command, { timeout: 30_000, maxBuffer: 16 * 1024 * 1024 + 1, windowsHide: true });
    const report = JSON.parse(stdout);
    if (report.session !== session) throw new Error('Accounting returned a different session');
    if (!Array.isArray(report.attempts)) throw new Error('Accounting attempts are unavailable');
    exportedAttempts += report.attempts.length;
    if (exportedAttempts > 8192) throw new Error('Export exceeds 8192 attempts; narrow the session set');
    reports.push(report);
  }
  const result = summarize(reports, window);
  const history = await readEvents(path.join(service, 'database.sqlite3'), sessions);
  result.operations = await operationalReport(history.events, reports, artifactReader(service), window);
  result.operations.coverage = history.coverage;
  let comparison = null;
  if (values.compare) {
    const comparePath = path.resolve(values.compare);
    const stat = await lstat(comparePath);
    if (!stat.isFile() || stat.size > 1024 * 1024) {
      throw new Error('Comparison evidence must be a regular file under 1 MiB');
    }
    comparison = JSON.parse(await readFile(comparePath, 'utf8'));
  }
  result.release_gates = evaluateReleaseGates(result, comparison);
  if (Buffer.byteLength(JSON.stringify(result)) > 32 * 1024 * 1024) {
    throw new Error('Report exceeds 32 MiB; narrow the window');
  }
  return result;
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try { console.log(JSON.stringify(await run(process.argv.slice(2)), null, 2)); }
  catch (error) { console.error(`Seat-day measurement refused: ${error.message}`); process.exitCode = 2; }
}
