import { diagnoseRequest } from './request-diagnostics.mjs';
// Accounting stays in Product; this module only combines its exported coordinates.
import { isDeepStrictEqual } from 'node:util';
const unavailable = reason => ({ status: 'unavailable', reason });

function count(usage, name) {
  const field = usage?.[name];
  const provenance = field?.provenance;
  if (field?.status !== 'observed' || (provenance?.kind !== 'provider-reported'
      && provenance?.kind !== 'provider-derived')) return null;
  if (!Number.isSafeInteger(field.tokens) || field.tokens < 0) {
    throw new Error(`Invalid observed ${name}`);
  }
  return field.tokens;
}

function sum(values) {
  if (!values.length) return null;
  const result = values.reduce((a, b) => a + b, 0);
  if (!Number.isSafeInteger(result)) throw new Error('Token sum exceeds exact integer range');
  return result;
}

export function totals(rows) {
  const paired = rows.filter(row => row.input_tokens !== null && row.cache_read_tokens !== null);
  const input = sum(paired.map(row => row.input_tokens));
  const read = sum(paired.map(row => row.cache_read_tokens));
  const costs = rows.filter(row => row.cost.status === 'estimated');
  const cost = costs.length ? costs.reduce((sum, row) => sum + row.cost.usd, 0) : null;
  if (cost !== null && !Number.isFinite(cost)) throw new Error('Cost sum overflow');
  return {
    attempts: rows.length, paired_attempts: paired.length,
    usage_unavailable_attempts: rows.length - paired.length,
    inclusive_input_tokens: sum(rows.flatMap(row => row.input_tokens === null ? [] : [row.input_tokens])),
    cache_read_tokens: sum(rows.flatMap(row => row.cache_read_tokens === null ? [] : [row.cache_read_tokens])),
    paired_input_tokens: input, paired_read_tokens: read,
    read_share_percent: input ? 100 * read / input : null,
    uncached_input_tokens: input === null ? null : input - read,
    fresh_input_tokens: sum(rows.flatMap(row => row.fresh_input_tokens === null ? [] : [row.fresh_input_tokens])),
    cache_write_tokens: sum(rows.flatMap(row => row.cache_write_tokens === null ? [] : [row.cache_write_tokens])),
    output_tokens: sum(rows.flatMap(row => row.output_tokens === null ? [] : [row.output_tokens])),
    field_unavailable_attempts: Object.fromEntries(
      ['input_tokens', 'cache_read_tokens', 'fresh_input_tokens', 'cache_write_tokens', 'output_tokens']
        .map(key => [key, rows.filter(row => row[key] === null).length])),
    cost: cost === null ? unavailable('pricing-or-usage-unavailable') : {
      status: 'estimated', usd: cost, scope: 'priced-attempts-only',
    },
    cost_unavailable_attempts: rows.length - costs.length,
  };
}

