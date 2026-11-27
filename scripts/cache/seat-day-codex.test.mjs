import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { run } from './seat-day.mjs';

const stamp = '2026-10-30T00:01:00Z';
const row = (type, payload, timestamp = stamp) => ({ timestamp, type, payload });
const meta = id => row('session_meta', { id, cli_version: '0.159.0', model_provider: 'openai' });
const context = row('turn_context', { turn_id: 'turn_one', model: 'gpt-6.1-sol', effort: 'high' });
const response = (id, input, cached, timestamp = stamp) => row('token_usage_record', {
  thread_id: 'thread_one', turn_id: 'turn_one', root_turn_id: 'task_one', response_id: id,
  usage: { input_tokens: input, cached_input_tokens: cached, output_tokens: 3 },
}, timestamp);
async function file(root, name, rows) {
  const target = path.join(root, name);
  await writeFile(target, rows.map(value => JSON.stringify(value)).join('\n') + '\n');
  return target;
}

test('the public exporter groups Codex response usage without counting replayed snapshots or fork copies', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'codex-day-'));
  try {
    const first = response('response_one', 100, 0);
    const compact = row('compacted', { compaction_response_id: 'response_summary' });
    const parent = await file(root, 'parent.jsonl', [meta('thread_one'), context, first,
      response('response_summary', 900, 810), compact, response('response_retry', 1000, 900),
      row('event_msg', { type: 'token_count', info: { last_token_usage: { input_tokens: 999999 } } }),
      response('after_window', 1, 0, '2026-10-30T00:02:00Z')]);
    const fork = await file(root, 'fork.jsonl', [meta('thread_fork'), context, first, compact]);
    const result = await run(['--codex-rollout', parent, '--codex-rollout', fork,
      '--from', stamp, '--until', '2026-10-30T00:02:00Z']);
    assert.equal(result.totals.attempts, 3);
    assert.equal(result.totals.read_share_percent, 85.5);
    assert.equal(result.totals.uncached_input_tokens, 290);
    assert.equal(result.codex.turns[0].inclusive_input_tokens, 2000);
    assert.equal(result.codex.compactions.recorded, 1);
    assert.equal(result.codex.copied_response_records, 1);
    assert.equal(result.codex.ignored_token_count_snapshots, 1);
    assert.equal(result.requests.find(value => value.attempt === 'response_summary').operation, 'compaction');
    assert.equal(result.attempt_reconciliation.status, 'incomplete');
    assert.equal(result.requests[0].opened_ms, null);
    assert.equal(result.requests[0].effort, 'high');
    assert.equal(result.seat_day_cost.status, 'unavailable');
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('invalid, conflicting and snapshot-only Codex inputs fail; missing fields remain unavailable', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'codex-day-'));
  try {
    const args = rows => file(root, 'input.jsonl', rows).then(name => ['--codex-rollout', name]);
    for (const records of [
      [meta('thread_one'), response('bad', -1, 0)],
      [meta('thread_one'), response('same', 10, 5), response('same', 20, 5)],
      [meta('thread_one'), row('event_msg', { type: 'token_count', info: {} })],
    ]) await assert.rejects(run(await args(records)), /Invalid Codex|Conflicting Codex|unavailable/);
    const partial = response('partial', 10, 0); delete partial.payload.usage.cached_input_tokens;
    const result = await run(await args([meta('thread_one'), partial]));
    assert.equal(result.totals.inclusive_input_tokens, 10);
    assert.equal(result.totals.read_share_percent, null);
    assert.equal(result.totals.usage_unavailable_attempts, 1);
    const input = await file(root, 'truncated.jsonl', [meta('thread_one')]);
    await writeFile(input, '{"type":"token_usage_record"}');
    await assert.rejects(run(['--codex-rollout', input]), /incomplete line/);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('fork timestamps and provider metadata never replace the original response attribution', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'codex-origin-'));
  try {
    const original = response('copied', 100, 80);
    const copy = { ...original, timestamp: '2026-10-30T01:00:00Z' };
    const childMeta = meta('child'); childMeta.payload.model_provider = 'different-provider';
    const parent = await file(root, 'parent.jsonl', [meta('thread_one'), original]);
    const child = await file(root, 'child.jsonl', [childMeta, copy]);
    const combined = await run(['--codex-rollout', child, '--codex-rollout', parent]);
    assert.equal(combined.totals.attempts, 1);
    assert.equal(combined.requests[0].reported_ms, Date.parse(stamp));
    assert.equal(combined.requests[0].provider, 'openai');
    const inherited = await run(['--codex-rollout', child]);
    assert.equal(inherited.requests[0].reported_ms, null);
    assert.equal(inherited.requests[0].provider, null);
    const windowed = await run(['--codex-rollout', child,
      '--from', '2026-10-30T01:00:00Z', '--until', '2026-10-30T02:00:00Z']);
    assert.equal(windowed.totals.attempts, 0);
    assert.deepEqual(windowed.window_unplaced_attempts, ['copied']);
  } finally { await rm(root, { recursive: true, force: true }); }
});
