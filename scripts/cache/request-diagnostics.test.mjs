import test from 'node:test';
import assert from 'node:assert/strict';
import { diagnoseRequest } from './request-diagnostics.mjs';
import { summarize } from './seat-day-report.mjs';
const h = n => n.toString(16).padStart(64, '0');
const field = tokens => ({ status: 'observed', tokens, provenance: { kind: 'provider-reported' } });
function request(id, previous, overrides = {}) {
  return {
    attempt: id, operation: 'assistant', terminal_request_observed: true,
    request_cache: { admitted_ms: id === 'a' ? 10 : 40, previous_attempt: previous,
      record: { attempt: id, step: id === 'a' ? 1 : 2, fingerprint: {
        target: h(1), cacheKey: h(2), routing: h(3), tools: h(4), instructions: h(5),
        options: h(6), input: h(7), inputItems: 2, prefixes: [h(8), h(9)], ...overrides,
      } } },
    latency: { opened_ms: id === 'a' ? 10 : 40, step_completed_ms: id === 'a' ? 30 : 60 },
    usage: { input_total: field(1000), cache_read: field(900) },
  };
}
// A fixture accounting export supplies the owner's compact comparison result.
function attachComparison(current, previous) {
  const a = current.request_cache.record.fingerprint, b = previous.request_cache.record.fingerprint;
  let stable = 0;
  while (stable < Math.min(a.prefixes.length,b.prefixes.length) && a.prefixes[stable] === b.prefixes[stable]) stable++;
  const changed = ['target','cacheKey','routing','tools','instructions','options'].filter(key => a[key] !== b[key]);
  const itemChanged = stable < Math.min(a.prefixes.length,b.prefixes.length);
  if (itemChanged || a.inputItems < b.inputItems) changed.push('input');
  current.request_cache.comparison = { changed_components:changed, stable_input_items:stable,
    stable_input_hash:stable ? a.prefixes[stable-1] : null,
    complete:itemChanged || stable === Math.min(a.inputItems,b.inputItems) || a.input === b.input,
    append_only:stable === b.inputItems && a.inputItems > b.inputItems, identical:a.input === b.input };
  delete a.prefixes; delete b.prefixes;
}
function diagnose(current, previous = request('a', null)) {
  attachComparison(current, previous);
  return diagnoseRequest(current, new Map([[previous.attempt, previous], [current.attempt, current]]));
}
test('deliberate tool and instruction changes name their components', () => {
  const tool = diagnose(request('b', 'a', { tools: h(20) }));
  assert.equal(tool.kind, 'tool-list-change');
  assert.deepEqual(tool.changed_components, ['tools']);
  const instructions = diagnose(request('b', 'a', { instructions: h(21) }));
  assert.equal(instructions.kind, 'prefix-break');
  assert.deepEqual(instructions.changed_components, ['instructions']);
  assert.equal(instructions.provider_cause, 'unreported');
});
test('pure append retains the exact preceding item boundary', () => {
  const append = diagnose(request('b', 'a', { input: h(22), inputItems: 3, prefixes: [h(8), h(9), h(10)] }));
  assert.equal(append.kind, 'new-suffix');
  assert.equal(append.stable_input_prefix_items, 2);
  assert.equal(append.stable_input_prefix_hash, h(9));
  assert.equal(append.idle_gap_ms, 10);
  const edit = diagnose(request('b', 'a', { input: h(23), prefixes: [h(8), h(24)] }));
  assert.equal(edit.kind, 'prefix-break');
  assert.deepEqual(edit.changed_components, ['input']);
  assert.equal(edit.stable_input_prefix_items, 1);
});
test('lost cache reuse with a stable prefix is an idle candidate, never proven TTL', () => {
  const current = request('b', 'a');
  current.usage.cache_read = field(0);
  const result = diagnose(current);
  assert.equal(result.kind, 'ttl-or-idle-gap');
  assert.match(result.confidence, /candidate/);
  assert.equal(result.provider_cause, 'unreported');
  current.request_cache.admitted_ms = 20;
  const next = request('b','a'); next.request_cache.admitted_ms = 20; next.usage.cache_read = field(0);
  const noIdle = diagnose(next);
  assert.equal(noIdle.kind, undefined);
  assert.equal(noIdle.idle_gap_ms, null);
});
test('old rows, nonterminals and capped comparisons stay explicitly unavailable', () => {
  assert.equal(diagnoseRequest({ attempt: 'old' }, new Map()).status, 'unavailable');
  const first = request('a', null); delete first.request_cache.record.fingerprint.prefixes;
  assert.equal(diagnoseRequest(first,new Map()).reason, 'first-request-observation');
  const nonterminal = request('b', 'a'); nonterminal.terminal_request_observed = false;
  assert.equal(diagnose(nonterminal).reason, 'request-terminal-unavailable');
  const previous = request('a', null, { inputItems: 600, prefixes: Array(512).fill(h(8)) });
  const current = request('b', 'a', { inputItems: 601, prefixes: Array(512).fill(h(8)), input: h(22) });
  assert.equal(diagnose(current, previous).reason, 'request-prefix-comparison-bound');
  current.request_cache.record.fingerprint.secret = 'message';
  assert.throws(() => diagnoseRequest(current,new Map([['a',previous]])), /bounded/);
});
test('window filtering retains the predecessor outside the selected window', () => {
  const a = request('a', null), b = request('b', 'a', { tools: h(20) });
  attachComparison(b,a);
  const result = summarize([{ schema: 'secode.session-accounting.v1', session: 'ses_test',
    enumeration_complete: true, attempts: [a, b] }], { from: 40, until: 70 });
  assert.equal(result.requests.length, 1);
  assert.equal(result.requests[0].miss_cause.kind, 'tool-list-change');
  assert.equal(result.totals.cache_read_tokens, 900);
});

