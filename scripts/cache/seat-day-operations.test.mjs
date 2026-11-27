import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, readFile, rm, symlink } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import os from 'node:os';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { operationalReport, normalizeFailure } from './seat-day-operations.mjs';
import { artifactReader } from './seat-day-artifacts.mjs';
import { readEvents } from './seat-day-events.mjs';

const event = (type, data, at = 100, session = 'ses_test', seq = 1, eventID = `evt_${seq}`) =>
  ({ type, data, created: at, session, seq, id: eventID });
const start = (call, name, at = 100, session = 'ses_test') => event('session.tool.input.started.1',
  { callID: call, name, sessionID: 'ses_test', assistantMessageID: 'msg_a' }, at, session);
const end = (call, kind, at, detail) => event('session.tool.terminal.1', {
  call, attempt: 'attempt_a', outcome: { kind, ...(kind === 'failed' ? { reason: 'execution-failed', detail } : {}) },
}, at);
const absent = async () => ({ status: 'unavailable', reason: 'ENOENT' });
const field = tokens => ({ status: 'observed', tokens, provenance: { kind: 'provider-reported' } });
const attempt = (id, input, cache, output, operation = 'assistant') => ({ attempt: id, operation,
  usage: { input_total: field(input), cache_read: field(cache), output: field(output) } });

test('fork copies count once, nested names survive, and percentiles include only observed durations', async () => {
  const events = [start('call_one', 'grep'), start('call_one', 'grep', 100, 'ses_fork'),
    end('call_one', 'failed', 110, { id: 'detail' }), { ...end('call_one', 'failed', 110, { id: 'detail' }), session: 'ses_fork' },
    event('session.tool.nested.started.1', { attempt: 'attempt_a', parent: 'call_one', call: 'call_nested', tool: 'stateengine_resume' }, 110),
    end('call_nested', 'completed', 130), start('call_two', 'grep', 120), end('call_two', 'completed', 220),
    start('call_pending', 'grep', 130), end('call_orphan', 'completed', 250)];
  const report = await operationalReport(events, [], async () => ({ status: 'observed', text: 'grep failed', disposition: 'complete' }));
  assert.equal(report.tool_calls.count, 5);
  assert.equal(report.tool_calls.failed, 1);
  assert.equal(report.tool_calls.duplicate_start_copies, 1);
  assert.equal(report.tool_calls.missing_name_calls, 1);
  const nested = report.tool_calls.by_name_outcome.find(row => row.name === 'stateengine_resume');
  assert.equal(nested.p50_ms, 20);
  const pending = report.tool_calls.by_name_outcome.find(row => row.outcome === 'pending');
  assert.equal(pending.p95_ms, null); assert.equal(pending.duration_unavailable, 1);
  assert.equal(report.top_tool_failures[0].count, 1);
  const windowed = await operationalReport(events, [], absent, { from: 110, until: 130 });
  assert.equal(windowed.tool_calls.count, 2);
});

test('nearest-rank p50/p95, conflicting copies and reversed timestamps are explicit', async () => {
  const events = Array.from({ length: 20 }, (_, n) => [start(`c${n}`, 'read'), end(`c${n}`, 'completed', 101 + n)]).flat();
  const result = await operationalReport(events, [], absent);
  const row = result.tool_calls.by_name_outcome[0];
  assert.equal(row.p50_ms, 10); assert.equal(row.p95_ms, 19);
  await assert.rejects(operationalReport([start('same', 'read'), start('same', 'write')], [], absent), /Conflicting/);
  await assert.rejects(operationalReport([start('same', 'read'), end('same', 'completed', 99)], [], absent), /precedes/);
  const missing = await operationalReport([start('one', 'read'), end('one', 'failed', 101, { id: 'missing' })], [], absent);
  assert.equal(missing.top_tool_failures[0].message, null);
  assert.deepEqual(missing.failure_detail_unavailable, { ENOENT: 1 });
});

test('failure normalization groups volatile coordinates without erasing HTTP status or mode guidance', () => {
  assert.equal(normalizeFailure('read not found: crates/old.rs; nearest existing parent: crates; entries: old.rs, new.rs'),
    'read not found: <path>; nearest existing parent: crates; entries: <entries>');
  assert.equal(normalizeFailure('HTTP status 404 at /home/example/a.rs msg-123 served_version: {"commit":"abc"}'),
    'HTTP status 404 at <path> <id>');
  assert.equal(normalizeFailure('history: omit cursor including 0 when using call'), 'history: omit cursor including 0 when using call');
});

