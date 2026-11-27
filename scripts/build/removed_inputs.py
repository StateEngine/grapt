"""Refuse deleting repository inputs still referenced by retained source."""
import ast
import fnmatch
from pathlib import Path
import re
import subprocess
import tomllib


CODE = {'.rs', '.py', '.ps1', '.sh', '.js', '.ts', '.mjs', '.cjs', '.toml', '.yml', '.yaml'}
INPUTS = {'.json', '.jsonl', '.toml'}
MAX_FILES = 20000
MAX_BYTES = 4 * 1024 * 1024
MAX_DATA_BYTES = 16 * 1024 * 1024
PATH = re.compile(r'(?:bench|crates|docs|fixtures|mutations|remote-compute|scripts)/[A-Za-z0-9_./-]+')
QUOTED = re.compile(r'''["']([^"'\n]+)["']''')


def git_paths(root, *arguments):
    result = subprocess.check_output(['git', *arguments], cwd=root, timeout=30)
    if len(result) > MAX_BYTES:
        raise ValueError('Repository path inventory exceeds its bound')
    return {name.decode() for name in result.split(b'\0') if name}


def deleted_paths(root, base=None):
    committed = (git_paths(root, 'diff', '--no-renames', '--diff-filter=D', '--name-only', '-z', base, 'HEAD')
                 if base else git_paths(root, 'diff-tree', '--root', '--no-commit-id', '--diff-filter=D',
                                        '--name-only', '-r', '-z', 'HEAD'))
    return committed | git_paths(root, 'diff', '--no-renames', '--diff-filter=D', '--name-only', '-z', 'HEAD')


def value(node, names, root):
    """Evaluate only literal path construction, never application code."""
    if isinstance(node, ast.Constant) and isinstance(node.value, (str, int)):
        return node.value
    if isinstance(node, ast.Name):
        return names.get(node.id)
    if isinstance(node, ast.BinOp):
        left, right = value(node.left, names, root), value(node.right, names, root)
        if isinstance(node.op, ast.Div) and isinstance(left, Path) and isinstance(right, str):
            return left / right
        if isinstance(node.op, ast.Add) and isinstance(left, str) and isinstance(right, str):
            return left + right
    if isinstance(node, ast.Attribute):
        path = value(node.value, names, root)
        if isinstance(path, Path) and node.attr == 'parent':
            return path.parent
        if isinstance(path, Path) and node.attr == 'parents':
            return tuple(path.parents)
    if isinstance(node, ast.Subscript):
        paths, index = value(node.value, names, root), value(node.slice, names, root)
        if isinstance(paths, tuple) and type(index) is int and 0 <= index < len(paths):
            return paths[index]
    if isinstance(node, ast.Call):
        arguments = [value(argument, names, root) for argument in node.args]
        if isinstance(node.func, ast.Name) and node.func.id == 'Path' and len(arguments) == 1:
            if isinstance(arguments[0], (str, Path)):
                path = Path(arguments[0])
                return path if path.is_absolute() else root / path
        if isinstance(node.func, ast.Attribute):
            path = value(node.func.value, names, root)
            if isinstance(path, Path):
                if node.func.attr in ('resolve', 'absolute') and not arguments:
                    return path.resolve()
                if node.func.attr == 'joinpath' and all(isinstance(arg, str) for arg in arguments):
                    return path.joinpath(*arguments)
                if node.func.attr == 'with_name' and len(arguments) == 1 and isinstance(arguments[0], str):
                    try:
                        return path.with_name(arguments[0])
                    except ValueError:
                        return None  # Invalid-path counterexamples are not concrete inputs.
    return None


def surviving_match(directory, kind, pattern):
    if not directory.is_dir():
        return False
    found = directory.iterdir() if kind == 'iterdir' else getattr(directory, kind)(pattern)
    return next(iter(found), None) is not None