export function summarize(reports, window = null) {
  const allRows = [], unplaced = [], seen = new Set(), incomplete = [], physical = new Map();
  let copiedAttempts = 0;
  for (const report of reports) {
    if (report.schema !== 'secode.session-accounting.v1' || !Array.isArray(report.attempts)
        || typeof report.session !== 'string' || typeof report.enumeration_complete !== 'boolean'
        || report.attempts.length > 8192) {
      throw new Error('Invalid Product accounting export');
    }
    if (!report.enumeration_complete || report.session_totals?.status !== 'complete') incomplete.push(report.session);
    const requestAttempts = new Map(report.attempts.map(attempt => [attempt.attempt, attempt]));
    for (const attempt of report.attempts) {
      const identity = `${report.session}/${attempt.attempt}`;
      if (typeof attempt.attempt !== 'string' || seen.has(identity)) {
        throw new Error('Missing or duplicated Physical Attempt');
      }
      seen.add(identity);
      if (seen.size > 8192) throw new Error('Report exceeds 8192 attempts; narrow the window');
      const at = attempt.latency?.opened_ms;
      const usage = attempt.usage;
      const input = count(usage, 'input_total'), read = count(usage, 'cache_read');
      if (input !== null && read !== null && read > input) throw new Error('Cache read exceeds inclusive input');
      const cost = attempt.cost;
      if (cost?.status === 'estimated' && (!Number.isFinite(cost.usd) || cost.usd < 0
          || typeof cost.policy !== 'string')) throw new Error('Invalid estimated cost');
      const paired = input !== null && read !== null;
      const row = { session: report.session, sessions: [report.session], attempt: attempt.attempt,
        operation: attempt.operation, provider: attempt.provider ?? null, model: attempt.model ?? null,
        opened_ms: at ?? null, input_tokens: input, cache_read_tokens: read,
        read_share_percent: paired && input > 0 ? 100 * read / input : null,
        uncached_input_tokens: paired ? input - read : null,
        fresh_input_tokens: count(usage, 'input_fresh'), cache_write_tokens: count(usage, 'cache_write'),
        output_tokens: count(usage, 'output'), usage_observations: usage,
        usage_outcome: attempt.usage_outcome ?? unavailable('attempt-outcome-unavailable'),
        cost: cost?.status === 'estimated' ? cost : unavailable(cost?.reason ?? 'pricing-or-usage-unavailable'),
        miss_cause: diagnoseRequest(attempt, requestAttempts),
        request_fingerprint: attempt.request_cache ?? null,
        terminal_request_observed: attempt.terminal_request_observed ?? false,
        step_completed_ms: attempt.latency?.step_completed_ms ?? null,
        local_comparison: attempt.request_cache ? { status: 'durable', basis: 'hash-only-local-request-comparison' }
          : unavailable('durable-request-fingerprint-unavailable'),
      };
      const previous = physical.get(attempt.attempt);
      if (previous) {
        const coordinates = value => [value.operation, value.provider, value.model, value.opened_ms,
          value.usage_observations, value.cost, value.request_fingerprint,
          value.terminal_request_observed, value.step_completed_ms];
        if (!isDeepStrictEqual(coordinates(previous), coordinates(row))) {
          throw new Error(`Conflicting Product exports for Physical Attempt ${attempt.attempt}; measure settled sessions separately`);
        }
        previous.sessions.push(report.session); copiedAttempts++;
      } else { physical.set(attempt.attempt, row); allRows.push(row); }
    }
  }
  // The window is presentation, never an integrity fence: compare all copies first.
  const rows = allRows.filter(row => {
    if (!window) return true;
    if (!Number.isSafeInteger(row.opened_ms)) { unplaced.push(`${row.session}/${row.attempt}`); return false; }
    return row.opened_ms >= window.from && row.opened_ms < window.until;
  });
  rows.sort((a, b) => (a.opened_ms ?? 0) - (b.opened_ms ?? 0) || a.attempt.localeCompare(b.attempt));
  const models = new Map();
  for (const row of rows) {
    const key = JSON.stringify([row.provider, row.model]);
    if (!models.has(key)) models.set(key, []);
    models.get(key).push(row);
  }
  return {
    schema: 'grapt.seat-day.v1', window,
    basis: 'Product read-only durable usage; window includes attempts opened in [from, until)',
    coverage: 'observed-subtotals-only; dispatched attempts retain explicit unavailable usage outcomes',
    attempt_reconciliation: { status: incomplete.length === 0 && unplaced.length === 0 ? 'complete' : 'incomplete' },
    incomplete_sessions: incomplete, window_unplaced_attempts: unplaced,
    copied_attempt_exports: copiedAttempts,
    seat_day_cost: incomplete.length === 0 && unplaced.length === 0 && rows.length > 0
      && rows.every(row => row.cost.status === 'estimated' && row.cost.terminal_usage === true)
      ? { status: 'estimated', usd: totals(rows).cost.usd, scope: 'complete-selected-window',
          policies: [...new Set(rows.map(row => row.cost.policy))],
          basis: 'API-equivalent estimate; subscription charges and billing unreported' }
      : unavailable('complete-priced-attempt-coverage-unavailable'),
    totals: totals(rows),
    models: [...models.values()].map(group => ({ provider: group[0].provider, model: group[0].model, ...totals(group) })),
    requests: rows,
  };
}

export const CRITERION_5_MAX_INFRA_FAILURE_PERCENT = 3.0;

/**
 * Evaluate RELEASE-0.1.md Criteria #3 (matched tasks: first-review ACCEPT rate,
 * tokens/accepted task, and cache read share vs Codex baseline) and #5
 * (infrastructure tool failure rate <= 3%, zero unavailable/unreadable failure
 * detail artifacts, and zero skipped/unsupported tool terminal event versions).
 */
