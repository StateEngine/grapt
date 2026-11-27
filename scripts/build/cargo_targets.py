"""Read-only Cargo output attribution for processes outside SE Code checkouts."""
import os
from pathlib import Path
import stat
import subprocess
import tomllib


MAX_CONFIG_BYTES = 1024 * 1024
MAX_ANCESTORS = 128


def non_path_overrides(arguments):
    """Strip only inline overrides proven not to redirect outputs or run wrappers."""
    clean = []
    index = 0
    while index < len(arguments):
        argument = arguments[index]
        if argument == '--':
            clean.extend(arguments[index:])
            break
        if argument == '--config' or argument.startswith('--config='):
            index += 1
            value = (argument.partition('=')[2] if argument != '--config'
                     else arguments[index] if index < len(arguments) else '')
            if argument == '--config':
                index += 1
            if not value or len(value) > 4096:
                raise ValueError('Cargo configuration override exceeds its bound')
            data = tomllib.loads(value)
            build = data.get('build', {})
            profile = data.get('profile', {})
            disabled_wrapper = (set(data) == {'build'} and isinstance(build, dict)
                                and len(build) == 1
                                and next(iter(build)) in ('rustc-wrapper', 'rustc-workspace-wrapper')
                                and next(iter(build.values())) == '')
            panic = (set(data) == {'profile'} and isinstance(profile, dict) and len(profile) == 1
                     and all(isinstance(row, dict) and set(row) == {'panic'}
                             and row['panic'] in ('unwind', 'abort') for row in profile.values()))
            if not (disabled_wrapper or panic):
                raise ValueError('Cargo configuration override cannot be attributed')
            continue
        clean.append(argument)
        index += 1
    return clean


def builtin(arguments):
    # Config/env aliases and external cargo-* commands may add their own output
    # flags. Only full builtin names are safe; even the usual short aliases can
    # be overridden. Unknown global syntax is not guessed past.
    index = 1
    while index < len(arguments):
        value = arguments[index]
        if value.startswith('+') or value in ('--quiet', '--verbose', '--locked', '--frozen', '--offline', '-q'):
            index += 1
        elif value.startswith('-') and len(value) > 1 and set(value[1:]) == {'v'}:
            index += 1
        elif value == '--color' and index + 1 < len(arguments):
            index += 2
        elif value.startswith('--color='):
            index += 1
        else:
            if value not in ('build', 'check', 'test', 'bench', 'doc', 'run', 'rustc', 'rustdoc'):
                raise ValueError('Cargo alias or custom command cannot be attributed')
            if value in ('rustc', 'rustdoc') and '--' in arguments[index + 1:]:
                raise ValueError('Forwarded compiler options cannot be attributed')
            return
    raise ValueError('Cargo command cannot be attributed')


def incremental(arguments, cwd):
    paths = set()
    for index, argument in enumerate(arguments):
        if argument in ('-C', '--codegen'):
            if index + 1 == len(arguments):
                raise ValueError('Compiler codegen option has no value')
            value = arguments[index + 1]
        elif argument.startswith('-C'):
            value = argument[2:].removeprefix('=')
        elif argument.startswith('--codegen='):
            value = argument[len('--codegen='):]
        else:
            continue
        key, separator, path = value.partition('=')
        if key.strip() == 'incremental':
            if not separator:
                raise ValueError('Compiler incremental output cannot be attributed')
            paths.add(absolute(path, cwd))
    return paths


def option(arguments, name):
    values = []
    for index, argument in enumerate(arguments):
        if argument == '--':
            break
        if argument == name:
            if index + 1 == len(arguments):
                raise ValueError('Cargo path option has no value')
            values.append(arguments[index + 1])
        elif argument.startswith(name + '='):
            values.append(argument[len(name) + 1:])
    if len(values) > 1:
        raise ValueError('Ambiguous repeated Cargo path option')
    return values[0] if values else None


def absolute(value, base):
    if not isinstance(value, str) or not value or len(value) > 4096 or '\0' in value:
        raise ValueError('Invalid Cargo output path')
    path = Path(value)
    return (path if path.is_absolute() else base / path).resolve()


