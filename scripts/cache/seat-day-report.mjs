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

function toEpochMs(value) {
  if (typeof value === 'number' && Number.isFinite(value)) return value;
  if (typeof value === 'string' && /^\d{4}-\d{2}-\d{2}/.test(value)) {
    const parsed = Date.parse(value);
    return Number.isFinite(parsed) ? parsed : null;
  }
  return null;
}

function isoDayOf(value) {
  const ms = toEpochMs(value);
  return ms === null ? null : new Date(ms).toISOString().slice(0, 10);
}

function extractRecordedWindow(report) {
  const windowPresent = report?.window !== null && report?.window !== undefined;
  if (windowPresent && (typeof report.window !== 'object' || Array.isArray(report.window))) {
    return { day: null, fromMs: null, untilMs: null, invalid: true };
  }
  const hasWindow = windowPresent;
  let fromMs = null;
  let untilMs = null;
  let windowDay = null;
  if (hasWindow) {
    fromMs = toEpochMs(report.window.from);
    untilMs = toEpochMs(report.window.until);
    if (fromMs === null || untilMs === null || untilMs <= fromMs) {
      return { day: null, fromMs, untilMs, invalid: true };
    }
    windowDay = isoDayOf(fromMs);
    // Half-open [from, until) must fall within a single UTC calendar day.
    const endDay = isoDayOf(untilMs - 1);
    if (endDay !== windowDay) {
      return { day: null, fromMs, untilMs, invalid: true };
    }
  }
  if (Array.isArray(report?.requests) && report.requests.length > 0) {
    const days = new Set();
    for (const row of report.requests) {
      const ts = toEpochMs(row?.opened_ms ?? row?.started_ms ?? row?.reported_ms ?? row?.at);
      if (ts === null) return { day: null, fromMs, untilMs, invalid: true };
      if (fromMs !== null && ts < fromMs) return { day: null, fromMs, untilMs, invalid: true };
      if (untilMs !== null && ts >= untilMs) return { day: null, fromMs, untilMs, invalid: true };
      days.add(isoDayOf(ts));
    }
    if (days.size !== 1) return { day: null, fromMs, untilMs, invalid: true };
    const reqDay = [...days][0];
    if (windowDay !== null && reqDay !== windowDay) {
      return { day: null, fromMs, untilMs, invalid: true };
    }
    return { day: windowDay ?? reqDay, fromMs, untilMs, invalid: false };
  }
  return { day: windowDay, fromMs, untilMs, invalid: false };
}

function extractDay(recordedWindow, explicitDay) {
  if (recordedWindow.invalid || recordedWindow.day === null) return null;
  const explicit = typeof explicitDay === 'string' && /^\d{4}-\d{2}-\d{2}$/.test(explicitDay) ? explicitDay : null;
  // Never let a comparison label override the export's recorded date.
  if (explicit !== null && explicit !== recordedWindow.day) return null;
  return recordedWindow.day;
}

function windowsMatch(graptWindow, codexWindow) {
  if (graptWindow.invalid || codexWindow.invalid) return false;
  const graptWindowed = graptWindow.fromMs !== null;
  const codexWindowed = codexWindow.fromMs !== null;
  if (graptWindowed !== codexWindowed) return false;
  if (graptWindowed && codexWindowed) {
    return graptWindow.untilMs !== null
      && codexWindow.untilMs !== null
      && graptWindow.fromMs === codexWindow.fromMs
      && graptWindow.untilMs === codexWindow.untilMs;
  }
  return true;
}

function extractRecordedModel(report) {
  if (Array.isArray(report?.models)) {
    const names = new Set();
    for (const entry of report.models) {
      if (typeof entry?.model === 'string' && entry.model.trim().length > 0) {
        names.add(entry.model.trim());
      }
    }
    if (names.size === 1) return [...names][0];
    return null;
  }
  if (typeof report?.model === 'string' && report.model.trim().length > 0) return report.model.trim();
  const models = report?.by_model && typeof report.by_model === 'object' ? Object.keys(report.by_model) : [];
  return models.length === 1 ? models[0] : null;
}