export function evaluateReleaseGates(graptReport, comparison = null) {
  const ops = graptReport?.operations;
  const toolCount = ops?.tool_calls?.count ?? 0;
  const infraFailedCount = ops?.tool_calls?.infrastructure_failed ?? null;
  const unavailableEntries = Object.entries(ops?.failure_detail_unavailable ?? {});
  const unavailableDetailCount = unavailableEntries.reduce((sum, [, count]) => sum + Number(count || 0), 0);
  const unnamedCount = Number(ops?.failure_detail_unavailable?.['detail-not-recorded'] ?? 0);
  const unsupportedVersions = ops?.coverage?.unsupported_event_versions ?? {};
  const unsupportedTerminalVersions = Object.entries(unsupportedVersions)
    .filter(([eventType]) => eventType.startsWith('session.tool.terminal.'))
    .reduce((sum, [, count]) => sum + Number(count || 0), 0);

  let criterion_5;
  if (toolCount <= 0) {
    criterion_5 = {
      status: 'unavailable',
      reason: 'no-tool-calls-observed',
      unnamed_terminal_failures: unnamedCount,
      unavailable_detail_artifacts: unavailableDetailCount,
      unsupported_terminal_versions: unsupportedTerminalVersions,
    };
  } else if (infraFailedCount === null) {
    criterion_5 = {
      status: 'unavailable',
      reason: 'infrastructure-failure-count-unavailable',
      unnamed_terminal_failures: unnamedCount,
      unavailable_detail_artifacts: unavailableDetailCount,
      unsupported_terminal_versions: unsupportedTerminalVersions,
    };
  } else {
    const infraRatePercent = (100 * infraFailedCount) / toolCount;
    const passed = infraRatePercent <= CRITERION_5_MAX_INFRA_FAILURE_PERCENT
      && unavailableDetailCount === 0
      && unsupportedTerminalVersions === 0;
    criterion_5 = {
      status: passed ? 'pass' : 'fail',
      tool_calls: toolCount,
      infrastructure_failed_tool_calls: infraFailedCount,
      infra_failure_rate_percent: Number(infraRatePercent.toFixed(4)),
      max_infra_failure_rate_percent: CRITERION_5_MAX_INFRA_FAILURE_PERCENT,
      unnamed_terminal_failures: unnamedCount,
      unavailable_detail_artifacts: unavailableDetailCount,
      unsupported_terminal_versions: unsupportedTerminalVersions,
    };
  }

  const codexReport = comparison && 'codexReport' in comparison ? comparison.codexReport : comparison;
  const matchedTasks = comparison && 'matchedTasks' in comparison ? comparison.matchedTasks : null;
  const graptShare = graptReport?.totals?.read_share_percent ?? null;
  const codexShare = codexReport?.totals?.read_share_percent ?? null;

  let criterion_3;
  if (graptShare === null || codexShare === null) {
    criterion_3 = {
      status: 'unavailable',
      reason: codexReport ? 'cache-read-share-unavailable' : 'comparison-baseline-not-provided',
      grapt_read_share_percent: graptShare === null ? null : Number(graptShare.toFixed(4)),
      codex_read_share_percent: codexShare === null ? null : Number(codexShare.toFixed(4)),
    };
  } else {
    const cacheShareOk = graptShare >= codexShare;
    const hasMatchedTasks = matchedTasks !== null
      && Number.isSafeInteger(matchedTasks.count) && matchedTasks.count > 0
      && typeof matchedTasks.grapt_first_review_accept_rate === 'number'
      && typeof matchedTasks.codex_first_review_accept_rate === 'number'
      && typeof matchedTasks.grapt_tokens_per_accepted_task === 'number'
      && typeof matchedTasks.codex_tokens_per_accepted_task === 'number';
    if (!hasMatchedTasks) {
      criterion_3 = {
        status: cacheShareOk ? 'incomplete' : 'fail',
        reason: 'matched-task-metrics-missing',
        cache_share_status: cacheShareOk ? 'pass' : 'fail',
        grapt_read_share_percent: Number(graptShare.toFixed(4)),
        codex_read_share_percent: Number(codexShare.toFixed(4)),
        cache_share_delta_percent: Number((graptShare - codexShare).toFixed(4)),
      };
    } else {
      const acceptOk = matchedTasks.grapt_first_review_accept_rate >= matchedTasks.codex_first_review_accept_rate;
      const tokensOk = matchedTasks.grapt_tokens_per_accepted_task <= matchedTasks.codex_tokens_per_accepted_task;
      criterion_3 = {
        status: cacheShareOk && acceptOk && tokensOk ? 'pass' : 'fail',
        matched_tasks: matchedTasks.count,
        grapt_read_share_percent: Number(graptShare.toFixed(4)),
        codex_read_share_percent: Number(codexShare.toFixed(4)),
        cache_share_delta_percent: Number((graptShare - codexShare).toFixed(4)),
        grapt_first_review_accept_rate: matchedTasks.grapt_first_review_accept_rate,
        codex_first_review_accept_rate: matchedTasks.codex_first_review_accept_rate,
        grapt_tokens_per_accepted_task: matchedTasks.grapt_tokens_per_accepted_task,
        codex_tokens_per_accepted_task: matchedTasks.codex_tokens_per_accepted_task,
      };
    }
  }

  return { criterion_3, criterion_5 };
}
