#!/usr/bin/env python3
"""Summarize every planned attempt, including missing, failed and timed-out work."""
import argparse
import json
from pathlib import Path
import statistics
import cache_metrics


LIVE_KINDS = ('agent_message_chunk', 'tool_call', 'tool_call_update', 'thought')


def live_counts(record):
    if record.get('native_harness', record.get('harness')) != 'grapt':
        return None
    counts = record.get('live_acp_updates')
    if (not isinstance(counts, dict)
            or not all(type(counts.get(kind)) is int and counts[kind] >= 0 for kind in LIVE_KINDS)):
        return None
    return counts


def streaming_defect(record):
    """Only completed SE attempts with observed counts can establish this defect."""
    counts = live_counts(record)
    if counts is None or record.get('status') != 'completed':
        return None
    return counts['agent_message_chunk'] == 0


def combined(reports):
    """Join disjoint development segments without replacing failed attempts."""
    first = reports[0]
    tasks, harnesses, records = [], [], []
    adapters = {}
    for report in reports:
        if report.get('corpus') != first.get('corpus'):
            raise ValueError('Cannot compare segments from different task corpora')
        if not report.get('planned_tasks') or not report.get('planned_harnesses'):
            raise ValueError('Report must preserve planned task and harness sets')
        for key, value in report.get('adapters', {}).items():
            if key in adapters and adapters[key] != value:
                raise ValueError('Changed model/authentication requires a separate comparison')
            adapters[key] = value
        tasks.extend(task for task in report['planned_tasks'] if task not in tasks)
        harnesses.extend(key for key in report['planned_harnesses'] if key not in harnesses)
        records.extend(report['attempts'])
    # summary rejects duplicate attempt keys, including a purported retry.
    result = summary(records, harnesses, tasks, adapters)
    return tasks, harnesses, records, adapters, result


