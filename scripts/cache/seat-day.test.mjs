import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, readFile, rm, mkdir } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { summarize } from './seat-day-report.mjs';
import { run, sessionsInWindow } from './seat-day.mjs';

const field = tokens => ({ status: 'observed', tokens, provenance: { kind: 'provider-reported' } });
function attempt(id, input, read, at = 100) {
  return { attempt: id, operation: 'assistant', provider: 'fixture', model: 'sol',
    latency: { opened_ms: at }, cost: { status: 'unavailable', reason: 'price-policy-unavailable' },
    usage: { input_total: field(input), cache_read: field(read) } };
}
function report(attempts, session = 'ses_test') {
  return { schema: 'secode.session-accounting.v1', session, enumeration_complete: true, session_totals: { status: 'complete' }, attempts };
}

test('read share is weighted and compaction/retries contribute once', () => {
  const compaction = { ...attempt('summary', 900, 810), operation: 'compaction', model: null };
  const result = summarize([report([attempt('first', 100, 0), compaction, attempt('retry', 1000, 900)])]);
  assert.equal(result.totals.read_share_percent, 85.5);
  assert.equal(result.totals.uncached_input_tokens, 290);
  assert.equal(result.totals.attempts, 3);
  assert.equal(result.models.length, 2);
  assert.equal(result.requests[0].miss_cause.status, 'unavailable');
  assert.throws(() => summarize([report([attempt('same', 1, 0), attempt('same', 1, 0)])]), /duplicated/);
});

test('absent usage and pricing stay unavailable while reported zero stays zero', () => {
  const absent = attempt('absent', 100, 0); delete absent.usage.cache_read;
  const missing = summarize([report([absent])]);
  assert.equal(missing.totals.read_share_percent, null);
  assert.equal(missing.totals.uncached_input_tokens, null);
  assert.equal(missing.totals.inclusive_input_tokens, 100);
  assert.equal(missing.totals.paired_input_tokens, null);
  assert.equal(missing.totals.field_unavailable_attempts.cache_read_tokens, 1);
  assert.equal(missing.totals.cost.status, 'unavailable');
  const zero = summarize([report([attempt('zero', 100, 0)])]);
  assert.equal(zero.totals.read_share_percent, 0);
  assert.equal(zero.totals.uncached_input_tokens, 100);
  assert.equal(summarize([]).totals.inclusive_input_tokens, null);
  assert.equal(summarize([report([attempt('empty', 0, 0)])]).totals.read_share_percent, null);
});

test('partial coverage and priced subtotals cannot claim a complete day', () => {
  const priced = attempt('priced', 100, 90);
  priced.cost = { status: 'estimated', usd: 0, policy: 'fixture-v1' };
  const partial = report([priced, attempt('unknown-price', 200, 100)]);
  partial.enumeration_complete = false;
  const result = summarize([partial]);
  assert.deepEqual(result.incomplete_sessions, ['ses_test']);
  assert.equal(result.totals.cost.usd, 0);
  assert.equal(result.totals.cost_unavailable_attempts, 1);
  assert.match(result.coverage, /observed-subtotals-only/);
});

test('window includes the opening boundary, excludes its end and retains unplaceable attempts', () => {
  const result = summarize([report([attempt('before', 1, 0, 99), attempt('inside', 100, 90, 100),
    attempt('end', 1, 0, 200), attempt('unknown', 1, 0, null)])], { from: 100, until: 200 });
  assert.equal(result.totals.attempts, 1);
  assert.deepEqual(result.window_unplaced_attempts, ['ses_test/unknown']);
});

test('a long producer session is accepted without dropping attempts', () => {
  const attempts = Array.from({ length: 2048 }, (_, index) => attempt(`day_${index}`, 100, 90));
  const result = summarize([report(attempts)]);
  assert.equal(result.totals.attempts, 2048);
  assert.equal(result.totals.inclusive_input_tokens, 204800);
  assert.equal(result.totals.uncached_input_tokens, 20480);
  assert.equal(result.totals.read_share_percent, 90);
});

