// Tool name/call/outcome/duration coordinates and bounded diagnostic previews follow
// the otel tool-result shape (Apache-2.0); the version pin is recorded in NOTICE.
// Copyright 2025 OpenAI. Durable Grapt events remain the only timing authority.
const unavailable = reason => ({ status: 'unavailable', reason });
const inside = (at, window) => !window || (at >= window.from && at < window.until);
const number = value => Number.isSafeInteger(value) && value >= 0;
function id(value) {
  if (typeof value !== 'string' || !value.length || Buffer.byteLength(value) > 256) throw new Error('Invalid operational identity');
  return value;
}
function stable(value) {
  if (Array.isArray(value)) return `[${value.map(stable).join(',')}]`;
  if (value && typeof value === 'object') {
    return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${stable(value[key])}`).join(',')}}`;
  }
  return JSON.stringify(value);
}
function unique(map, key, value) {
  const old = map.get(key);
  if (old && stable(old) !== stable(value)) throw new Error(`Conflicting operational facts for ${key}`);
  if (!old) map.set(key, value);
}
function percentile(values, fraction) {
  return values.length ? values[Math.ceil(values.length * fraction) - 1] : null;
}
function duration(start, end) {
  if (start === undefined || end === undefined) return null;
  if (end < start) throw new Error('Tool terminal precedes input identity');
  return end - start;
}

