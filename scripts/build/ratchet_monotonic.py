#!/usr/bin/env python3
"""Enforce monotonic shrinking of the three structural ratchet ledgers against a base commit.

Local Rust tests read only the working tree (zero git-history dependency).
CI and the gate script invoke this checker with the trusted base commit so a
pull request cannot add new debt and edit the ledger in the same change.

usage: ratchet_monotonic.py <base-commit>
"""
from collections import Counter
import json
from pathlib import Path
import subprocess
import sys

import module_cycles

ROOT = Path(__file__).resolve().parents[2]
INCLUDE_RATCHET = 'crates/grapt-types/tests/include-ratchet.json'
LEGACY_RATCHET = 'crates/grapt-types/tests/legacy-expect-ratchet.json'
CYCLES_RATCHET = 'scripts/build/module-cycles.json'


def verify_base_commit(root, base):
    """Fail closed if `base` is not a valid commit in `root`."""
    if not base or not base.strip():
        return 'base commit must not be empty'
    probe = subprocess.run(['git', 'rev-parse', '--verify', '--quiet', f'{base}^{{commit}}'],
                           cwd=root, capture_output=True, text=True, timeout=30)
    if probe.returncode != 0:
        return f'unknown or invalid base commit {base!r}'
    return None


def git_show_json(root, base, relpath):
    """Return (doc, error) at base:relpath.

    Returns (None, None) ONLY when `git ls-tree` succeeds and proves the path
    did not exist in `base`. Any git/IO error or invalid JSON fails closed.
    """
    probe = subprocess.run(['git', 'ls-tree', '--name-only', base, '--', relpath],
                           cwd=root, capture_output=True, text=True, timeout=30)
    if probe.returncode != 0:
        detail = probe.stderr.strip() or f'exit {probe.returncode}'
        return None, f'{relpath}: failed to inspect base commit {base!r}: {detail}'
    if not probe.stdout.strip():
        return None, None
    read = subprocess.run(['git', 'show', f'{base}:{relpath}'],
                          cwd=root, capture_output=True, text=True, timeout=30)
    if read.returncode != 0:
        detail = read.stderr.strip() or f'exit {read.returncode}'
        return None, f'{relpath}: failed to read blob at {base!r}: {detail}'
    try:
        return json.loads(read.stdout), None
    except json.JSONDecodeError as error:
        return None, f'{relpath}: invalid JSON at {base!r}: {error}'


def live_workspace_configs(root):
    """Return (configs_set, error) of live target+feature configs from `cargo metadata`."""
    probe = subprocess.run(
        ['cargo', 'metadata', '--no-deps', '--format-version', '1'],
        cwd=root, capture_output=True, text=True, timeout=60,
    )
    if probe.returncode != 0:
        detail = probe.stderr.strip() or f'exit {probe.returncode}'
        return None, f'failed to read workspace configs from cargo metadata: {detail}'
    try:
        metadata = json.loads(probe.stdout)
    except json.JSONDecodeError as error:
        return None, f'invalid cargo metadata JSON: {error}'
    configs = set()
    for package in metadata.get('packages', []):
        name = package['name']
        kinds = {kind for target in package.get('targets', []) for kind in target.get('kind', [])}
        base_names = []
        if 'lib' in kinds:
            base_names.append(name)
        bins = sorted({target['name'] for target in package.get('targets', []) if 'bin' in target.get('kind', [])})
        for binary in bins:
            base_names.append(f'{name}:{binary}')
        for base_name in base_names:
            for label, _ in module_cycles.feature_configs(package):
                configs.add(base_name if label is None else f'{base_name}[{label}]')
    return configs, None


def normalize_include_carriers(document):
    carriers = document.get('carriers', {})
    out = {}
    for carrier, entry in carriers.items():
        fragments = entry.get('fragments', []) if isinstance(entry, dict) else entry
        out[carrier] = Counter(fragments)
    return out


def check_include_ratchet(base_doc, head_doc):
    if base_doc is None:
        return []
    base = normalize_include_carriers(base_doc)
    head = normalize_include_carriers(head_doc)
    problems = []
    for carrier, fragments in sorted(head.items()):
        if carrier not in base:
            problems.append(f'{INCLUDE_RATCHET}: new carrier {carrier} added vs base')
            continue
        base_counts = base[carrier]
        for fragment, count in sorted(fragments.items()):
            base_count = base_counts.get(fragment, 0)
            if count > base_count:
                problems.append(
                    f'{INCLUDE_RATCHET}: new fragment {fragment} added under {carrier} '
                    f'({count} > {base_count}) vs base')
    return problems