test('bounded failure artifacts with long whitespace finish without losing the diagnostic', () => {
  const module = JSON.stringify(new URL('./seat-day-operations.mjs', import.meta.url).href);
  const child = spawnSync(process.execPath, ['--input-type=module', '--eval', `
    import { operationalReport } from ${module};
    const common = { session: 'ses_space', seq: 1 };
    const events = [
      { ...common, type: 'session.tool.input.started.1', created: 1,
        data: { callID: 'call_space', name: 'read' } },
      { ...common, type: 'session.tool.terminal.1', created: 2,
        data: { call: 'call_space', attempt: 'attempt_space', outcome: {
          kind: 'failed', reason: 'execution-failed', detail: { id: 'detail' } } } },
    ];
    const diagnostic = 'HTTP status 404';
    const text = ' '.repeat(1024 * 1024 - diagnostic.length) + diagnostic;
    const report = await operationalReport(events, [], async () => ({
      status: 'observed', text, disposition: 'complete' }));
    console.log(JSON.stringify({ failed: report.tool_calls.failed,
      message: report.top_tool_failures[0].message }));
  `], { encoding: 'utf8', timeout: 5_000, killSignal: 'SIGKILL', maxBuffer: 65_536 });
  assert.ifError(child.error);
  assert.equal(child.status, 0, child.stderr);
  assert.deepEqual(JSON.parse(child.stdout), { failed: 1, message: 'HTTP status 404' });
});

test('compaction joins summary retries and context endpoints; missing usage stays null', async () => {
  const descriptor = { compactionID: 'compact_a', sourceSession: 'ses_test', sourceEpoch: 0, targetEpoch: 1,
    summary: { kind: 'clientReadable', artifact: { id: 'summary', length: 6 } }, nextRequest: { step: 3 } };
  const events = [event('session.request.context.1', { attempt: 'before', epoch: 0, descriptor: 'initial' }, 100, 'ses_test', 1),
    event('session.compaction.call.started.1', { compaction: 'compact_a', attempt: 'retry', step: 3 }, 150, 'ses_test', 2),
    event('session.compaction.call.ended.1', { call: { compaction: 'compact_a', attempt: 'retry' }, outcome: 'failed' }, 160, 'ses_test', 3),
    event('session.compaction.call.started.1', { compaction: 'compact_a', attempt: 'summary', step: 3 }, 170, 'ses_test', 4),
    event('session.compaction.call.ended.1', { call: { compaction: 'compact_a', attempt: 'summary' }, outcome: 'completed' }, 180, 'ses_test', 5),
    event('session.compaction.ended.2', descriptor, 190, 'ses_test', 6, 'evt_descriptor'),
    event('session.request.context.1', { attempt: 'after', epoch: 1, descriptor: 'evt_descriptor' }, 200, 'ses_test', 7),
    event('session.compaction.call.started.1', { compaction: 'compact_failed', attempt: 'unavailable', step: 4 }, 250, 'ses_test', 8),
    event('session.compaction.call.ended.1', { call: { compaction: 'compact_failed', attempt: 'unavailable' }, outcome: 'cancelled' }, 260, 'ses_test', 9)];
  const reports = [{ session: 'ses_test', attempts: [attempt('before', 300000, 290000, 100),
    attempt('retry', 40000, 0, 0, 'compaction'), attempt('summary', 45000, 0, 2000, 'compaction'),
    attempt('after', 20000, 10000, 100)] }];
  const result = await operationalReport(events, reports, async () => ({ status: 'observed', text: '界abc' }));
  const row = result.compactions[0];
  assert.equal(row.summary_input_tokens, 85000); assert.equal(row.summary_cached_tokens, 0);
  assert.equal(row.summary_output_tokens, 2000); assert.equal(row.context_before_tokens, 300000);
  assert.equal(row.context_after_tokens, 20000); assert.equal(row.summary_length_characters, 4);
  assert.equal(row.summary_length_bytes, 6); assert.equal(row.summary_attempts[0].outcome, 'failed');
  assert.equal(result.compactions[1].summary_input_tokens, null);
  assert.equal(result.compactions[1].descriptor_committed, false);
  assert.equal(result.compactions[1].summary_text_unavailable_reason, 'descriptor-not-committed');
});

test('failed execution and Step reasons use the settled v3 timestamp and survive rewinds', async () => {
  const events = [event('session.execution.failed.1', { sessionID: 'ses_test', executionID: 'exe_a', error: { type: 'ContextLimit', message: 'context too large' } }),
    event('session.step.failed.3', { step: 7, reason: { kind: 'provider-failure', class: 'throttled' }, completedAt: 200 }, 90)];
  const result = await operationalReport(events, [], absent, { from: 100, until: 201 });
  assert.equal(result.failed_executions[0].reason.type, 'ContextLimit');
  assert.equal(result.step_failures[0].at, 200);
  assert.equal(result.step_failures[0].reason.class, 'throttled');
});