test('invalid counts, cross-coordinate corruption and cost overflow refuse', () => {
  for (const value of [-1, 0.5, Number.MAX_SAFE_INTEGER + 1]) {
    assert.throws(() => summarize([report([attempt('invalid', value, 0)])]), /Invalid observed/);
  }
  assert.throws(() => summarize([report([attempt('invalid', 1, 2)])]), /exceeds/);
  const huge = attempt('huge', 0, 0); huge.cost = { status: 'estimated', usd: Infinity, policy: 'bad' };
  assert.throws(() => summarize([report([huge])]), /cost/);
  assert.throws(() => summarize([report([attempt('a', Number.MAX_SAFE_INTEGER, 0), attempt('b', 1, 0)])]), /sum/);
});

test('forked Physical Attempts contribute once and conflicting copies refuse', () => {
  const source = attempt('physical_one', 100, 90);
  const copied = structuredClone(source);
  source.latency.opening_event = 'evt_original'; copied.latency.opening_event = 'evt_reminted';
  const result = summarize([report([source], 'ses_parent'), report([copied], 'ses_fork')]);
  assert.equal(result.totals.attempts, 1);
  assert.equal(result.totals.inclusive_input_tokens, 100);
  assert.equal(result.copied_attempt_exports, 1);
  assert.deepEqual(result.requests[0].sessions, ['ses_parent', 'ses_fork']);
  copied.usage.cache_read.tokens = 80;
  assert.throws(() => summarize([report([source], 'ses_parent'), report([copied], 'ses_fork')]), /Conflicting Product exports/);
});

test('window boundaries and missing openings cannot hide conflicting Physical Attempt copies', () => {
  const source = attempt('same_physical', 100, 90, 100);
  for (const at of [200, null]) {
    const copied = { ...source, latency: { opened_ms: at } };
    for (const reports of [[report([source], 'ses_parent'), report([copied], 'ses_fork')],
      [report([copied], 'ses_fork'), report([source], 'ses_parent')]]) {
      assert.throws(() => summarize(reports, { from: 0, until: 150 }), /Conflicting Product exports/);
    }
  }
  const copied = structuredClone(source);
  const result = summarize([report([source], 'ses_parent'), report([copied], 'ses_fork')], { from: 150, until: 300 });
  assert.equal(result.totals.attempts, 0);
  assert.equal(result.copied_attempt_exports, 1);
});

test('window session enumeration reads the existing database without writes', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'grapt-seat-day-'));
  try {
    const file = path.join(root, 'database.sqlite3');
    const db = new DatabaseSync(file);
    db.exec('CREATE TABLE event (aggregate_family TEXT, aggregate_id TEXT, created INTEGER)');
    const insert = db.prepare('INSERT INTO event VALUES (?, ?, ?)');
    insert.run('session', 'ses_old', 99); insert.run('session', 'ses_open', 100);
    insert.run('session', 'ses_open', 101); insert.run('session', 'ses_end', 200);
    insert.run('other', 'ses_foreign', 150); db.close();
    const before = await readFile(file);
    assert.deepEqual(sessionsInWindow(file, { from: 100, until: 200 }), ['ses_open']);
    assert.deepEqual(await readFile(file), before);
    await assert.rejects(run(['--bundle', root, '--service', root, '--from', 'bad', '--until', 'bad']), /UTC/);
    await assert.rejects(run(['--bundle', '/absent', '--service', '/absent',
      '--from', '2026-02-30T00:00:00Z', '--until', '2026-03-03T00:00:00Z']), /UTC/);
    await assert.rejects(run(['--bundle', '/absent', '--service', '/absent',
      '--from', '2028-02-29T00:00:00Z', '--until', '2028-03-01T00:00:00.001Z']), /ENOENT|absent/);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('one command invokes the normal usage exporter with the explicit price policy', {
  skip: process.platform === 'win32' ? 'POSIX executable fixture; native Windows uses the packaged Product' : false,
}, async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'grapt-seat-day-cli-'));
  try {
    const bundle = path.join(root, 'package with spaces'); await mkdir(bundle);
    const body = JSON.stringify(report([attempt('live-recorded', 1000, 950)]));
    const executable = `#!${process.execPath}\nconst fs=require('node:fs');fs.writeFileSync(${JSON.stringify(path.join(root, 'args.json'))},JSON.stringify(process.argv.slice(2)));console.log(${JSON.stringify(body)});\n`;
    await writeFile(path.join(bundle, 'grapt-server'), executable, { mode: 0o700 });
    const db = new DatabaseSync(path.join(root, 'database.sqlite3'));
    db.exec('CREATE TABLE event (id TEXT, aggregate_family TEXT, aggregate_id TEXT, seq INTEGER, created INTEGER, type TEXT, data TEXT)');
    const insert = db.prepare('INSERT INTO event VALUES (?, ?, ?, ?, ?, ?, ?)');
    insert.run('evt_start', 'session', 'ses_test', 1, 100, 'session.tool.input.started.1', JSON.stringify({ callID: 'call_read', name: 'read' }));
    insert.run('evt_end', 'session', 'ses_test', 2, 125, 'session.tool.terminal.1', JSON.stringify({ call: 'call_read', attempt: 'live-recorded', outcome: { kind: 'completed' } }));
    db.close();
    const before = await readFile(path.join(root, 'database.sqlite3'));
    const result = await run(['--bundle', bundle, '--service', root, '--session', 'ses_test', '--prices', path.join(root, 'price policy.json')]);
    assert.equal(result.totals.read_share_percent, 95);
    assert.equal(result.operations.tool_calls.count, 1);
    assert.equal(result.operations.tool_calls.by_name_outcome[0].p95_ms, 25);
    assert.deepEqual(await readFile(path.join(root, 'database.sqlite3')), before);
    assert.deepEqual(JSON.parse(await readFile(path.join(root, 'args.json'), 'utf8')),
      ['usage', '--service', root, '--session', 'ses_test', '--prices', path.join(root, 'price policy.json')]);
  } finally { await rm(root, { recursive: true, force: true }); }
});

