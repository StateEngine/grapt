"""Native call evidence and cache metrics; absent evidence remains unknown."""
from decimal import Decimal
import json
import math
from pathlib import Path

MAX_CALLS = 256


def valid_usage(value):
    return (isinstance(value, dict)
            and all(type(value.get(k)) is int and 0 <= value[k] <= 2**53
                    for k in ['input_tokens', 'cached_input_tokens'])
            and value['cached_input_tokens'] <= value['input_tokens'])


class Calls:
    """One bounded projection of unique native model messages, in observed order."""
    def __init__(self, harness):
        self.harness = harness
        self.calls = {}
        self.invalid = False

    def observe(self, row):
        if not isinstance(row, dict):
            return
        if self.harness == 'opencode' and row.get('type') == 'step_finish':
            message = row.get('part', {})
            if not isinstance(message, dict):
                self.invalid = True
                return
            tokens = message.get('tokens', {})
            if not isinstance(tokens, dict) or not isinstance(tokens.get('cache'), dict):
                self.invalid = True
                return
            cache = tokens['cache']
            identity = message.get('messageID')
            values = [tokens.get('input'), cache.get('read'), cache.get('write')]
        elif self.harness == 'claude' and row.get('type') == 'assistant':
            message = row.get('message', {})
            if not isinstance(message, dict) or not isinstance(message.get('usage'), dict):
                self.invalid = True
                return
            tokens = message['usage']
            identity = message.get('id')
            values = [tokens.get('input_tokens'), tokens.get('cache_read_input_tokens'),
                      tokens.get('cache_creation_input_tokens')]
        else:
            return
        if (not isinstance(identity, str) or not identity or len(identity) > 128
                or not all(type(v) is int and 0 <= v <= 2**53 for v in values)):
            self.invalid = True
            return
        call = {'identity': identity, 'input_tokens': sum(values),
                'cached_input_tokens': values[1], 'cache_write_tokens': values[2]}
        if self.harness == 'claude':
            duration = tokens.get('cache_creation')
            duration = duration if isinstance(duration, dict) else {}
            partition = [duration.get('ephemeral_5m_input_tokens'), duration.get('ephemeral_1h_input_tokens')]
            if all(type(v) is int and v >= 0 for v in partition) and sum(partition) == values[2]:
                call['cache_write_durations'] = dict(zip(['5m', '1h'], partition))
        if identity in self.calls and self.calls[identity] != call:
            self.invalid = True
        elif len(self.calls) >= MAX_CALLS and identity not in self.calls:
            self.invalid = True
        else:
            self.calls[identity] = call

    def result(self, usage):
        calls = list(self.calls.values())
        if (self.invalid or not calls or not valid_usage(usage)
                or any(sum(c[k] for c in calls) != usage[k]
                       for k in ['input_tokens', 'cached_input_tokens'])):
            return None
        return calls


def prices():
    path = Path(__file__).with_name('cache-prices.json')
    document = json.loads(path.read_text())
    return document['models']


def billed_cost(row):
    billing = row.get('provider_billing')
    billed = None
    if isinstance(billing, dict) and billing.get('provenance') == 'provider-reported':
        try:
            observed = Decimal(str(billing['usd']))
            if observed.is_finite() and observed >= 0:
                amount = float(observed)
                billed = amount if math.isfinite(amount) else None
        except (KeyError, ArithmeticError, ValueError):
            pass
    return billed