def check_legacy_ratchet(base_doc, head_doc):
    if base_doc is None:
        return []
    base = base_doc.get('files', {})
    head = head_doc.get('files', {})
    problems = []
    for path, lints in sorted(head.items()):
        if path not in base:
            problems.append(f'{LEGACY_RATCHET}: new file {path} added vs base')
            continue
        base_lints = base[path]
        if not isinstance(base_lints, dict) or not isinstance(lints, dict):
            problems.append(f'{LEGACY_RATCHET}: {path} must map lint names to counts')
            continue
        for lint, count in sorted(lints.items()):
            if lint not in base_lints:
                problems.append(f'{LEGACY_RATCHET}: new lint {lint} added in {path} vs base')
            elif count > base_lints[lint]:
                problems.append(
                    f'{LEGACY_RATCHET}: {path} lint {lint} grew from {base_lints[lint]} to {count} vs base')
    return problems


def check_cycles_ratchet(base_doc, head_doc, live_configs=None):
    if base_doc is None:
        return []
    problems = []
    # Lock deep/collapse prefixes: removing or altering a deep prefix can collapse
    # sub-modules and disguise cyclic edges as apparent shrinkage.
    base_deep = sorted(base_doc.get('deep', []))
    head_deep = sorted(head_doc.get('deep', []))
    if head_deep != base_deep:
        problems.append(
            f'{CYCLES_RATCHET}: deep prefixes changed from {base_deep} to {head_deep}; '
            'deep prefixes are policy-locked'
        )
    # Lock checked configuration set against actual live workspace target+feature
    # configs (never trusting the candidate ledger's own config list): dropping
    # any still-live config hides its cyclic edges, while removing a deleted
    # Cargo feature's or deleted target's config is allowed.
    base_configs = base_doc.get('configs')
    head_configs = head_doc.get('configs')
    if isinstance(base_configs, list):
        if not isinstance(head_configs, list):
            problems.append(f'{CYCLES_RATCHET}: configs list was removed vs base')
        else:
            head_config_set = set(head_configs)
            for cfg in sorted(set(base_configs) - head_config_set):
                if live_configs is None or cfg in live_configs:
                    problems.append(
                        f'{CYCLES_RATCHET}: checked config {cfg} was dropped while still live'
                    )
    if not isinstance(base_doc.get('edges'), dict):
        return problems
    base_config_set = set(base_configs) if isinstance(base_configs, list) else None
    base_cycles = base_doc.get('cycles', {})
    head_cycles = head_doc.get('cycles', {})
    base_edges = base_doc['edges']
    head_edges = head_doc.get('edges', {})
    for target, groups in sorted(head_cycles.items()):
        lookup = module_cycles.baseline_key_for(target, base_config_set, live_configs)
        prior = base_cycles.get(lookup, [])
        for group in groups:
            if not any(set(group) <= set(old) for old in prior):
                problems.append(f'{CYCLES_RATCHET}: {target} new or larger cycle {group} vs base')
    for target, edges in sorted(head_edges.items()):
        lookup = module_cycles.baseline_key_for(target, base_config_set, live_configs)
        prior_edges = {tuple(edge) for edge in base_edges.get(lookup, [])}
        for edge in edges:
            pair = tuple(edge)
            if pair not in prior_edges:
                problems.append(
                    f'{CYCLES_RATCHET}: {target} new cyclic edge {pair[0]} -> {pair[1]} vs base')
    return problems


def check(root, base, live_configs=None):
    base_error = verify_base_commit(root, base)
    if base_error is not None:
        return [base_error]
    if live_configs is None:
        live_configs, meta_error = live_workspace_configs(root)
        if meta_error is not None:
            return [meta_error]
    problems = []
    for relpath, checker in (
        (INCLUDE_RATCHET, check_include_ratchet),
        (LEGACY_RATCHET, check_legacy_ratchet),
        (CYCLES_RATCHET, lambda b, h: check_cycles_ratchet(b, h, live_configs=live_configs)),
    ):
        head_path = Path(root) / relpath
        if not head_path.is_file():
            problems.append(f'{relpath}: missing ratchet ledger')
            continue
        head_doc = json.loads(head_path.read_text(encoding='utf-8'))
        base_doc, read_error = git_show_json(root, base, relpath)
        if read_error is not None:
            problems.append(read_error)
            continue
        problems.extend(checker(base_doc, head_doc))
    return problems


def main():
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    problems = check(ROOT, sys.argv[1])
    for problem in problems:
        print(problem)
    return 1 if problems else 0


if __name__ == '__main__':
    sys.exit(main())