export function normalizeFailure(text) {
  // Keep the semantic error (including status/mode), remove volatile coordinates.
  // Final trimming handles whitespace before the literal diagnostic marker.
  return text.replace(/served_version:[\s\S]*$/i, '')
    .replace(/\b(?:ses|msg|agt|evt|call|attempt|exec)-[A-Za-z0-9_-]+\b|\b(?:ses|call|msg|exec)_[A-Za-z0-9_-]+\b/g, '<id>')
    .replace(/\b\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z\b/g, '<time>')
    .replace(/(?:[A-Za-z]:[\\/]|\/(?:home|tmp|Users|var)\/)[^\s;"']+/g, '<path>')
    .replace(/(at |not found: |nearest existing parent: |expected lines in )[^\s;"']*[/\\][^\s;"']*/g, '$1<path>')
    .replace(/; entries: [\s\S]*/, '; entries: <entries>')
    .replace(/\s+/g, ' ').trim().slice(0, 1024);
}

function index(events) {
  const starts = new Map(), ends = new Map(), summaries = new Map(), descriptors = new Map();
  const contexts = new Map(), executions = new Map(), steps = new Map(), summaryEnds = new Map();
  const aliases = new Map(), openings = new Map();
  let duplicateCopies = 0;
  for (const event of events) {
    const { data: d, type, created: at, session, seq } = event;
    if (!Number.isSafeInteger(at) || !number(seq)) throw new Error('Invalid operational timestamp/sequence');
    if (type.startsWith('session.assistant-message.opened.')) {
      if (!number(d.step) || !d.step) throw new Error('Invalid assistant opening Step');
      unique(openings, `${session}/${d.step}`, id(d.attempt));
    } else if (type.startsWith('session.tool.input.started.')) {
      const key = id(d.callID), value = { name: id(d.name), started_ms: at, parent: null };
      if (starts.has(key)) duplicateCopies++;
      unique(starts, key, value);
    } else if (type === 'session.tool.nested.started.1') {
      const key = id(d.call), value = { name: id(d.tool), started_ms: at, parent: id(d.parent) };
      if (starts.has(key)) duplicateCopies++;
      unique(starts, key, value);
    } else if (type === 'session.tool.terminal.1') {
      if (!['completed', 'failed', 'cancelled', 'result-missing'].includes(d.outcome?.kind)) throw new Error('Invalid tool outcome');
      if (d.outcome.kind === 'failed') id(d.outcome.reason);
      if (d.outcome.kind === 'cancelled') id(d.outcome.by);
      unique(ends, id(d.call), { outcome: d.outcome, ended_ms: at, attempt: id(d.attempt) });
    } else if (type === 'session.compaction.call.started.1') {
      if (!number(d.step) || d.step === 0) throw new Error('Invalid summary Step');
      unique(summaries, id(d.attempt), { compaction: id(d.compaction), attempt: d.attempt, step: d.step, at });
    } else if (type === 'session.compaction.call.ended.1') {
      if (!['completed', 'failed', 'cancelled'].includes(d.outcome)) throw new Error('Invalid summary call outcome');
      unique(summaryEnds, id(d.call?.attempt), { compaction: id(d.call.compaction), outcome: d.outcome, at });
    } else if (type === 'session.compaction.ended.2') {
      id(d.sourceSession);
      if (!number(d.sourceEpoch) || !number(d.targetEpoch) || !['clientReadable', 'providerBound'].includes(d.summary?.kind)) {
        throw new Error('Invalid compaction descriptor');
      }
      const key = id(d.compactionID);
      const existing = descriptors.get(key);
      if (existing && (stable(existing.data) !== stable(d) || existing.created !== at)) throw new Error('Conflicting compaction descriptor');
      if (!existing) descriptors.set(key, event);
      aliases.set(event.id, key);
    } else if (type === 'session.request.context.1') {
      if (!number(d.epoch)) throw new Error('Invalid request context epoch');
      id(d.descriptor);
      const key = `${session}/${id(d.attempt)}`;
      unique(contexts, key, { ...d, session, seq, at });
    } else if (type === 'session.execution.failed.1') {
      if (d.sessionID !== session || typeof d.error?.message !== 'string' || typeof d.error?.type !== 'string') throw new Error('Invalid failed execution');
      unique(executions, `${session}/${id(d.executionID)}`, { session, execution: d.executionID, at, reason: d.error });
    } else if (type.startsWith('session.step.failed.')) {
      if (!number(d.step) || !d.step || typeof d.reason?.kind !== 'string') throw new Error('Invalid failed Step');
      const completed = type.endsWith('.3') ? d.completedAt : at;
      if (!Number.isSafeInteger(completed)) throw new Error('Invalid Step completion time');
      unique(steps, `${session}/${d.step}`, { session, step: d.step, at: completed, reason: d.reason });
    }
  }
  return { starts, ends, summaries, summaryEnds, descriptors, aliases, contexts, executions, steps, openings, duplicateCopies };
}

function failedSteps(facts, window) {
  const rows = new Map();
  for (const row of facts.steps.values()) {
    const attempt = facts.openings.get(`${row.session}/${row.step}`);
    const key = attempt ?? `${row.session}/${row.step}`;
    const previous = rows.get(key);
    if (previous) {
      if (previous.at !== row.at || stable(previous.reason) !== stable(row.reason)) throw new Error('Conflicting copied Step failure');
      previous.sessions.push(row.session);
    } else rows.set(key, { ...row, sessions: [row.session], initial_attempt: attempt ?? null,
      identity_status: attempt ? 'joined' : 'opening-unavailable' });
  }
  return [...rows.values()].filter(row => inside(row.at, window));
}

export const INFRASTRUCTURE_FAILURE_REASONS = new Set([
  'authority-unavailable',
  'approval-unavailable',
  'capture-failed',
  'effect-unknown',
]);

export async function operationalReport(events, reports, readArtifact, window = null) {
  const facts = index(events), groups = new Map(), failures = new Map(), missingDetails = new Map();
  let count = 0, failed = 0, infrastructureFailed = 0, missingNames = 0;
  const toolIDs = new Set([...facts.starts.keys(), ...facts.ends.keys()]);
  if (toolIDs.size > 32768) throw new Error('Operational report exceeds 32768 tool calls');
  for (const call of toolIDs) {
    const start = facts.starts.get(call), end = facts.ends.get(call);
    if (!inside(start?.started_ms ?? end.ended_ms, window)) continue;
    const name = start?.name ?? null, outcome = end?.outcome.kind ?? 'pending';
    const key = JSON.stringify([name, outcome]);
    if (!groups.has(key)) groups.set(key, { name, outcome, count: 0, durations: [] });
    const group = groups.get(key); group.count++; count++;
    if (name === null) missingNames++;
    const elapsed = duration(start?.started_ms, end?.ended_ms);
    if (elapsed !== null) group.durations.push(elapsed);
    if (outcome === 'result-missing') {
      infrastructureFailed++;
    } else if (outcome === 'failed') {
      failed++;
      if (INFRASTRUCTURE_FAILURE_REASONS.has(end.outcome.reason)) infrastructureFailed++;
      const detail = end.outcome.detail ? await readArtifact(end.outcome.detail) : unavailable('detail-not-recorded');
      if (detail.status !== 'observed') missingDetails.set(detail.reason, (missingDetails.get(detail.reason) ?? 0) + 1);
      const message = detail.status === 'observed' ? normalizeFailure(detail.text) : null;
      const failureKey = JSON.stringify([name, end.outcome.reason, message, detail.status, detail.reason]);
      const row = failures.get(failureKey) ?? { name, reason: end.outcome.reason, message,
        detail_status: detail.status, detail_unavailable_reason: detail.reason ?? null, count: 0,
        detail_truncated: detail.disposition === 'truncated' };
      row.count++; failures.set(failureKey, row);
    }
  }
  const byName = [...groups.values()].map(({ durations, ...row }) => {
    durations.sort((a, b) => a - b);
    return { ...row, duration_samples: durations.length, duration_unavailable: row.count - durations.length,
      p50_ms: percentile(durations, 0.5), p95_ms: percentile(durations, 0.95) };
  }).sort((a, b) => b.count - a.count || String(a.name).localeCompare(String(b.name)) || a.outcome.localeCompare(b.outcome));
  const allDurations = [...groups.values()].flatMap(group => group.durations).sort((a, b) => a - b);
  const ranked = [...failures.values()].sort((a, b) => b.count - a.count || String(a.name).localeCompare(String(b.name)) || String(a.message).localeCompare(String(b.message)));
  const allCompactions = await compactionRows(facts, reports, readArtifact, window);
  const compactions = allCompactions.filter(row => inside(row.at, window));
  return {
    tool_calls: { count, failed, failure_percent: count ? 100 * failed / count : null,
      infrastructure_failed: infrastructureFailed,
      infrastructure_failure_percent: count ? 100 * infrastructureFailed / count : null,
      duplicate_start_copies: facts.duplicateCopies, missing_name_calls: missingNames,
      duration_samples: allDurations.length, duration_unavailable: count - allDurations.length,
      p50_ms: percentile(allDurations, 0.5), p95_ms: percentile(allDurations, 0.95),
      duration_basis: 'input identity (nested admission for nested calls) to terminal; includes preparation, approval and queue time; nearest-rank percentiles',
      window_basis: 'call start in [from,until); orphan terminals use terminal time', by_name_outcome: byName },
    top_tool_failures: ranked.slice(0, 50), other_failure_groups: Math.max(0, ranked.length - 50),
    failure_detail_unavailable: Object.fromEntries(missingDetails), compactions,
    compaction_cache: compactionCache(allCompactions, window),
    failed_executions: [...facts.executions.values()].filter(row => inside(row.at, window)),
    step_failures: failedSteps(facts, window),
  };
}

function compactionCache(rows, window) {
  const selected = new Map(), unplaced = new Set();
  for (const row of rows) for (const attempt of row.first_post_step.attempts) {
    if (attempt.opened_ms === null) { unplaced.add(attempt.attempt); continue; }
    if (!inside(attempt.opened_ms, window)) continue;
    unique(selected, attempt.attempt, attempt);
  }
  const attempts = [...selected.values()];
  const paired = attempts.filter(row => row.input_tokens !== null && row.cache_read_tokens !== null);
  const sum = key => {
    const value = paired.reduce((total, row) => total + row[key], 0);
    if (!number(value)) throw new Error('Post-compaction token sum overflow');
    return paired.length ? value : null;
  };
  const input = sum('input_tokens'), read = sum('cache_read_tokens');
  const committed = rows.filter(row => row.descriptor_committed && inside(row.at, window));
  return {
    committed_compactions: committed.length,
    uncommitted_compactions: rows.filter(row => !row.descriptor_committed && inside(row.at, window)).length,
    first_post_steps_unavailable: committed.filter(row => !row.first_post_step.attempts.length).length,
    first_post_step: { basis: 'all Physical Attempts of the first descriptor-bound assistant Step in the source session; attempts opened in [from,until), including prewindow compactions',
      attempts, paired_attempts: paired.length, usage_unavailable_attempts: attempts.length - paired.length,
      unplaced_attempts: [...unplaced], paired_input_tokens: input, paired_read_tokens: read,
      read_share_percent: input ? 100 * read / input : null,
      uncached_input_tokens: input === null ? null : input - read },
  };
}

async function compactionRows(facts, reports, readArtifact, window) {
  const attempts = new Map();
  for (const report of reports) for (const attempt of report.attempts) attempts.set(`${report.session}/${attempt.attempt}`, attempt);
  const token = (attempt, key) => {
    const value = attempt?.usage?.[key];
    return value?.status === 'observed' && ['provider-reported', 'provider-derived'].includes(value.provenance?.kind)
      && number(value.tokens) ? value.tokens : null;
  };
  const rows = [];
  const compactionIDs = new Set([...facts.descriptors.keys(), ...[...facts.summaries.values()].map(call => call.compaction)]);
  for (const compaction of compactionIDs) {
    const event = facts.descriptors.get(compaction);
    const summaryCalls = [...facts.summaries.values()].filter(call => call.compaction === compaction)
      .sort((a, b) => a.at - b.at || a.attempt.localeCompare(b.attempt));
    const at = event?.created ?? summaryCalls[0]?.at;
    const d = event?.data;
    const session = d?.sourceSession ?? reports.find(report => report.attempts.some(attempt => summaryCalls.some(call => call.attempt === attempt.attempt)))?.session ?? null;
    const tokens = key => {
      const values = summaryCalls.map(call => token(attempts.get(`${session}/${call.attempt}`), key));
      if (!values.length || values.some(value => value === null)) return null;
      const total = values.reduce((a, b) => a + b, 0);
      if (!number(total)) throw new Error('Compaction token sum overflow');
      return total;
    };
    const contextual = [...facts.contexts.values()].filter(context => context.session === session);
    const before = contextual.filter(context => d && context.epoch === d.sourceEpoch && context.seq < event.seq)
      .sort((a, b) => b.seq - a.seq)[0];
    const after = contextual.filter(context => d && context.epoch === d.targetEpoch
      && facts.aliases.get(context.descriptor) === compaction && context.seq > event.seq)
      .sort((a, b) => a.seq - b.seq)[0];
    const firstPost = after && number(after.step) && after.step > 0 ? contextual.filter(context =>
      context.step === after.step && context.epoch === d.targetEpoch
      && facts.aliases.get(context.descriptor) === compaction && context.seq > event.seq) : [];
    const postAttempts = firstPost.map(context => {
      const attempt = attempts.get(`${session}/${context.attempt}`);
      const input = token(attempt, 'input_total'), read = token(attempt, 'cache_read');
      if (input !== null && read !== null && read > input) throw new Error('Post-compaction cache read exceeds input');
      return { attempt: context.attempt, step: context.step,
        opened_ms: Number.isSafeInteger(attempt?.latency?.opened_ms) ? attempt.latency.opened_ms : null,
        input_tokens: input, cache_read_tokens: read,
        read_share_percent: input ? (read === null ? null : 100 * read / input) : null };
    });
    const summary = !inside(at, window) ? unavailable('compaction-outside-window')
      : d?.summary?.kind === 'clientReadable' ? await readArtifact(d.summary.artifact)
        : unavailable(d ? 'provider-bound-summary' : 'descriptor-not-committed');
    rows.push({ session, compaction, at, descriptor_committed: !!d, next_step: d?.nextRequest?.step ?? null,
      summary_attempts: summaryCalls.map(call => ({ attempt: call.attempt,
        outcome: facts.summaryEnds.get(call.attempt)?.outcome ?? 'pending' })), summary_input_tokens: tokens('input_total'),
      summary_cached_tokens: tokens('cache_read'), summary_output_tokens: tokens('output'),
      summary_output_basis: 'provider generation output across summary attempts, including reasoning when counted by the provider; not retained-text input tokens',
      retained_summary_tokens: unavailable('exact-retained-text-input-token-count-not-recorded'),
      first_post_step: { step: after?.step ?? null, attempts: postAttempts },
      context_before_tokens: token(attempts.get(`${session}/${before?.attempt}`), 'input_total'),
      context_after_tokens: token(attempts.get(`${session}/${after?.attempt}`), 'input_total'),
      context_basis: 'provider-reported inclusive input on last source-epoch / first descriptor-bound assistant request; absent endpoints are null',
      summary_length_bytes: d?.summary?.artifact?.length ?? null,
      summary_length_characters: summary.status === 'observed' ? [...summary.text].length : null,
      summary_text_status: summary.status, summary_text_unavailable_reason: summary.reason ?? null });
  }
  return rows.sort((a, b) => a.at - b.at || a.compaction.localeCompare(b.compaction));
}
