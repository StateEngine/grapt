// Durable TokenUsageRecord format, Apache-2.0. Read reported response usage,
// never token_count's replayed or estimated context snapshots. No prompt/tool
// bodies are exported.
import { createReadStream } from 'node:fs';
import { lstat } from 'node:fs/promises';
import { isDeepStrictEqual } from 'node:util';
import { summarize, totals } from './seat-day-report.mjs';

const MAX_RECORDS = 8192;
const unavailable = reason => ({ status: 'unavailable', reason });
const observed = tokens => tokens === undefined
  ? unavailable('codex-field-not-recorded')
  : { status: 'observed', tokens, provenance: { kind: 'provider-reported' } };

function identity(value) {
  if (typeof value !== 'string' || !value.length || value.length > 256) {
    throw new Error('Codex usage identity is missing or exceeds its bound');
  }
  return value;
}

const optionalText = value => value == null ? null : identity(value);

function timestamp(value) {
  if (typeof value !== 'string' || !/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,9})?Z$/.test(value)
      || !Number.isSafeInteger(Date.parse(value))
      || new Date(Date.parse(value)).toISOString().slice(0, 19) !== value.slice(0, 19)) throw new Error('Codex record timestamp is invalid');
  return Date.parse(value);
}

function usage(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('Codex response usage is absent');
  }
  for (const key of ['input_tokens', 'cached_input_tokens', 'cache_write_input_tokens',
    'output_tokens', 'reasoning_output_tokens', 'total_tokens']) {
    if (value[key] !== undefined && (!Number.isSafeInteger(value[key]) || value[key] < 0)) {
      throw new Error(`Invalid Codex ${key}`);
    }
  }
  if (value.cached_input_tokens > value.input_tokens) throw new Error('Codex cache read exceeds input');
  return {
    input_total: observed(value.input_tokens), cache_read: observed(value.cached_input_tokens),
    cache_write: observed(value.cache_write_input_tokens), output: observed(value.output_tokens),
  };
}

async function* lines(file, budget) {
  const stat = await lstat(file);
  if (!stat.isFile()) throw new Error('Codex rollout is not a regular file');
  let pending = Buffer.alloc(0);
  const decoder = new TextDecoder('utf-8', { fatal: true });
  for await (const chunk of createReadStream(file, { highWaterMark: 65536 })) {
    budget.bytes += chunk.length;
    if (budget.bytes > 256 * 1024 * 1024) throw new Error('Codex export exceeds 256 MiB');
    pending = Buffer.concat([pending, chunk]);
    let end;
    while ((end = pending.indexOf(10)) !== -1) {
      if (end > 8 * 1024 * 1024 || ++budget.lines > 262144) {
        throw new Error('Codex rollout exceeds its line bound');
      }
      yield decoder.decode(pending.subarray(0, end));
      pending = pending.subarray(end + 1);
    }
    if (pending.length > 8 * 1024 * 1024) throw new Error('Codex rollout line exceeds 8 MiB');
  }
  // A partial append is not a settled export, even when it happens to be JSON.
  if (pending.length) throw new Error('Codex rollout ends with an incomplete line');
}