// Coverage is Product's dispatch reconciliation, separate from observed sums.
test('historical and pending dispatch coverage propagate to the day report', () => {
  for (const status of [undefined, 'incomplete']) {
    const value = report([attempt('known', 100, 90)]);
    value.session_totals = status ? { status, reason: 'historical-attempt-coverage-unavailable' } : undefined;
    const result = summarize([value]);
    assert.deepEqual(result.incomplete_sessions, ['ses_test']);
    assert.equal(result.attempt_reconciliation.status, 'incomplete');
    assert.equal(result.totals.inclusive_input_tokens, 100);
  }
  const missing = { ...attempt('unpublished', 0, 0), usage: {},
    usage_outcome: { status: 'unavailable', reason: 'provider-usage-not-recorded' } };
  const result = summarize([report([attempt('observed', 100, 90), missing])]);
  assert.equal(result.attempt_reconciliation.status, 'complete');
  assert.equal(result.totals.inclusive_input_tokens, 100);
  assert.equal(result.totals.field_unavailable_attempts.input_tokens, 1);
  assert.equal(result.requests.find(row => row.attempt === 'unpublished').usage_outcome.status, 'unavailable');
});


test('seat-day estimate requires all settled window attempts, retaining the known subtotal', () => {
  const known = attempt('known-cost', 100, 90);
  known.cost = { status: 'estimated', usd: 0.25, policy: 'pinned-test', terminal_usage: true };
  const complete = summarize([report([known])]);
  assert.equal(complete.seat_day_cost.status, 'estimated');
  assert.equal(complete.seat_day_cost.usd, 0.25);
  assert.deepEqual(complete.seat_day_cost.policies, ['pinned-test']);
  const nonterminal = structuredClone(known); nonterminal.cost.terminal_usage = false;
  assert.equal(summarize([report([nonterminal])]).seat_day_cost.status, 'unavailable');
  const copy = summarize([report([known], 'ses_parent'), report([structuredClone(known)], 'ses_fork')]);
  assert.equal(copy.seat_day_cost.usd, 0.25);
  for (const reports of [[], [report([known, attempt('unknown-cost', 100, 0)])],
    [{ ...report([known]), enumeration_complete: false }],
    [{ ...report([known]), session_totals: { status: 'incomplete' } }]]) {
    assert.equal(summarize(reports).seat_day_cost.status, 'unavailable');
  }
  assert.equal(summarize([report([known, attempt('missing-cost', 10, 0)])]).totals.cost.usd, 0.25);
});