def document(path):
    # Absent is different from unreadable; FIFOs and oversized inputs refuse.
    descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as source:
        before = os.fstat(source.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_size > MAX_CONFIG_BYTES:
            raise ValueError('Cargo config is not a bounded regular file')
        raw = source.read(MAX_CONFIG_BYTES + 1)
        after = os.fstat(source.fileno())
        if (len(raw) > MAX_CONFIG_BYTES or
                (before.st_ino, before.st_size, before.st_mtime_ns) !=
                (after.st_ino, after.st_size, after.st_mtime_ns)):
            raise ValueError('Cargo config changed during attribution')
    result = tomllib.loads(raw.decode())
    if 'include' in result:
        raise ValueError('Included Cargo config cannot be attributed')
    return result


def config(directory):
    # Cargo prefers the extensionless spelling when both exist.
    for name in ('config', 'config.toml'):
        path = directory / name
        try:
            return document(path), path.parent.parent
        except FileNotFoundError:
            # A dangling symlink is unreadable, not an absent configuration.
            if path.is_symlink():
                raise ValueError('Dangling Cargo config') from None
    return {}, directory.parent


def workspace(cwd, arguments, deadline):
    import time
    cargo = str(Path.home() / '.cargo/bin/cargo')
    command = [cargo, '+1.97.1', 'locate-project', '--workspace', '--message-format', 'plain']
    manifest = option(arguments, '--manifest-path')
    if manifest is not None:
        command.extend(['--manifest-path', str(absolute(manifest, cwd))])
    timeout = min(5, deadline - time.monotonic())
    if timeout <= 0:
        raise ValueError('Cargo attribution deadline expired')
    # locate-project reads manifests only: no metadata resolution, build, lockfile
    # generation, source execution or forwarding of foreign Cargo options/env.
    result = subprocess.run(command, cwd=cwd, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, timeout=timeout, check=True)
    if len(result.stdout) > 8192:
        raise ValueError('Cargo workspace path exceeds its bound')
    manifest = Path(os.fsdecode(result.stdout).strip())
    if not manifest.is_absolute() or manifest.name != 'Cargo.toml':
        raise ValueError('Cargo did not identify a workspace manifest')
    return manifest.parent.resolve()


def resolve(cwd, environment, arguments, deadline, kind='cargo'):
    """Match Cargo path precedence; unsupported configuration keeps refusal."""
    env = {os.fsdecode(k): os.fsdecode(v) for k, v in environment.items()}
    args = [os.fsdecode(value) for value in arguments]
    if kind in ('rustc', 'rustdoc'):
        if any(argument.startswith('@') for argument in args):
            raise ValueError('Compiler response-file output cannot be attributed')
        # Compiler -C is a codegen option, not Cargo's cwd override. Its actual
        # output flags are stronger evidence than re-deriving Cargo settings.
        outputs = {absolute(value, cwd) for flag in ('--out-dir', '--output', '-o')
                   if (value := option(args, flag)) is not None}
        emit = option(args, '--emit')
        if (not outputs or (emit is not None and '=' in emit) or
                any(a.startswith('-o') and not a.startswith('--') and a != '-o' for a in args)):
            raise ValueError('Compiler output cannot be attributed')
        outputs.update(incremental(args, cwd))
        for key in ('CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET_DIR', 'CARGO_BUILD_BUILD_DIR'):
            if key in env:
                if '{' in env[key] or '}' in env[key]:
                    raise ValueError('Compiler output template cannot be attributed')
                outputs.add(absolute(env[key], cwd))
        return outputs
    args = non_path_overrides(args)
    builtin(args)
    options = args[:args.index('--')] if '--' in args else args
    if any(a in ('--config', '-C') or a.startswith(('--config=', '-C')) for a in options):
        raise ValueError('Cargo command configuration cannot be attributed')
    home = absolute(env.get('HOME', str(Path.home())), cwd)
    cargo_home = absolute(env.get('CARGO_HOME', str(home / '.cargo')), cwd)
    parents = list(cwd.parents)
    if len(parents) >= MAX_ANCESTORS:
        raise ValueError('Cargo config ancestor bound exceeded')
    settings = {}
    # Home is lowest precedence; nearer cwd configs replace scalar paths.
    for directory in [cargo_home, *[p / '.cargo' for p in reversed([cwd, *parents])]]:
        data, base = config(directory)
        build = data.get('build', {})
        if not isinstance(build, dict):
            raise ValueError('Invalid Cargo build configuration')
        for key in ('target-dir', 'build-dir'):
            if key in build:
                settings[key] = (build[key], base)
    root = None

    def expand(value, base):
        nonlocal root
        if not isinstance(value, str):
            raise ValueError('Cargo output path is not a string')
        if '{workspace-root}' in value:
            root = root or workspace(cwd, args, deadline)
            value = value.replace('{workspace-root}', str(root))
        value = value.replace('{cargo-cache-home}', str(cargo_home))
        if '{' in value or '}' in value:
            raise ValueError('Cargo output template cannot be attributed')
        return absolute(value, base)

    selected = option(args, '--target-dir')
    if selected is None:
        selected = env.get('CARGO_TARGET_DIR', env.get('CARGO_BUILD_TARGET_DIR'))
    if selected is not None:
        target = absolute(selected, cwd)
    elif 'target-dir' in settings:
        target = absolute(*settings['target-dir'])
    else:
        root = workspace(cwd, args, deadline)
        target = root / 'target'
    outputs = {target}
    if 'CARGO_BUILD_BUILD_DIR' in env:
        outputs.add(expand(env['CARGO_BUILD_BUILD_DIR'], cwd))
    elif 'build-dir' in settings:
        outputs.add(expand(*settings['build-dir']))
    # Direct compiler invocations can name output independently of Cargo config.
    for flag in ('--out-dir', '--output', '-o'):
        value = option(args, flag)
        if value is not None:
            outputs.add(absolute(value, cwd))
    return outputs


def managed_roots(owners):
    managed = absolute(os.environ.get('SECODE_CACHE_DIR', str(Path.home() / '.cache/secode')),
                       Path.cwd()) / 'target'
    return {managed.resolve(), *(path.resolve() for path in owners)}


def foreign(cwd, environment, arguments, owners, deadline, kind='cargo', excused=None):
    """Additional veto only: True never replaces the whole-tree observation.

    A resolved output root the caller has already attributed to this build is
    the one target it is allowed to write into, so it is not an unknown user of
    that target. Every other overlap still refuses.
    """
    outputs = resolve(cwd, environment, arguments, deadline, kind)
    roots = managed_roots(owners)
    if excused is not None:
        allowed = Path(os.path.realpath(excused, strict=True))
        # An output inside the root this build declared is that root being used,
        # including when a managed root is its ancestor. An output above the
        # declared root is still an unknown user of the tree around it.
        outputs = {path for path in outputs
                   if not (path == allowed or allowed in path.parents)}
    # An output alias into any managed/local target remains an unknown user of
    # that target. Refuse rather than classifying it as a harmless foreign build.
    return all(not (path == root or root in path.parents or path in root.parents)
               for path in outputs for root in roots)
