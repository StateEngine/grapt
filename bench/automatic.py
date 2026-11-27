"""Plan or execute the separate C04 development pressure profile once."""
import argparse
from decimal import Decimal
import fcntl
import hashlib
import json
import os
import tempfile
from pathlib import Path
import shutil
import sys

import run
import automatic_settings
import anthropic
from accounts import aligned
from budget import Budget, BudgetStop, amount, key_material
from compact import fingerprint


def save_allocation(path, value):
    if path.is_symlink():
        raise BudgetStop('C04 allocation ledger cannot be a symlink')
    with tempfile.NamedTemporaryFile(mode='w', encoding='utf-8', dir=path.parent,
                                     prefix='.C04-', delete=False) as output:
        temporary = Path(output.name)
        try:
            output.write(json.dumps(value)+'\n')
            output.flush()
            os.fsync(output.fileno())
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)


class RowBudget(Budget):
    def __init__(self, key, ledger, allocation, observer=None):
        super().__init__(key, **({'observer': observer} if observer else {}))
        self.ledger = ledger
        self.allocation = allocation
        self.start = amount(ledger['start_usage_usd'])
        self.row_start = None

    def check(self):
        selected = key_material(self.key)
        if hashlib.sha256(selected.encode()).hexdigest() != self.ledger['key_digest']:
            raise BudgetStop('C04 key differs from the existing weekly ledger')
        current = super().check()
        total = amount(current['usage'])
        if 'start_usage_usd' not in self.allocation:
            self.allocation['start_usage_usd'] = str(total)
        if self.row_start is None:
            self.row_start = total
        added = total-amount(self.allocation['start_usage_usd'])
        row = total-self.row_start
        if added < 0 or row < 0 or added >= Decimal('5') or row >= Decimal('2.50'):
            self.stopped = True
            raise BudgetStop('C04 $2.50 row or $5 total shared-key stop reached')
        return dict(current, compaction_shared_key_usage_usd=float(added), row_shared_key_usage_usd=float(row))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--execute', action='store_true')
    parser.add_argument('--authorization-ref', help='Actual later run release, not the design approval')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--harness', action='append')
    parser.add_argument('--defer-harness', action='append', default=[], metavar='HARNESS=REASON',
                        help='keep this row planned but do not probe or execute its native binary')
    parser.add_argument('--codex-binary', default='codex')
    parser.add_argument('--claude-binary', default='claude')
    parser.add_argument('--opencode-binary', default='opencode')
    parser.add_argument('--opencode-model-witness', type=Path,
                        default=run.ROOT/'bench/native-model-limits.json')
    parser.add_argument('--grapt-binary', default='grapt')
    parser.add_argument('--product-binary', default='grapt-product')
    parser.add_argument('--account-source', type=Path)
    parser.add_argument('--grapt-compaction-ref', help='Released supported buffer-key source ref')
    parser.add_argument('--trigger-profile', default='c04-32k',
                        help='c04-32k (default) or grapt-only c04-long-128k')
    parser.add_argument('--server-side', choices=['off', 'on'], default='off',
                        help='SE provider compaction arm; off uses SE summary')
    parser.add_argument('--weekly-budget-ledger', type=Path)
    parser.add_argument('--allocation-ledger', type=Path,
                        default=Path.home()/'.cache/grapt/bench-private/C04-paid-allocation.json')
    parser.add_argument('--openrouter-key', type=Path,
                        default=Path.home()/'.config/grapt-bench/openrouter.key')
    parser.add_argument('--claude-route', choices=['anthropic-direct', 'openrouter'], default='anthropic-direct')
    parser.add_argument('--anthropic-model', default='claude-haiku-5-5')
    parser.add_argument('--anthropic-key', type=Path, default=Path.home()/'.config/grapt-bench/anthropic-api.key')
    parser.add_argument('--anthropic-monthly-ledger', type=Path)
    args = parser.parse_args()
    fixture = run.ROOT/'fixtures/benchmarks/automatic-compaction.json'
    task = json.loads(fixture.read_text())['tasks'][0]
    adapters = json.loads((run.ROOT/'bench/adapters.json').read_text())
    adapters = anthropic.adapters_for_route(adapters, args.claude_route, args.anthropic_model)
    if args.harness:
        if len(set(args.harness)) != len(args.harness) or set(args.harness)-set(adapters):
            parser.error('Unknown or duplicate harness')
        adapters = {key: row for key, row in adapters.items() if key in args.harness}
    try:
        deferred = run.deferred_harnesses(args.defer_harness, adapters)
    except ValueError as error:
        parser.error(str(error))
    active = {key: row for key, row in adapters.items() if key not in deferred}
    try:
        profile = automatic_settings.trigger_profile(args.trigger_profile, args.server_side == 'on')
        automatic_settings.validate_policy(task['automatic_compaction'], automatic_settings.trigger_profile())
        for adapter in adapters.values():
            automatic_settings.trigger_profile(args.trigger_profile, args.server_side == 'on', adapter['harness'])
    except ValueError as error:
        parser.error(str(error))
    task = dict(task, automatic_compaction=dict(task['automatic_compaction'],
                                               threshold_tokens=profile['threshold_tokens']))
    plan = {'task': 'C04', 'held_out': False, 'profile': 'controlled-native-automatic-compaction',
            'planned_tasks': ['C04'], 'planned_harnesses': list(adapters),
            'provider_calls': 0, 'attempt_seconds': 600, 'retries': 0,
            'paid_row_stop_usd': 2.5, 'paid_total_stop_usd': 5,
            'weekly_run_stop_usd': 30, 'utc_week_stop_usd': 70,
            'corpus': hashlib.sha256(fixture.read_bytes()).hexdigest(),
            'coverage_requires': 'completed automatic compaction before final-stage reveal, new source work and same-session continuation',
            'grapt_dependency': 'Supported auto/buffer/keep.tokens key, exact accepted source, run release'}
    plan.update(profile)
    plan['deferred_harnesses'] = deferred
    if not any(row['auth'] == 'openrouter' for row in adapters.values()):
        # The old per-row/allocation/week stops belong to OpenRouter only.
        for key in ['paid_row_stop_usd', 'paid_total_stop_usd', 'weekly_run_stop_usd', 'utc_week_stop_usd']:
            plan[key] = None
    plan['anthropic_calendar_month_cap_usd'] = (
        200 if any(row['auth'] == 'anthropic-api' for row in adapters.values()) else None)
    plan['adapters'] = {name: {k: v for k, v in row.items() if k != 'argv'}
                        for name, row in adapters.items()}
    plan['compaction_modes'] = {name: ('provider-server' if profile['server_side'] else
                                      'client-summary' if row['harness'] == 'grapt' else 'native-client')
                                for name, row in adapters.items()}
    if not args.execute:
        print(json.dumps(plan, indent=2))
        return 0
    if not args.authorization_ref or not args.output or not args.account_source:
        parser.error('Execution needs the later run release, fresh output, and actual account catalog')
    if any(row['auth'] == 'chatgpt-oauth' for row in active.values()):
        aligned(Path.home()/'.codex/auth.json', Path.home()/'.local/share/opencode/auth.json')
    paid = any(row['auth'] == 'openrouter' for row in active.values())
    if paid and not args.weekly_budget_ledger:
        parser.error('Paid rows require the EXISTING weekly ledger; never reset its baseline')
    if any(row['auth'] == 'anthropic-api' for row in active.values()) and not args.anthropic_monthly_ledger:
        parser.error('Direct Anthropic rows require a separate monthly ledger')
    if any(row['harness'] == 'grapt' for row in active.values()) and not args.grapt_compaction_ref:
        parser.error('SE requires the released supported auto/buffer source')
    replacements = {name: getattr(args, name) for name in
                    ['codex_binary', 'claude_binary', 'opencode_binary', 'grapt_binary',
                     'product_binary', 'account_source']}
    replacements['grapt_adapter'] = str(run.ROOT/'bench/grapt.py')
    replacements['grapt_compaction_ref'] = args.grapt_compaction_ref
    replacements['opencode_model_witness'] = args.opencode_model_witness
    replacements['compaction_profile'] = profile['trigger_profile']
    replacements['compaction_server_side'] = profile['server_side']
    replacements['anthropic_key'] = args.anthropic_key
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    records = []
    plan.update(provider_calls=None, authorization_ref=args.authorization_ref, attempts=records, binaries={},
                adapters={key: {k: v for k, v in row.items() if k != 'argv'}
                          for key, row in adapters.items()},
                runner_source={p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                               for p in [Path(__file__), run.ROOT/'bench/automatic_session.py',
                                         run.ROOT/'bench/compaction_evidence.py',
                                         run.ROOT/'bench/automatic_settings.py',
                                         run.ROOT/'bench/grapt_usage.py',
                                         run.ROOT/'bench/provider_timing.py',
                                         run.ROOT/'bench/cache_diagnostics.py', Path(run.__file__)]})
    lock = None
    try:
        allocation, budget = {}, None
        monthly = anthropic.MonthlySpend(args.anthropic_monthly_ledger) if args.anthropic_monthly_ledger else None
        if paid:
            ledger = json.loads(args.weekly_budget_ledger.read_text())
            args.allocation_ledger.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            lock = args.allocation_ledger.with_suffix('.lock').open('a')
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            if args.allocation_ledger.exists():
                allocation = json.loads(args.allocation_ledger.read_text())
                if allocation.get('weekly_run') != ledger['weekly_run']:
                    raise BudgetStop('C04 allocation belongs to a different weekly run')
            else:
                allocation = {'weekly_run': ledger['weekly_run'], 'claimed_rows': []}
            budget = RowBudget(args.openrouter_key, ledger, allocation)
            budget.check()
            save_allocation(args.allocation_ledger, allocation)
        for harness, adapter in active.items():
            native = adapter['harness']
            binary = shutil.which(str(replacements[native+'_binary']))
            if not binary:
                raise ValueError('Selected native binary is absent')
            plan['binaries'][harness] = fingerprint(Path(binary).resolve())
            adapter = dict(adapter)
            if native == 'claude':
                adapter['argv'] = [binary, *adapter['argv'][1:]]
            if adapter['auth'] == 'openrouter':
                if harness in allocation['claimed_rows']:
                    raise BudgetStop('C04 paid row already claimed; no retries')
                allocation['claimed_rows'].append(harness)
                budget.row_start = None
                budget.check()
                save_allocation(args.allocation_ledger, allocation)
            if adapter['auth'] == 'anthropic-api':
                monthly.check()
            record = run.attempt(task, harness, adapter, output, 600, replacements,
                                 budget=budget if adapter['auth'] == 'openrouter' else None)
            if adapter['auth'] == 'anthropic-api':
                record['anthropic_monthly_spend'] = monthly.record(record)
                (output/harness/task['id']/'result.json').write_text(json.dumps(record, indent=2)+'\n')
            records.append(record)
            (output/'results.json').write_text(json.dumps(plan, indent=2)+'\n')
            if record['status'] == 'budget_stopped':
                break
    finally:
        plan['measurement_complete'] = len(records) == len(adapters)
        (output/'results.json').write_text(json.dumps(plan, indent=2)+'\n')
        if lock:
            lock.close()
    return 0 if len(records) == len(adapters) and all(row['success'] for row in records) else 1


if __name__ == '__main__':
    raise SystemExit(main())