def markdown(reports):
    tasks, harnesses, records, adapters, result = combined(reports)
    complete = all(report.get('held_out_included') for report in reports)
    title = 'Weekly harness benchmark' if complete else 'Development harness benchmark'
    lines = [f'# {title}', '',
             f'{len(tasks)} planned tasks × {len(harnesses)} harness rows. '
             f'{len(records)} attempt records; missing attempts remain in the denominator.', '',
             f'Measurement status: {"complete" if len(records) == len(tasks)*len(harnesses) else "partial"} '
             f'({len(records)}/{len(tasks)*len(harnesses)} planned outcomes recorded). '
             'Diff review and native compaction coverage are separate criteria.', '',
             'No retries. Test outcomes and diff quality are reported separately. '
             'Unknown telemetry is shown as unknown.', '',
             '| Harness row | Cohort | Successful attempts | Native completed | Hidden checks passed | Agent median | Diff quality reviewed | Crashes / timeouts / missing | Streaming defects |',
             '|---|---|---:|---:|---:|---:|---:|---:|---:|']
    for harness, row in result.items():
        median = row['wall_seconds_median']
        timing = f'{median:.1f}s ({row["wall_seconds_coverage"]}/{row["planned"]})' if median is not None else 'unknown'
        counts = row['status_counts']
        failures = ' / '.join(str(counts.get(status, 0)) for status in ['crash', 'timeout', 'missing'])
        defects = ('unknown' if row['streaming_defects'] is None else str(row['streaming_defects']))
        defects += f' ({row["streaming_defect_coverage"]} completed evaluated)'
        if adapters.get(harness, {}).get('harness', harness) != 'grapt':
            defects = 'n/a'
        lines.append(f'| {harness} | {adapters.get(harness, {}).get("cohort", "unknown")} | '
                     f'{row["successful_attempts"]}/{row["planned"]} | '
                     f'{row["native_completed"]}/{row["planned"]} | '
                     f'{row["hidden_tests_passed"]}/{row["planned"]} '
                     f'({row["hidden_tests_coverage"]} evaluated) | {timing} | '
                     f'{row["quality_reviewed"]}/{row["planned"]} | {failures} | {defects} |')
    pending = []
    for harness, row in result.items():
        missing = row['status_counts'].get('missing', 0)
        if missing:
            reasons = list(dict.fromkeys(report.get('deferred_harnesses', {}).get(harness)
                                        for report in reports
                                        if report.get('deferred_harnesses', {}).get(harness)))
            pending.append(f'- {harness}: {missing}/{row["planned"]} pending. '
                           + ('; '.join(reasons) if reasons else 'Not yet recorded.'))
    if pending:
        lines += ['', '## Pending attempts', '', *pending]
    lines += ['', '## Input efficiency', '',
              'Means use native-completed workloads, including completed coding failures. '
              'Uncached input excludes cache reads and writes. Per-call input includes every '
              'observed Physical Attempt, including compaction summaries. First-call share '
              'is the mean of each completed workload’s first model call. Coverage is '
              'shown as known/completed; incomplete coverage makes the aggregate unknown.', '',
              'Input-cost-equivalent = uncached + verified read weight × cache reads + '
              'verified write weight × writes, using the requested write duration. '
              'Unknown write counts/durations stay unknown. OpenAI routes have zero write '
              'premium; output is separate. Frozen prices are in bench/cache-prices.json. '
              'Equivalent tokens are not subscription billing. Billed $ requires an explicit '
              'provider-reported cost; native price estimates and shared-key deltas do not qualify.', '',
              '| Harness row | Model | Uncached/run | Uncached/call | First-call cached | Input-cost-equivalent/run | Output/run | Billed $ total | Cache share |',
              '|---|---|---:|---:|---:|---:|---:|---:|---:|']
    for harness, row in result.items():
        for metric in row['cache_metrics']:
            def cell(key, coverage, percent=False):
                value = metric[key]
                shown = ('unknown' if value is None else f'{value*100:.2f}%' if percent
                         else f'{value:.6f}' if key == 'provider_billed_usd_total' else f'{value:,.2f}')
                return f'{shown} ({metric[coverage]}/{metric["native_completed"]})'
            lines.append(f'| {harness} | {metric["model"]} | '
                         + ' | '.join([cell('uncached_input_per_completed_run', 'uncached_coverage_runs'),
                             cell('uncached_input_per_call', 'call_coverage_runs'),
                             cell('first_call_cached_share_mean', 'first_call_coverage_runs', True),
                             cell('input_equivalent_per_completed_run', 'price_coverage_runs'),
                             cell('output_per_completed_run', 'output_coverage_runs'),
                             cell('provider_billed_usd_total', 'billed_coverage_runs'),
                             cell('cache_read_share', 'usage_coverage', True)]) + ' |')
    se_rows = [r for r in records if r.get('native_harness', r['harness']) == 'grapt']
    if se_rows:
        lines += ['', '## Live ACP streaming', '',
                  'Counts come from captured session/update notifications. Thought counts '
                  'are ACP agent_thought_chunk events. A completed SE row with zero '
                  'agent_message_chunk updates has a streaming defect, independently of task success. '
                  'Missing counts and non-completed rows are not evaluated for this flag.', '',
                  '| Harness row | Task | Native status | Agent chunks | Tool calls | Tool updates | Thought | Streaming defect |',
                  '|---|---|---|---:|---:|---:|---:|---|']
        for record in se_rows:
            observed = live_counts(record)
            values = [str(observed[kind]) if observed is not None else 'unknown' for kind in LIVE_KINDS]
            flag = streaming_defect(record)
            verdict = 'yes' if flag is True else 'no' if flag is False else 'not evaluated'
            lines.append(f'| {record["harness"]} | {record["task"]} | {record["status"]} | '
                         + ' | '.join(values) + f' | {verdict} |')
    lines += ['', '## Models, routes and provenance', '',
              'Requested models are distinct from observed provider identity. '
              'Subscription spend is distinct from API-equivalent estimates.', '']
    for harness in harnesses:
        adapter = adapters.get(harness, {})
        lines.append(f'- {harness}: requested `{adapter.get("model", "unknown")}` via '
                     f'`{adapter.get("auth", "unknown")}`.')
    for index, report in enumerate(reports, 1):
        lines += ['', f'Segment {index}: started {report.get("started_utc", "unknown")}; '
                  f'authorization `{report.get("authorization_ref", "unknown")}`; '
                  f'corpus `{report.get("corpus", "unknown")}`.', '',
                  f'Native versions: `{json.dumps(report.get("harness_versions", {}), sort_keys=True)}`.']
        for note in report.get('notes', []):
            lines.append(f'- {note}')
    if any('cache_metrics_replay' in r for r in reports):
        lines += ['', '## Offline replay', '',
                  'This is a separate replay of the saved October 7 weekly. Every original '
                  'attempt field and summary value is unchanged. New native call metadata '
                  'is pinned in the data file. Historical Sonnet remains Sonnet; the next '
                  'weekly uses Haiku. Historical SE usage is unavailable, Codex exposes '
                  'whole-run usage, and OpenCode does not report Anthropic cache-write duration. '
                  'Billed cost is unavailable in these native logs. No model calls were made.']
    lines += ['', '## Telemetry and limitations', '',
              'Coverage counts include only observed telemetry. No event is not proof '
              'that compaction never occurred. Context stress does not close the '
              'charter compaction journey.', '', '```json',
              json.dumps(result, indent=2), '```', '']
    return '\n'.join(lines)