test('missing provider usage and fully cached requests do not invent miss causes', () => {
  const missing = request('b', 'a', { tools: h(20) });
  delete missing.usage.cache_read;
  assert.equal(diagnose(missing).reason, 'cache-usage-unreported');
  const cached = request('b', 'a', { tools: h(20) });
  cached.usage.cache_read = field(1000);
  assert.equal(diagnose(cached).status, 'no-miss');
  const invalid = request('b', 'a'); invalid.usage.cache_read = field(1001);
  assert.throws(() => diagnose(invalid), /exceeds/);
  const unobserved = request('a', null); delete unobserved.usage.input_total;
  assert.equal(diagnose(request('b', 'a'), unobserved).reason, 'predecessor-cache-usage-unreported');
});

test('fork copies preserve one durable diagnostic and fingerprint conflicts refuse before windowing', () => {
  const source = request('a', null); delete source.request_cache.record.fingerprint.prefixes;
  const copied = structuredClone(source);
  const report = (session, attempt) => ({ schema: 'secode.session-accounting.v1', session,
    enumeration_complete: true, attempts: [attempt] });
  const reports = [report('ses_parent', source), report('ses_fork', copied)];
  const result = summarize(reports);
  assert.equal(result.totals.attempts, 1);
  assert.equal(result.copied_attempt_exports, 1);
  assert.deepEqual(result.requests[0].request_fingerprint, source.request_cache);
  for (const fingerprint of [null, { ...copied.request_cache, record: {
    ...copied.request_cache.record, fingerprint: { ...copied.request_cache.record.fingerprint, tools: h(55) },
  } }]) {
    copied.request_cache = fingerprint;
    assert.throws(() => summarize(reports, { from: 40, until: 70 }), /Conflicting Product exports/);
  }
});

test('fork diagnostic terminal and completion conflicts refuse in both orders before windowing', () => {
  for (const conflict of ['terminal', 'completion']) {
    const a = request('a', null), b = request('b', 'a', { tools: h(20) });
    attachComparison(b, a);
    const copies = structuredClone([a, b]);
    if (conflict === 'terminal') copies[1].terminal_request_observed = false;
    else copies[0].latency.step_completed_ms = 39;
    const reports = [[a, b], copies].map((attempts, i) => ({
      schema: 'secode.session-accounting.v1', session: i ? 'ses_fork' : 'ses_parent',
      enumeration_complete: true, attempts,
    }));
    for (const ordered of [reports, reports.toReversed()]) {
      assert.throws(() => summarize(ordered, { from: 1000, until: 2000 }), /Conflicting Product exports/);
    }
  }
});