def references(path, root, candidates):
    bound = MAX_DATA_BYTES if path.suffix in {'.json', '.jsonl'} else MAX_BYTES
    if path.stat().st_size > bound:
        raise ValueError(f'Repository input exceeds scan bound: {path.relative_to(root)}')
    text = path.read_text(errors='replace')
    joined = re.sub(r'''["']\s*(?:/|\+)\s*["']''', '/', text).replace('\\', '/')
    joined = re.sub(r'''["']\s*\)\s*\.join\(\s*["']''', '/', joined)
    matches = list(PATH.finditer(joined))
    found = {root / match.group().rstrip('.') for match in matches}
    for match in matches:
        name = match.group().rstrip('.')
        line = joined[joined.rfind('\n', 0, match.start()) + 1:match.start()].lstrip()
        if name.startswith('fixtures/') and not Path(name).suffix and not line.startswith(('#', '//')):
            found.update(root / candidate for candidate in candidates
                         if (root / candidate).is_relative_to(root / name))
    filenames = set()
    for match in QUOTED.finditer(text):
        token = match.group(1).replace('\\', '/')
        if len(token) < 4096 and Path(token).suffix and not any(char.isspace() for char in token):
            found.add(path.parent / token)
            filenames.add(token)
    # Rust helpers also pass a literal filename to a function that joins it
    # beneath a literal repository directory. Preserve that concrete pair.
    for directory in list(found):
        if not directory.suffix:
            found.update(directory / filename for filename in filenames
                         if (directory / filename).resolve().is_relative_to(root)
                         and (directory / filename).resolve().relative_to(root).as_posix() in candidates)
    if path == root / 'Cargo.toml':
        workspace = tomllib.loads(text).get('workspace', {})
        for directory in workspace.get('exclude', []):
            found.update(root / candidate for candidate in candidates
                         if (root / candidate).is_relative_to(root / directory))
    if path.suffix == '.py':
        tree = ast.parse(text, filename=str(path))
        names = {'__file__': str(path), 'ROOT': root, 'REPO': root, 'REPO_ROOT': root}
        for node in tree.body:
            if isinstance(node, ast.Assign):
                result = value(node.value, names, root)
                for target in node.targets:
                    if isinstance(target, ast.Name):
                        names[target.id] = result
        # Helpers often accept literal filenames beneath a module-level input
        # directory. Preserve those concrete candidates without executing them.
        for directory in {item for item in names.values() if isinstance(item, Path)}:
            found.update(directory / filename for filename in filenames)
        for node in ast.walk(tree):
            result = value(node, names, root)
            if isinstance(result, Path):
                found.add(result)
            if isinstance(node, (ast.Import, ast.ImportFrom)):
                modules = ([node.module] if isinstance(node, ast.ImportFrom) else [name.name for name in node.names])
                for module in modules:
                    if module:
                        found.add(path.parent / (module.replace('.', '/') + '.py'))
            if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute):
                directory = value(node.func.value, names, root)
                if isinstance(directory, Path) and node.func.attr in ('glob', 'rglob', 'iterdir'):
                    pattern = value(node.args[0], names, root) if node.args else '*'
                    # A discovery pattern that still finds surviving files is not
                    # relying on any one deleted path; only an emptied one is.
                    if isinstance(pattern, str) and not surviving_match(directory, node.func.attr, pattern):
                        for name in candidates:
                            candidate = root / name
                            if candidate.is_relative_to(directory):
                                relative = candidate.relative_to(directory).as_posix()
                                if fnmatch.fnmatch(relative, pattern):
                                    found.add(candidate)
    return {item.resolve().relative_to(root).as_posix() for item in found
            if item.resolve().is_relative_to(root)}


def consumers(root, removed):
    root = root.resolve()
    tracked = git_paths(root, 'ls-files', '--cached', '--others', '--exclude-standard', '-z')
    if len(tracked) > MAX_FILES:
        raise ValueError('Repository file inventory exceeds its bound')
    pending = [name for name in tracked - removed if Path(name).suffix in CODE and (root / name).is_file()]
    seen, result = set(), {}
    while pending:
        name = pending.pop()
        if name in seen:
            continue
        seen.add(name)
        if len(seen) > MAX_FILES:
            raise ValueError('Repository input graph exceeds its bound')
        for target in references(root / name, root, removed):
            if target in removed:
                result.setdefault(target, set()).add(name)
            elif Path(target).suffix in INPUTS and (root / target).is_file():
                pending.append(target)
    return result
