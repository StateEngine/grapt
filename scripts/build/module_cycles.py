#!/usr/bin/env python3
"""Module dependency cycles and cyclic edges may only shrink.

Each crate's graph comes from cargo-modules (both default features and
--all-features when the package declares features). Items are collapsed to the
module that holds them, at depth one below the crate root (deeper for prefixes
listed in the baseline's "deep"), and cycles are the strongly connected groups
of those modules plus the directed cyclic edges inside each group. Edges to or
from the crate root are ignored: re-exports there are not dependencies. A new
cycle, a module joining an existing one, or a new cyclic edge within an
existing SCC fails; shrinkage must be recorded so the ledger stays exact.

usage: module_cycles.py check | record
"""
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path(__file__).with_name('module-cycles.json')
VERSION = 'cargo-modules 0.27.0'
NODE = re.compile(r'^\s*"([^"]+)" \[label="([^|"]*)\|')
USES = re.compile(r'^\s*"([^"]+)" -> "([^"]+)" \[label="uses"')
FEATURE_CONFIG_SUFFIX = re.compile(r'^(?P<base>.+)\[(?:all-features|feature:[^\[\]\s]+)\]$')
MODULE_KINDS = {'crate', 'mod'}


def baseline_key_for(target, base_configs):
    """Return the baseline lookup key for `target` against `base_configs`.

    - Existing configs in `base_configs` always compare strictly against
      themselves (never falling back to a parent target).
    - Newly added feature-variant configs (`<base>[all-features]` or
      `<base>[feature:<name>]`) whose `<base>` target already exists in
      `base_configs` inherit `<base>`'s baseline so adding a Cargo feature
      passes iff it introduces zero new cycles or cyclic edges beyond `<base>`.
    - Brand-new targets not in `base_configs` compare strictly against empty.
    """
    if base_configs is not None and target not in base_configs:
        match = FEATURE_CONFIG_SUFFIX.match(target)
        if match is not None:
            base_target = match.group('base')
            if base_target in base_configs:
                return base_target
    return target


def tool():
    path = os.environ.get('SECODE_CARGO_MODULES', 'cargo-modules')
    version = subprocess.run([path, '--version'], capture_output=True, text=True, timeout=30)
    if version.returncode or version.stdout.strip() != VERSION:
        raise SystemExit(f'install {VERSION} (cargo install cargo-modules --version 0.27.0 --locked) '
                         'or set SECODE_CARGO_MODULES')
    return path


def parse(text):
    modules, root, edges = set(), None, []
    for line in text.splitlines():
        if (match := USES.match(line)):
            edges.append(match.groups())
        elif (match := NODE.match(line)):
            if match.group(2).split()[-1] in MODULE_KINDS:
                modules.add(match.group(1))
                root = root or match.group(1)
    if root is None:
        raise ValueError('graph has no crate node')
    return root, modules, edges


def module_key(path, modules, deep):
    depth = 2
    for prefix in deep:
        if path == prefix or path.startswith(prefix + '::'):
            depth = max(depth, prefix.count('::') + 2)
    parts = path.split('::')[:depth]
    # An item defined in the crate root keeps its own key: it can carry a real
    # dependency between siblings, unlike the root's re-exports.
    while len(parts) > 2 and '::'.join(parts) not in modules:
        parts.pop()
    return '::'.join(parts)


def module_graph(text, deep=()):
    """Build the collapsed module graph from a single cargo-modules DOT graph."""
    graph = {}
    root, modules, edges = parse(text)
    for source, target in edges:
        if root in (source, target):
            continue  # the root's own `use` re-exports are not dependencies
        a, b = module_key(source, modules, deep), module_key(target, modules, deep)
        if a != b:
            graph.setdefault(a, set()).add(b)
            graph.setdefault(b, set())
    return graph


def cycles_and_edges(text, deep=()):
    graph = module_graph(text, deep)
    groups = strongly_connected(graph)
    cyclic_edges = []
    for group in groups:
        members = set(group)
        for source in group:
            for target in sorted(graph.get(source, ())):
                if target in members and target != source:
                    cyclic_edges.append([source, target])
    return groups, sorted(cyclic_edges)


def cycles(text, deep=()):
    return cycles_and_edges(text, deep)[0]


def strongly_connected(graph):
    """Iterative Tarjan; groups of two or more, each sorted, in a stable order."""
    index, low, on, stack, found = {}, {}, set(), [], []
    for start in sorted(graph):
        if start in index:
            continue
        work = [(start, iter(sorted(graph[start])))]
        index[start] = low[start] = len(index)
        stack.append(start)
        on.add(start)
        while work:
            node, children = work[-1]
            child = next(children, None)
            if child is None:
                work.pop()
                if work:
                    low[work[-1][0]] = min(low[work[-1][0]], low[node])
                if low[node] == index[node]:
                    group = []
                    while True:
                        member = stack.pop()
                        on.discard(member)
                        group.append(member)
                        if member == node:
                            break
                    if len(group) > 1:
                        found.append(sorted(group))
            elif child not in index:
                index[child] = low[child] = len(index)
                stack.append(child)
                on.add(child)
                work.append((child, iter(sorted(graph[child]))))
            elif child in on:
                low[node] = min(low[node], index[child])
    return sorted(found)