def summary(records, harnesses, tasks, adapters=None):
    indexed = {}
    for record in records:
        key = (record['harness'], record['task'])
        if key in indexed:
            raise ValueError(f'Duplicate attempt: {key}; retries are not part of this benchmark')
        if key[0] not in harnesses or key[1] not in tasks:
            raise ValueError(f'Unplanned attempt: {key}')
        indexed[key] = record
    result = {}
    table = cache_metrics.prices()
    for harness in harnesses:
        rows = [indexed.get((harness, task), {'status': 'missing', 'success': False}) for task in tasks]
        times = [r['wall_seconds'] for r in rows if isinstance(r.get('wall_seconds'), (int, float))]
        costs = [r['api_equivalent_cost_usd'] for r in rows
                 if isinstance(r.get('api_equivalent_cost_usd'), (int, float))]
        reviewed = [r for r in rows if isinstance(r.get('diff_quality'), (int, float))]
        interventions = [r['interventions'] for r in rows if type(r.get('interventions')) is int]
        compactions = [r['observed_compactions'] for r in rows if type(r.get('observed_compactions')) is int]
        live = [live_counts(r) for r in rows if live_counts(r) is not None]
        streaming = [streaming_defect(r) for r in rows if streaming_defect(r) is not None]
        evaluated = [r for r in rows
                     if r.get('test_status') in {'completed', 'failed', 'crash', 'timeout'}
                     and type(r.get('test_exit')) is int]
        tokens = {}
        token_coverage = {}
        for field in ['input_tokens', 'cached_input_tokens', 'output_tokens']:
            known = [r['usage'][field] for r in rows if isinstance(r.get('usage'), dict)
                     and type(r['usage'].get(field)) is int]
            tokens[field] = sum(known) if len(known) == len(tasks) else None
            token_coverage[field] = len(known)
        result[harness] = {
            'planned': len(tasks),
            'successful_attempts': sum(r.get('success', False) for r in rows),
            'native_completed': sum(r['status'] == 'completed' for r in rows),
            'hidden_tests_passed': sum(r['test_status'] == 'completed' and r['test_exit'] == 0
                                       for r in evaluated),
            'hidden_tests_coverage': len(evaluated),
            'success_rate': sum(r.get('success', False) for r in rows)/len(tasks),
            'status_counts': {status: sum(r['status'] == status for r in rows)
                              for status in sorted({r['status'] for r in rows})},
            'wall_seconds_median': statistics.median(times) if times else None,
            'wall_seconds_coverage': len(times),
            'api_equivalent_cost_usd_total': sum(costs) if len(costs) == len(tasks) else None,
            'cost_coverage': len(costs),
            'quality_reviewed': len(reviewed),
            'diff_quality_mean': statistics.mean(r['diff_quality'] for r in reviewed) if reviewed else None,
            'interventions': sum(interventions) if len(interventions) == len(tasks) else None,
            'interventions_coverage': len(interventions),
            'tokens': tokens,
            'token_coverage': token_coverage,
            'observed_compactions': sum(compactions) if len(compactions) == len(tasks) else None,
            'compaction_coverage': len(compactions),
            'live_acp_updates': {kind: sum(counts[kind] for counts in live)
                                 if len(live) == len(tasks) else None for kind in LIVE_KINDS},
            'live_update_coverage': len(live),
            'streaming_defects': sum(streaming) if streaming else None,
            'streaming_defect_coverage': len(streaming),
        }
        groups = {}
        for row in rows:
            model = row.get('requested_model') or (adapters or {}).get(harness, {}).get('model', 'unknown')
            groups.setdefault(model, []).append(row)
        result[harness]['cache_metrics'] = [cache_metrics.summarize(group, model, table)
                                          for model, group in groups.items()]
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('results', type=Path, nargs='+')
    parser.add_argument('--markdown', type=Path, help='publish one report, optionally joining disjoint development segments')
    args = parser.parse_args()
    reports = [json.loads(path.read_text()) for path in args.results]
    try:
        result = combined(reports)[-1]
        if args.markdown:
            args.markdown.parent.mkdir(parents=True, exist_ok=True)
            args.markdown.write_text(markdown(reports))
        print(json.dumps(result, indent=2))
    except ValueError as error:
        parser.error(str(error))


if __name__ == '__main__':
    main()
