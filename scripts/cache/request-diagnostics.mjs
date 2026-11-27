// Local equality evidence can identify a changed component; providers do not report eviction causes.
const absent = reason => ({ status: 'unavailable', reason });
const components = ['target', 'cacheKey', 'routing', 'tools', 'instructions', 'options', 'input'];
const hash = value => typeof value === 'string' && /^[0-9a-f]{64}$/.test(value);

function fingerprint(attempt) {
  const observation = attempt.request_cache;
  if (observation == null) return null;
  const record = observation.record;
  if (!Number.isSafeInteger(observation.admitted_ms) || record?.attempt !== attempt.attempt
      || !Number.isSafeInteger(record.step) || record.step < 1
      || (observation.previous_attempt !== null && typeof observation.previous_attempt !== 'string')) {
    throw new Error('Invalid durable request coordinates');
  }
  const fp = record.fingerprint;
  if (fp === null) return null;
  if (!fp || Object.keys(fp).length !== 8 || !components.every(key => hash(fp[key]))
      || !Number.isSafeInteger(fp.inputItems) || fp.inputItems < 0 || fp.inputItems > 0xffffffff) throw new Error('Invalid bounded request fingerprint');
  return fp;
}
function observed(attempt, name) {
  const field = attempt.usage?.[name];
  return field?.status === 'observed'
    && ['provider-reported', 'provider-derived'].includes(field.provenance?.kind)
    && Number.isSafeInteger(field.tokens) && field.tokens >= 0 ? field.tokens : null;
}

export function diagnoseRequest(attempt, attempts) {
  const current = fingerprint(attempt);
  if (!current) return absent('durable-request-fingerprint-unavailable');
  if (!attempt.terminal_request_observed) return absent('request-terminal-unavailable');
  const input = observed(attempt, 'input_total'), read = observed(attempt, 'cache_read');
  if (input === null || read === null) return absent('cache-usage-unreported');
  if (read > input) throw new Error('Cache read exceeds provider input');
  if (read === input) return { status: 'no-miss', basis: 'provider-reported-usage', uncached_input_tokens: 0 };
  const predecessor = attempt.request_cache.previous_attempt;
  if (predecessor === null) return absent('first-request-observation');
  const previous = attempts.get(predecessor);
  if (!previous) return absent('predecessor-not-enumerated');
  const prior = fingerprint(previous);
  if (!prior) return absent('predecessor-fingerprint-unavailable');
  if (!previous.terminal_request_observed) return absent('predecessor-terminal-unavailable');
  const previousRead = observed(previous, 'cache_read');
  if (observed(previous, 'input_total') === null || previousRead === null) return absent('predecessor-cache-usage-unreported');
  const priorEnd = previous.latency?.step_completed_ms;
  const gap = Number.isSafeInteger(priorEnd) ? attempt.request_cache.admitted_ms - priorEnd : null;
  // Step completion may follow a retry/summary admission. It cannot measure that interval.
  const idle = Number.isSafeInteger(gap) && gap >= 0 ? gap : null;
  const comparison = attempt.request_cache.comparison;
  if (!comparison) return absent('request-comparison-unavailable');
  const { changed_components: changed, stable_input_items: stable, stable_input_hash: boundary,
    complete: exact, append_only: append, identical } = comparison;
  const headers = components.filter(key => key !== 'input');
  if (!Array.isArray(changed) || changed.length > 7 || new Set(changed).size !== changed.length
      || !changed.every(key => components.includes(key))
      || !Number.isInteger(stable) || stable < 0 || stable > Math.min(identical ? 0xffffffff : 512, current.inputItems, prior.inputItems)
      || (stable === 0 ? boundary !== null : !hash(boundary))
      || ![exact, append, identical].every(value => typeof value === 'boolean')
      || headers.some(key => changed.includes(key) !== (current[key] !== prior[key]))
      || identical !== (current.input === prior.input)
      || append !== (stable === prior.inputItems && current.inputItems > prior.inputItems)) {
    throw new Error('Invalid request comparison summary');
  }
  const base = {
    status: 'inferred', basis: 'durable-local-request-comparison', provider_cause: 'unreported',
    previous_attempt: predecessor, changed_components: changed,
    stable_input_prefix_items: stable, stable_input_prefix_hash: boundary,
    stable_input_hash_basis: identical ? 'complete-input' : 'chained-items',
    prefix_comparison_complete: exact, idle_gap_ms: idle,
  };
  if (changed.length) return { ...base, kind: changed.includes('tools') ? 'tool-list-change' : 'prefix-break' };
  if (exact && idle > 0 && read !== null && previousRead !== null && read < previousRead) {
    return { ...base, kind: 'ttl-or-idle-gap', confidence: 'candidate; provider-eviction-unreported' };
  }
  if (append) {
    return { ...base, kind: 'new-suffix' };
  }
  if (current.input === prior.input) {
    return { ...base, status: 'unavailable', reason: 'identical-request; provider-miss-cause-unreported' };
  }
  return { ...base, status: 'unavailable', reason: 'request-prefix-comparison-bound' };
}