test('copied Step failures join the unchanged initial attempt, not reminted message identity', async () => {
  const events = ['ses_parent', 'ses_fork'].flatMap(session => [
    event('session.assistant-message.opened.2', { attempt: 'physical_initial', step: 7, message: `msg_${session}` }, 100, session),
    event('session.step.failed.3', { step: 7, reason: { kind: 'provider-failure', class: 'throttled' }, completedAt: 200 }, 150, session),
  ]);
  const result = await operationalReport(events, [], absent);
  assert.equal(result.step_failures.length, 1);
  assert.equal(result.step_failures[0].initial_attempt, 'physical_initial');
  assert.deepEqual(result.step_failures[0].sessions, ['ses_parent', 'ses_fork']);
  events[3].data.reason.class = 'network';
  await assert.rejects(operationalReport(events, [], absent), /Conflicting copied Step/);
});

test('Step settlement conflicts are checked across both window boundaries before selection', async () => {
  const events = ['ses_parent', 'ses_fork'].flatMap((session, index) => [
    event('session.assistant-message.opened.2', { attempt: 'physical_initial', step: 7 }, 50, session),
    event('session.step.failed.3', { step: 7, reason: { kind: 'durability-failure' }, completedAt: index ? 200 : 100 }, 60, session),
  ]);
  for (const window of [{ from: 0, until: 150 }, { from: 150, until: 300 }]) {
    await assert.rejects(operationalReport(events, [], absent, window), /Conflicting copied Step/);
    await assert.rejects(operationalReport([...events].reverse(), [], absent, window), /Conflicting copied Step/);
  }
});

async function artifact(root, text, sensitivity = 'ordinary') {
  const bytes = Buffer.from(text), digest = createHash('sha256').update(bytes).digest('hex');
  const length = Buffer.alloc(8); length.writeBigUInt64BE(BigInt(bytes.length));
  const id = createHash('sha256').update('secode-artifact/id/2').update('captured-output').update('complete').update(length).update(digest).digest('hex');
  const base = path.join(root, 'dogfood-artifacts');
  for (const dir of ['meta', 'objects']) await mkdir(path.join(base, dir, id.slice(0, 2)), { recursive: true });
  await writeFile(path.join(base, 'meta', id.slice(0, 2), id), `v1\nid ${id}\nlength ${bytes.length}\ndigest ${digest}\ndisposition complete\nsensitivity ${sensitivity}\nkind captured-output\nchunk_size 65536\ncaptured_at 0\n`);
  const object = path.join(base, 'objects', id.slice(0, 2), id);
  await writeFile(object, bytes);
  return { reference: { id, length: bytes.length, digest, disposition: 'complete' }, object };
}

test('read-only artifacts verify metadata, identity, length and digest; no secret or linked bytes', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'grapt-observe-artifacts-'));
  try {
    const good = await artifact(root, 'tool failed 界');
    const before = await readFile(good.object);
    assert.equal((await artifactReader(root)(good.reference)).text, 'tool failed 界');
    assert.deepEqual(await readFile(good.object), before);
    await writeFile(good.object, 'changed bytes');
    assert.equal((await artifactReader(root)(good.reference)).status, 'unavailable');
    const secret = await artifact(root, 'private', 'secret');
    assert.equal((await artifactReader(root)(secret.reference)).reason, 'secret-bearing');
    assert.equal((await artifactReader(root)({ ...good.reference, id: '../escape' })).reason, 'invalid-reference');
    if (process.platform !== 'win32') {
      await rm(good.object); await symlink(secret.object, good.object);
      assert.equal((await artifactReader(root)(good.reference)).reason, 'not-regular');
    }
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('SQLite event reader is read-only, bounded, version-explicit and does not drop rewound facts', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'grapt-observe-db-'));
  try {
    const database = path.join(root, 'database.sqlite3'), db = new DatabaseSync(database);
    db.exec('CREATE TABLE event (id TEXT, aggregate_family TEXT, aggregate_id TEXT, seq INTEGER, created INTEGER, type TEXT, data TEXT)');
    const insert = db.prepare('INSERT INTO event VALUES (?, ?, ?, ?, ?, ?, ?)');
    insert.run('evt_one', 'session', 'ses_test', 1, 100, 'session.tool.input.started.1', JSON.stringify({ callID: 'call_one', name: 'read' }));
    insert.run('evt_future', 'session', 'ses_test', 2, 110, 'session.tool.terminal.99', '{}');
    insert.run('evt_revert', 'session', 'ses_test', 3, 120, 'session.revert.committed.1', '{}');
    insert.run('evt_foreign', 'session', 'ses_other', 1, 100, 'session.tool.input.started.1', '{}');
    db.close();
    const before = await readFile(database), result = await readEvents(database, ['ses_test']);
    assert.equal(result.events.length, 1);
    assert.deepEqual(result.coverage.unsupported_event_versions, { 'session.tool.terminal.99': 1 });
    assert.deepEqual(await readFile(database), before);
    const writable = new DatabaseSync(database);
    writable.prepare('UPDATE event SET data = ? WHERE id = ?').run('x'.repeat(1048577), 'evt_one'); writable.close();
    await assert.rejects(readEvents(database, ['ses_test']), /byte bound/);
  } finally { await rm(root, { recursive: true, force: true }); }
});