def differences(target, now, recorded, now_edges=None, recorded_edges=None):
    problems = []
    for group in now:
        if not any(set(group) <= set(old) for old in recorded):
            problems.append(f'{target}: new or larger cycle {group}; break it instead of recording it')
    for old in recorded:
        if not any(set(old) <= set(group) for group in now):
            problems.append(f'{target}: cycle {old} shrank or split; run module_cycles.py record')
    if now_edges is not None and recorded_edges is not None:
        now_set = {tuple(edge) for edge in now_edges}
        old_set = {tuple(edge) for edge in recorded_edges}
        for source, dest in sorted(now_set - old_set):
            problems.append(
                f'{target}: new cyclic edge {source} -> {dest}; break it instead of recording it')
        for source, dest in sorted(old_set - now_set):
            problems.append(
                f'{target}: cyclic edge {source} -> {dest} was removed; run module_cycles.py record')
    return problems


def feature_configs(package):
    """Independent feature configurations for a package: default, --all-features,
    and each individual opt-in feature when multiple opt-in features exist.
    """
    configs = [(None, [])]
    features = package.get('features') or {}
    non_default = sorted(name for name in features if name != 'default')
    if non_default:
        configs.append(('all-features', ['--all-features']))
        if len(non_default) > 1:
            for feat in non_default:
                configs.append((f'feature:{feat}', ['--features', feat]))
    elif features:
        configs.append(('all-features', ['--all-features']))
    return configs


def targets(baseline):
    _ = baseline
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--no-deps', '--format-version', '1', '--locked'], cwd=ROOT, text=True))
    for package in sorted(metadata['packages'], key=lambda p: p['name']):
        kinds = {kind for target in package['targets'] for kind in target['kind']}
        configs = feature_configs(package)
        selectors = []
        if 'lib' in kinds:
            selectors.append((package['name'], ['--lib']))
        bins = sorted({target['name'] for target in package['targets'] if 'bin' in target['kind']})
        for binary in bins:
            selectors.append((f"{package['name']}:{binary}", ['--bin', binary]))
        for base_name, selector in selectors:
            for label, feature_args in configs:
                config_name = base_name if label is None else f'{base_name}[{label}]'
                yield config_name, package['name'], selector, feature_args


def measure(path, baseline):
    found_cycles, found_edges, checked_configs = {}, {}, []
    for name, package, selector, feature_args in targets(baseline):
        checked_configs.append(name)
        dot = subprocess.run(
            [path, 'dependencies', '-p', package, *selector, *feature_args,
             '--no-externs', '--no-sysroot'],
            cwd=ROOT, capture_output=True, text=True, timeout=1800)
        if dot.returncode:
            raise SystemExit(f'{name} ({feature_args}): cargo-modules failed: {dot.stderr.strip()[-2000:]}')
        groups, edges = cycles_and_edges(dot.stdout, baseline.get('deep', []))
        if groups:
            found_cycles[name] = groups
            found_edges[name] = edges
    return found_cycles, found_edges, sorted(checked_configs)


def main():
    mode = sys.argv[1] if len(sys.argv) == 2 else ''
    if mode not in ('check', 'record'):
        raise SystemExit(__doc__)
    baseline = json.loads(BASELINE.read_text(encoding='utf-8'))
    current_cycles, current_edges, checked_configs = measure(tool(), baseline)
    recorded_cycles = baseline.get('cycles', {})
    recorded_edges = baseline.get('edges', {})
    recorded_configs = baseline.get('configs')
    recorded_config_set = set(recorded_configs) if isinstance(recorded_configs, list) else None
    all_targets = sorted(set(current_cycles) | set(recorded_cycles) | set(current_edges) | set(recorded_edges))
    problems = [
        problem for name in all_targets
        for problem in differences(
            name,
            current_cycles.get(name, []),
            recorded_cycles.get(baseline_key_for(name, recorded_config_set), []),
            current_edges.get(name, []),
            recorded_edges.get(baseline_key_for(name, recorded_config_set), []),
        )
    ]
    if recorded_configs is not None and sorted(recorded_configs) != checked_configs:
        problems.append('module-cycles.json: checked config set drifted; run module_cycles.py record')
    growth = [problem for problem in problems if 'new or larger' in problem or 'new cyclic edge' in problem]
    for problem in growth if mode == 'record' else problems:
        print(problem)
    if mode == 'record' and not growth:
        # Recording only ever shrinks the list; growth is refused untouched.
        baseline['configs'] = checked_configs
        baseline['cycles'] = current_cycles
        baseline['edges'] = current_edges
        BASELINE.write_text(json.dumps(baseline, indent=1) + '\n', encoding='utf-8')
        return 0
    return 1 if (growth if mode == 'record' else problems) else 0


if __name__ == '__main__':
    sys.exit(main())