function extractModel(report, explicitModel) {
  const explicit = typeof explicitModel === 'string' && explicitModel.trim().length > 0 ? explicitModel.trim() : null;
  const recorded = extractRecordedModel(report);
  if (recorded !== null) {
    // Never let a comparison label override the export's recorded model.
    if (explicit !== null && explicit !== recorded) return null;
    return recorded;
  }
  // Multi-model exports cannot be relabelled into a single-model comparison.
  if (Array.isArray(report?.models) && report.models.length > 1) return null;
  return explicit;
}

export const CRITERION_3_MIN_MATCHED_TASKS = 3;
export const CRITERION_3_MAX_VARIANCE_RATIO = 0.20;
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
      && typeof matchedTasks === 'object'
      && typeof matchedTasks.count === 'number'
      && typeof matchedTasks.grapt_accepted_tasks === 'number'
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
      const graptWindow = extractRecordedWindow(graptReport);
      const codexWindow = extractRecordedWindow(codexReport);
      const graptDay = extractDay(graptWindow, matchedTasks.grapt_day);
      const codexDay = extractDay(codexWindow, matchedTasks.codex_day);
      const graptModel = extractModel(graptReport, matchedTasks.grapt_model);
      const codexModel = extractModel(codexReport, matchedTasks.codex_model);
      const validCounts = Number.isSafeInteger(matchedTasks.count) && matchedTasks.count > 0
        && Number.isSafeInteger(matchedTasks.grapt_accepted_tasks)
        && matchedTasks.grapt_accepted_tasks >= 0
        && matchedTasks.grapt_accepted_tasks <= matchedTasks.count;
      const validRates = Number.isFinite(matchedTasks.grapt_first_review_accept_rate)
        && matchedTasks.grapt_first_review_accept_rate >= 0
        && matchedTasks.grapt_first_review_accept_rate <= 1
        && Number.isFinite(matchedTasks.codex_first_review_accept_rate)
        && matchedTasks.codex_first_review_accept_rate >= 0
        && matchedTasks.codex_first_review_accept_rate <= 1;
      const validTokens = Number.isFinite(matchedTasks.grapt_tokens_per_accepted_task)
        && matchedTasks.grapt_tokens_per_accepted_task > 0
        && Number.isFinite(matchedTasks.codex_tokens_per_accepted_task)
        && matchedTasks.codex_tokens_per_accepted_task > 0;
      const sameDay = graptDay !== null && codexDay !== null && graptDay === codexDay
        && windowsMatch(graptWindow, codexWindow);
      const sameModel = graptModel !== null && codexModel !== null && graptModel === codexModel;
      const enoughTasks = matchedTasks.count >= CRITERION_3_MIN_MATCHED_TASKS;
      const allAccepted = matchedTasks.grapt_accepted_tasks === matchedTasks.count;
      const acceptFloor = matchedTasks.codex_first_review_accept_rate * (1 - CRITERION_3_MAX_VARIANCE_RATIO);
      const tokensCeiling = matchedTasks.codex_tokens_per_accepted_task * (1 + CRITERION_3_MAX_VARIANCE_RATIO);
      const acceptOk = matchedTasks.grapt_first_review_accept_rate >= acceptFloor - 1e-12;
      const tokensOk = matchedTasks.grapt_tokens_per_accepted_task <= tokensCeiling + 1e-12;
      const reason = (!validCounts || !validRates || !validTokens)
        ? 'invalid-matched-task-metrics'
        : (!sameDay
            ? 'mismatched-comparison-day'
            : (!sameModel
                ? 'mismatched-comparison-model'
                : (!enoughTasks
                    ? 'insufficient-matched-tasks'
                    : (!allAccepted ? 'unaccepted-matched-tasks' : null))));
      const passed = reason === null && cacheShareOk && acceptOk && tokensOk;
      criterion_3 = {
        status: passed ? 'pass' : 'fail',
        reason,
        grapt_day: graptDay,
        codex_day: codexDay,
        grapt_model: graptModel,
        codex_model: codexModel,
        matched_tasks: matchedTasks.count,
        min_matched_tasks: CRITERION_3_MIN_MATCHED_TASKS,
        grapt_accepted_tasks: matchedTasks.grapt_accepted_tasks,
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