export async function codexReport(files, window = null) {
  if (!files.length || files.length > 64 || new Set(files).size !== files.length) {
    throw new Error('Supply 1–64 distinct Codex rollout paths');
  }
  const budget = { bytes: 0, lines: 0 };
  const records = new Map(), contexts = new Map(), sessions = new Map(), compactions = new Map();
  let copies = 0, snapshots = 0, unknownCompactions = 0, inherited = 0;
  for (const file of files) {
    let meta = null;
    for await (const line of lines(file, budget)) {
      if (!line.trim()) throw new Error('Codex rollout contains an empty record');
      let row;
      try { row = JSON.parse(line); }
      catch { throw new Error('Codex rollout contains invalid JSON'); }
      const body = row?.payload;
      if (!row || typeof row !== 'object' || !body || typeof body !== 'object' || Array.isArray(body)) {
        throw new Error('Codex rollout record is malformed');
      }
      if (row.type === 'session_meta') {
        const id = identity(body.id);
        const current = { id, cli_version: optionalText(body.cli_version), provider: optionalText(body.model_provider) };
        if (meta && !isDeepStrictEqual(meta, current)) throw new Error('Conflicting Codex session metadata');
        meta = current;
        sessions.set(id, current);
        if (sessions.size > 256) throw new Error('Codex export exceeds 256 sessions');
      } else if (row.type === 'turn_context' && body.turn_id) {
        const key = identity(body.turn_id);
        const context = { model: optionalText(body.model), effort: optionalText(body.effort) };
        if (contexts.has(key) && !isDeepStrictEqual(contexts.get(key), context)) {
          throw new Error('Codex turn model changed without per-response attribution');
        }
        contexts.set(key, context);
        if (contexts.size > 32768) throw new Error('Codex export exceeds 32768 turn contexts');
      } else if (row.type === 'token_usage_record') {
        if (!meta) throw new Error('Codex usage precedes session metadata');
        const id = identity(body.response_id);
        const record = { id, session: identity(body.thread_id), turn: identity(body.turn_id),
          runtime_session: optionalText(body.session_id), root_turn: identity(body.root_turn_id),
          usage: usage(body.usage) };
        // Fork history is persisted again with new outer timestamps. Only
        // the originating thread's own file can attribute time and provider.
        const at = timestamp(row.timestamp);
        const origin = meta.id === record.session ? { reported_ms: at, provider: meta.provider } : null;
        if (!origin) inherited++;
        const previous = records.get(id);
        if (previous) {
          if (!isDeepStrictEqual(previous.value, record)) throw new Error('Conflicting Codex response copies');
          if (origin && previous.origin && !isDeepStrictEqual(origin, previous.origin)) {
            throw new Error('Conflicting Codex origin attribution');
          }
          if (origin) previous.origin = origin;
          copies++;
        } else records.set(id, { value: record, origin });
        if (records.size > MAX_RECORDS) throw new Error('Codex export exceeds 8192 responses');
      } else if (row.type === 'compacted') {
        if (!meta) throw new Error('Codex compaction precedes session metadata');
        if (body.compaction_response_id) {
          const id = identity(body.compaction_response_id), at = timestamp(row.timestamp);
          if (!compactions.has(id)) compactions.set(id, new Map());
          const origins = compactions.get(id);
          if (origins.has(meta.id) && origins.get(meta.id) !== at) throw new Error('Conflicting Codex compaction origin');
          origins.set(meta.id, at);
          if (compactions.size > MAX_RECORDS) throw new Error('Codex export exceeds 8192 compactions');
        } else {
          const at = timestamp(row.timestamp);
          if (!window || (at >= window.from && at < window.until)) unknownCompactions++;
        }
      } else if (row.type === 'event_msg' && body.type === 'token_count') snapshots++;
    }
    if (!meta) throw new Error('Codex rollout session metadata is absent');
  }
  if (!records.size) throw new Error('Codex reported response records unavailable; token_count snapshots are insufficient');
  const reports = new Map();
  const unplaced = [];
  const sourceRows = [...records.values()].map(row => ({ ...row.value,
    reported_ms: row.origin?.reported_ms ?? null, provider: row.origin?.provider ?? null }));
  const byResponse = new Map(sourceRows.map(row => [row.id, row]));
  const selected = sourceRows.filter(row => {
    if (!window) return true;
    if (row.reported_ms === null) { unplaced.push(row.id); return false; }
    return row.reported_ms >= window.from && row.reported_ms < window.until;
  });
  for (const row of selected) {
    if (!reports.has(row.session)) reports.set(row.session, {
      schema: 'secode.session-accounting.v1', session: row.session,
      enumeration_complete: false, session_totals: { status: 'incomplete' }, attempts: [],
    });
    reports.get(row.session).attempts.push({
      attempt: row.id, provider: row.provider, model: contexts.get(row.turn)?.model ?? null,
      operation: compactions.has(row.id) ? 'compaction' : 'unclassified',
      latency: {}, usage: row.usage, cost: unavailable('codex-billing-not-reported'),
      usage_outcome: { status: 'observed', basis: 'codex-token-usage-record', terminal_status: 'unavailable' },
    });
  }
  const result = summarize([...reports.values()]);
  result.source = 'codex';
  result.window = window;
  result.window_unplaced_attempts = unplaced;
  result.basis = 'Codex provider response records; window selects reported timestamps in [from, until), not unrecorded opening times';
  result.attempt_reconciliation = { status: 'incomplete', reason: 'codex-dispatch-ledger-unavailable' };
  const turns = new Map();
  for (const row of result.requests) {
    const source = byResponse.get(row.attempt);
    Object.assign(row, { reported_ms: source.reported_ms, turn_id: source.turn,
      root_turn_id: source.root_turn, effort: contexts.get(source.turn)?.effort ?? null });
    if (!turns.has(source.root_turn)) turns.set(source.root_turn, []);
    turns.get(source.root_turn).push(row);
  }
  result.codex = {
    sessions: [...sessions.values()], copied_response_records: copies, inherited_observations: inherited,
    responses_without_origin: sourceRows.filter(row => row.reported_ms === null).length,
    ignored_token_count_snapshots: snapshots,
    coverage: {
      basis: 'reported response records only; unreported failed dispatches and opening timestamps are unavailable',
      all_dispatches_reconciled: false, retry_classification: 'unavailable',
      window_boundary_crossings: 'unavailable',
      model_basis: 'selected turn context; per-response rerouting is not recorded',
    },
    compactions: {
      recorded: [...compactions].filter(([id, origins]) => {
        const at = origins.get(byResponse.get(id)?.session);
        return at !== undefined && (!window || (at >= window.from && at < window.until));
      }).length,
      without_origin_timing: [...compactions].filter(([id, origins]) => !origins.has(byResponse.get(id)?.session)).length,
      without_response_identity: unknownCompactions,
    },
    turns: [...turns].map(([root_turn_id, rows]) => ({ root_turn_id, ...totals(rows) })),
  };
  if (Buffer.byteLength(JSON.stringify(result)) > 32 * 1024 * 1024) {
    throw new Error('Codex report exceeds 32 MiB');
  }
  return result;
}
