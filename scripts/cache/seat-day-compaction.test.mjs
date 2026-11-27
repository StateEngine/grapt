import test from 'node:test';
import assert from 'node:assert/strict';
import { operationalReport } from './seat-day-operations.mjs';

const field = tokens => ({ status: 'observed', tokens, provenance: { kind: 'provider-reported' } });
const attempt = (id, at, input, read, output = 0) => ({ attempt: id, operation: 'assistant',
  latency: { opened_ms: at }, usage: { input_total: field(input), cache_read: field(read), output: field(output) } });
const event = (type, data, seq, at = seq) => ({ type, data, seq, created: at, session: 'ses_test', id: `evt_${seq}` });
const compact = (id, seq, at) => event('session.compaction.ended.2', {
  compactionID: id, sourceSession: 'ses_test', sourceEpoch: 0, targetEpoch: 1,
  summary: { kind: 'clientReadable', artifact: { id: 'summary', length: 6 } }, nextRequest: { step: 5 },
}, seq, at);
const context = (id, descriptor, step, seq) => event('session.request.context.1', {
  attempt: id, descriptor, step, epoch: 1,
}, seq);
const readArtifact = async () => ({ status: 'observed', text: '界abc' });
const report = attempts => [{ session: 'ses_test', attempts }];

test('first post-compaction Step includes retries, deduplicates fork copies and weights actual input', async () => {
  const committed = compact('compact_a', 1, 90);
  const events = [committed, { ...committed, session: 'ses_fork', id: 'evt_fork' },
    context('first', 'evt_1', 5, 2), context('retry', 'evt_1', 5, 3), context('later', 'evt_1', 6, 4),
    compact('compact_b', 5, 150), context('outside', 'evt_5', 5, 6)];
  const result = await operationalReport(events, report([attempt('first', 100, 1000, 900),
    attempt('retry', 110, 100, 0), attempt('later', 120, 10000, 9999), attempt('outside', 200, 100, 50)]),
  readArtifact, { from: 100, until: 200 });
  assert.equal(result.compactions.length, 1); // First descriptor precedes the window.
  assert.equal(result.compaction_cache.committed_compactions, 1);
  const post = result.compaction_cache.first_post_step;
  assert.deepEqual(post.attempts.map(row => row.attempt), ['first', 'retry']);
  assert.equal(post.paired_input_tokens, 1100); assert.equal(post.paired_read_tokens, 900);
  assert.equal(post.read_share_percent, 100 * 900 / 1100);
  assert.equal(post.uncached_input_tokens, 200);
});

test('missing post usage, missing dispatch and unplaceable openings remain explicit', async () => {
  const events = [compact('compact_a', 1, 100), context('missing_usage', 'evt_1', 5, 2),
    context('missing_opening', 'evt_1', 5, 3), compact('compact_no_post', 4, 150)];
  const missing = attempt('missing_usage', 110, 100, 0); delete missing.usage.cache_read;
  const unplaced = attempt('missing_opening', null, 100, 90);
  const result = await operationalReport(events, report([missing, unplaced]), readArtifact, { from: 100, until: 200 });
  assert.equal(result.compaction_cache.committed_compactions, 2);
  assert.equal(result.compaction_cache.first_post_steps_unavailable, 1);
  const post = result.compaction_cache.first_post_step;
  assert.equal(post.usage_unavailable_attempts, 1); assert.equal(post.read_share_percent, null);
  assert.deepEqual(post.unplaced_attempts, ['missing_opening']);
});

test('summary generation tokens are separate from retained text size and failed summaries', async () => {
  const events = [event('session.compaction.call.started.1', { compaction: 'compact_a', attempt: 'summary', step: 5 }, 1),
    compact('compact_a', 2, 100),
    event('session.compaction.call.started.1', { compaction: 'failed', attempt: 'failed_summary', step: 7 }, 3, 110)];
  const result = await operationalReport(events, report([attempt('summary', 90, 1000, 900, 850)]), readArtifact);
  assert.equal(result.compaction_cache.committed_compactions, 1);
  assert.equal(result.compaction_cache.uncommitted_compactions, 1);
  const row = result.compactions.find(row => row.compaction === 'compact_a');
  assert.equal(row.summary_output_tokens, 850); assert.equal(row.summary_length_bytes, 6);
  assert.equal(row.summary_length_characters, 4);
  assert.equal(row.retained_summary_tokens.status, 'unavailable');
  assert.match(row.summary_output_basis, /reasoning/);
});

test('inconsistent post usage refuses and no compactions stays unexercised', async () => {
  await assert.rejects(operationalReport([compact('compact_a', 1, 100), context('bad', 'evt_1', 5, 2)],
    report([attempt('bad', 110, 1, 2)]), readArtifact), /cache read exceeds/);
  const result = await operationalReport([], [], readArtifact);
  assert.equal(result.compaction_cache.committed_compactions, 0);
  assert.equal(result.compaction_cache.first_post_step.read_share_percent, null);
});