def row_metrics(row, model, table):
    usage = row.get('usage')
    if not valid_usage(usage):
        return None
    calls = row.get('call_usage')
    known_calls = (isinstance(calls, list) and 0 < len(calls) <= MAX_CALLS
                   and all(valid_usage(c) and isinstance(c.get('identity'), str)
                           and 0 < len(c['identity']) <= 128 for c in calls)
                   and len({c['identity'] for c in calls}) == len(calls)
                   and all(sum(c[k] for c in calls) == usage[k]
                           for k in ['input_tokens', 'cached_input_tokens']))
    first = calls[0] if known_calls else None
    # Legacy telemetry defaulted missing writes to zero; it is not evidence.
    writes = row.get('cache_write_tokens_observed')
    if not (type(writes) is int and 0 <= writes <= usage['input_tokens']-usage['cached_input_tokens']):
        writes = None
    uncached = usage['input_tokens']-usage['cached_input_tokens']-writes if writes is not None else None
    quote = table.get(model)
    weight = None
    if quote:
        fresh, cached = Decimal(quote['input_usd_per_million']), Decimal(quote['cache_read_usd_per_million'])
        if fresh > 0 and cached >= 0:
            weight = cached/fresh
    equivalent = None
    if weight is not None and uncached is not None:
        if writes == 0 or quote.get('price_model', model).startswith('openai/'):
            premium = Decimal(0)
        else:
            durations = row.get('cache_write_durations')
            if (not isinstance(durations, dict)
                    or set(durations) != {'5m', '1h'}
                    or not all(type(durations.get(k)) is int and durations[k] >= 0 for k in ['5m', '1h'])
                    or sum(durations.values()) != writes):
                durations = None
            raw = quote['raw_pricing']
            if durations is not None and all(key in raw for key in ['input_cache_write', 'input_cache_write_1h']):
                premium = sum(Decimal(durations[k])*Decimal(raw[price])/Decimal(raw['prompt'])
                              for k, price in [('5m', 'input_cache_write'), ('1h', 'input_cache_write_1h')])
            else:
                premium = None
        if premium is not None:
            equivalent = float(Decimal(uncached)+weight*usage['cached_input_tokens']+premium)
    billed = billed_cost(row)
    return {'uncached_input_tokens': uncached,
            'calls': len(calls) if known_calls else None,
            'uncached_input_per_call': uncached/len(calls) if known_calls and uncached is not None else None,
            'first_call_cached_share': (first['cached_input_tokens']/first['input_tokens']
                                        if first and first['input_tokens'] else None),
            'cache_read_share': usage['cached_input_tokens']/usage['input_tokens'] if usage['input_tokens'] else None,
            'cache_read_weight': float(weight) if weight is not None else None,
            'input_equivalent': equivalent, 'provider_billed_usd': billed,
            'output_tokens': row['usage'].get('output_tokens')}


def summarize(rows, model, table=None):
    """Completed native workloads are the run denominator, including coding failures."""
    completed = [r for r in rows if r.get('status') == 'completed']
    values = [row_metrics(r, model, table if table is not None else prices()) for r in completed]
    known = [v for v in values if v is not None]
    calls = [v for v in known if v['uncached_input_per_call'] is not None]
    uncached = [v['uncached_input_tokens'] for v in known if v['uncached_input_tokens'] is not None]
    first = [v['first_call_cached_share'] for v in known if v['first_call_cached_share'] is not None]
    equivalent = [v['input_equivalent'] for v in known if v['input_equivalent'] is not None]
    bills = [billed_cost(r) for r in completed]
    billed = [v for v in bills if v is not None]
    output = [v['output_tokens'] for v in known
              if type(v['output_tokens']) is int and 0 <= v['output_tokens'] <= 2**53]
    total_input = sum(r['usage']['input_tokens'] for r in completed if valid_usage(r.get('usage')))
    total_cached = sum(r['usage']['cached_input_tokens'] for r in completed if valid_usage(r.get('usage')))
    def mean(values):
        return sum(values)/len(values) if values and len(values) == len(completed) else None
    return {'model': model, 'native_completed': len(completed), 'usage_coverage': len(known),
            'uncached_input_per_completed_run': mean(uncached), 'uncached_coverage_runs': len(uncached),
            'uncached_input_per_call': sum(v['uncached_input_tokens'] for v in calls)/sum(v['calls'] for v in calls)
                                       if calls and len(calls) == len(completed) else None,
            'call_coverage_runs': len(calls), 'calls_observed': sum(v['calls'] for v in known if v['calls'] is not None)
                              if any(v['calls'] is not None for v in known) else None,
            'first_call_cached_share_mean': mean(first),
            'first_call_coverage_runs': len(first),
            'input_equivalent_per_completed_run': mean(equivalent),
            'price_coverage_runs': len(equivalent),
            'provider_billed_usd_total': sum(billed) if billed and len(billed) == len(completed) else None,
            'billed_coverage_runs': len(billed), 'output_per_completed_run': mean(output),
            'output_coverage_runs': len(output),
            'cache_read_share': total_cached/total_input if total_input and len(known) == len(completed) else None}
