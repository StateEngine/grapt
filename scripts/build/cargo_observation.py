"""Fail-closed observations of a foreign build process and every descendant."""
import errno
import os
from pathlib import Path
import re
import stat
import time


MAX_BYTES = 1024 * 1024
# A token and a piece bound for the resolving check. Both only ever add a veto,
# so a value that trips one is refused rather than skipped.
MAX_TOKENS = 8192
MAX_PIECES = 1 << 14
# What joins a flag or a key to the path it carries.
PIECE_SEPARATOR = re.compile(rb'[=:,\s]+|(?<=^)|(?=-L)|(?=-l)|(?=--)')
MAX_PROCESSES = 1024
MAX_ENTRIES = 4096
MAX_RESPONSES = 64
# A live process whose own reads disagree is re-read this many times; a PID that
# keeps changing hands is an unknown, not a clearance.
OBSERVE_ATTEMPTS = 3


def remaining(deadline):
    if time.monotonic() >= deadline:
        raise RuntimeError('Foreign process observation exceeded its deadline')


def read(path, deadline):
    remaining(deadline)
    descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > MAX_BYTES:
            raise ValueError('Process observation is not a bounded regular input')
        value = source.read(MAX_BYTES + 1)
    if len(value) > MAX_BYTES:
        raise ValueError('Process observation exceeds its read bound')
    remaining(deadline)
    return value


def entries(path, deadline):
    remaining(deadline)
    result = []
    with os.scandir(path) as stream:
        for entry in stream:
            if len(result) >= MAX_ENTRIES:
                raise ValueError('Process observation entry bound exceeded')
            result.append(Path(entry.path))
    return result


def exited(process, deadline=None):
    """A dead main thread is insufficient if siblings can still hold files."""
    deadline = time.monotonic()+60 if deadline is None else deadline
    try:
        raw = read(process/'stat', deadline)
        header = raw.rsplit(b')', 1)
        fields = header[-1].split()
        if (len(raw) > 8192 or len(header) != 2
                or not header[0].startswith(os.fsencode(process.name) + b' (')
                or not fields or fields[0] not in {b'Z', b'X'}):
            return False
        tasks = entries(process/'task', deadline)
        return (len(tasks) == 1 and tasks[0].name == process.name
                and stat.S_ISDIR(tasks[0].lstat().st_mode))
    except (FileNotFoundError, ProcessLookupError):
        try:
            process.lstat()
        except (FileNotFoundError, ProcessLookupError):
            return True
        except OSError:
            return False
        return False
    except (OSError, ValueError):
        return False


def identity(process, deadline):
    raw = read(process / 'stat', deadline)
    fields = raw.rsplit(b')', 1)[-1].split()
    if len(fields) < 20 or not fields[19].isdigit():
        raise ValueError('Process identity cannot be observed')
    return fields[0], fields[19]


def children(process, deadline):
    tasks = entries(process / 'task', deadline)
    if not tasks:
        raise ValueError('Process tasks cannot be observed')
    result = set()
    for task in tasks:
        if not task.name.isdecimal():
            raise ValueError('Unexpected process task identity')
        for value in read(task / 'children', deadline).split():
            if not value.isdigit() or int(value) <= 0:
                raise ValueError('Child process identity cannot be observed')
            result.add(str(int(value)))
            if len(result) > MAX_PROCESSES:
                raise ValueError('Foreign process tree exceeds its bound')
    return result


def _note(sink, reason):
    """Record why an observation vetoed, when the caller asked for the reason."""
    if sink is not None:
        sink.append(reason)


def describe(error):
    """Name an error by type and errno alone, never by its text.

    Every path in this module can be derived from a process's environment, argv
    or files, and an OSError raised on such a path repeats the path in str(), in
    filename and in args. Any of those can hold a credential. The type and the
    errno name are whole facts about what failed, and they are the only parts a
    diagnostic may carry: no exception text and no filename ever reaches one.
    """
    kind = type(error).__name__
    number = getattr(error, 'errno', None)
    if not isinstance(number, int):
        return kind
    try:
        return f'{kind}: {errno.errorcode.get(number, number)}'
    except ImportError:
        return kind


def _named(where):
    """A source namer that answers the same place for every token in it."""
    return lambda index, token: where


def _argument_label(index, token):
    """The position of an argument, never its text."""
    return f'argument {index}'


def _environment_label(index, entry):
    """The name of an environment entry, never its value.

    A value can carry a credential, so only the key before "=" is reported. An
    entry with no "=" is anonymous and is reported as the environment.
    """
    key, separator, _value = entry.partition(b'=')
    if not separator or not key:
        return 'the environment'
    return f'environment variable {os.fsdecode(key)}'


def resolves_in(tokens, roots, deadline, excused=None, refused=None, source=None,
                own_output=None):
    """True when a token names a root through resolution, or cannot be bounded.

    This only ever adds a veto to the raw substring test, so together they are
    additive: whichever finds a reference refuses.

    Each token is split on the punctuation that joins a flag or a key to its
    path, and each piece that exists on disk is resolved strictly. A path
    spelled through an alias, with ".." after it, or otherwise unlike the root's
    own bytes still names it. A piece that does not exist names nothing.

    The one token that names this build's own output is exempt, and only that
    token: every other token is resolved and vetoed as usual.

    `own_output` extends that exemption by ancestry, and only by ancestry. A
    piece that RESOLVES TO an ancestor of the process's declared own output is a
    layout directory the build inherited -- the toolchain root, the worktrees
    root -- not a claim on a sibling. It protects that own output and every
    candidate enclosing it, and it never protects a sibling, because the
    exemption is applied by the caller to the candidates it already protects.

    The container is the directory whose entries are the sibling candidates: the
    parent of the innermost candidate enclosing own output. It stands for every
    sibling, so it is never excused, and a nested own output keeps the container
    of the target that encloses it. A piece on a process with no declared own
    output is not an ancestor of anything, so it vetoes.

    A piece that resolves inside own output is the own output being used, unless
    it also lands inside a candidate that own output does not enclose: that
    candidate is a target of its own, not this build's output, so the piece
    keeps the veto.

    `source(index, token)` names where a token was read -- an environment
    variable, an argument, a descriptor -- and never repeats its value, because
    any of those can carry a credential. The recorded reason says that source
    and the managed root it matched.

    An incomplete inspection is a veto, not a clearance: the token bound, the
    piece bound, an unresolvable piece, and a declared output root that does not
    resolve all refuse, and the caller reports why.
    """
    if len(tokens) > MAX_TOKENS:
        raise ValueError('Process observation token bound exceeded')
    # Both lookups are built once from the roots, so a token costs work in its
    # own path only. A census with many candidates and many descriptors would
    # otherwise scan every root for every piece of every source.
    root_set = set(roots)
    # A directory that directly holds a candidate is some sibling's container,
    # so it is never layout.
    containers = {root.parent for root in roots}
    container = None if own_output is None else _container(own_output, root_set)
    name = source or (lambda index, token: 'a process source')
    pieces = 0
    for index, token in enumerate(tokens):
        remaining(deadline)
        if excused is not None and token == excused:
            continue
        # The token whole, its value after a leading KEY= or --flag=, and its
        # pieces: a real path containing a space, "=" or a comma is then
        # resolved as itself, while a flag joined to its path still yields that
        # path. Splitting alone would cut such a name.
        forms = [token]
        if b'=' in token:
            forms.append(token.split(b'=', 1)[1])
        forms.extend(re.split(PIECE_SEPARATOR, token))
        for piece in forms:
            remaining(deadline)
            if not piece or b'/' not in piece and os.altsep is None:
                continue
            pieces += 1
            if pieces > MAX_PIECES:
                raise ValueError('Process observation piece bound exceeded')
            text = os.fsdecode(piece)
            if not os.path.isabs(text):
                continue
            try:
                real = os.path.realpath(text, strict=True)
            except FileNotFoundError:
                # An absent declared output root is an unprovable claim; any
                # other absent path holds nothing and adds nothing.
                if token.split(b'=', 1)[0] in (
                        b'CARGO_TARGET_DIR', b'CARGO_BUILD_TARGET_DIR'):
                    _note(refused, f'{name(index, token)} declares an output that does '
                                   f'not exist, so it cannot be shown to be outside the '
                                   f'managed root')
                    return True
                continue
            except (OSError, ValueError):
                _note(refused, f'{name(index, token)} could not be resolved')
                return True  # A cycle or an unreadable link is unknown evidence.
            candidate = Path(real)
            if own_output is not None:
                inside_own = candidate == own_output or own_output in candidate.parents
                # Rule 1: own output, or a path inside it. A candidate that
                # encloses it and lies above own output is that candidate's
                # output, not this build's, so the piece still vetoes. Testing
                # the candidate's own ancestry keeps this one pass.
                elsewhere = any(root in root_set and root != own_output
                                and root not in own_output.parents
                                for root in (candidate, *candidate.parents))
                if inside_own and not elsewhere:
                    continue
                if (not inside_own and candidate in own_output.parents
                        and candidate != container and candidate not in containers):
                    # Rule 2: a strict ancestor ABOVE the candidate container is
                    # the toolchain or worktrees layout the build inherited, not a
                    # claim on any sibling. A directory that directly holds a
                    # candidate IS a container, so it is not layout and still
                    # vetoes. The caller applies this by protecting only the
                    # candidates that enclose own output.
                    continue
                # Rule 4: the container stands for any sibling, so it falls
                # through and vetoes as before.
            matched = [path for path in (candidate, *candidate.parents)
                       if path in root_set]
            if matched:
                _note(refused, f'{name(index, token)} resolves inside the managed '
                               f'root {matched[0]}')
                return True
    return False


def own_output(environment, arguments):
    """(root, token): the output root this build declares, and the token naming it.

    Exactly one source is honoured: a single absolute CARGO_TARGET_DIR value, or
    one --target-dir argument. The token is the exact bytes that carry it, so a
    caller can exempt that one token and nothing else; the root is resolved
    strictly and never normalised lexically, so an alias spelling answers for the
    root it names. Anything else, including several conflicting values or a
    value that will not resolve, declares nothing and leaves the caller's veto.
    """
    values = []
    if b'CARGO_TARGET_DIR' in environment:
        value = environment[b'CARGO_TARGET_DIR']
        # The token as it appears in environ, so only that token is exempt from
        # the raw byte test and not another that merely names the same path.
        values.append((value, b'CARGO_TARGET_DIR=' + value))
    for index, argument in enumerate(arguments):
        if argument.startswith(b'--target-dir='):
            values.append((argument.split(b'=', 1)[1], argument))
        elif argument == b'--target-dir' and index + 1 < len(arguments):
            values.append((arguments[index + 1], arguments[index + 1]))
    if len(values) != 1:
        return None, None
    value, token = values[0]
    text = os.fsdecode(value)
    if not os.path.isabs(text) or '{' in text or '}' in text:
        return None, None
    try:
        return Path(os.path.realpath(text, strict=True)), token
    except (OSError, ValueError):
        return None, None


def gone(process, deadline):
    """True only when the process provably no longer exists.

    A vanished descriptor, an unreadable source or a replaced PID is not an exit.
    Reading a live process's references can fail for each of those, and treating
    any of them as the process having exited would turn lost evidence into
    clearance for the targets it still holds. So the answer is yes only when the
    process directory itself is gone; anything else stays an unknown.
    """
    try:
        identity(process, deadline)
    except (FileNotFoundError, ProcessLookupError):
        try:
            process.lstat()
        except (FileNotFoundError, ProcessLookupError):
            return True
        except OSError:
            return False
        return False
    return False


def observe(process, references, deadline, responses):
    """One process's own references: True clear, False referenced, None exited.

    A process that has provably exited holds nothing and drops out. A PID whose
    start time moved is a different process and is read afresh. A source that
    fails to read under a live process is retried and then refuses: it is lost
    evidence, never clearance. A response file that vanished with its process is
    that process having exited; a response file missing while the process is
    still alive is unreadable evidence and refuses.
    """
    for _attempt in range(OBSERVE_ATTEMPTS):
        if gone(process, deadline):
            return None
        before = identity(process, deadline)
        if before[0] == b'Z':
            return True, before, set()  # A zombie holds no files.
        missing = None
        try:
            cwd = os.readlink(process / 'cwd')
            arguments = read(process / 'cmdline', deadline)
            environment = read(process / 'environ', deadline)
            if (references(os.fsencode(cwd), _named('working directory'))
                    or references(arguments, _argument_label)
                    or references(environment, _environment_label)):
                return False, before, set()
            for descriptor in entries(process / 'fd', deadline):
                remaining(deadline)
                try:
                    target = os.readlink(descriptor)
                except FileNotFoundError:
                    continue  # This descriptor closed; the rest still count.
                if references(os.fsencode(target),
                              _named(f'descriptor {descriptor.name}')):
                    return False, before, set()
            # Enumerating the split argv makes the index the real argument
            # position, which is what the refusal reports.
            for index, argument in enumerate(arguments.split(b'\0')):
                if not argument.startswith(b'@'):
                    continue
                responses.append(1)
                if len(responses) > MAX_RESPONSES or len(argument) == 1:
                    raise ValueError('Response-file observation exceeds its bound')
                path = Path(os.fsdecode(argument[1:]))
                path = (path if path.is_absolute() else Path(cwd) / path).resolve()
                try:
                    content = read(path, deadline)
                except FileNotFoundError as absent:
                    missing = absent
                    break
                # Name the source by argv position. The path itself comes from
                # the process, and a filename can carry a credential.
                name = _named(f'response file at argv[{index}]')
                if references(os.fsencode(path), name) or references(content, name):
                    return False, before, set()
            else:
                descendants = children(process, deadline)
                after = identity(process, deadline)
        except (FileNotFoundError, ProcessLookupError):
            # A source vanished mid-read: either the process is exiting or its
            # metadata changed. Re-observe; never treat this as clearance.
            continue
        if missing is not None:
            if gone(process, deadline):
                return None  # Its process is gone; the file went with it.
            if identity(process, deadline) != before:
                continue  # A different process holds the PID; read it afresh.
            raise missing  # A response file missing under a live process refuses.
        if after != before:
            continue  # A different process now holds the PID; read it afresh.
        return True, before, descendants
    raise ValueError('Foreign process changed while it was observed')


def _container(own_output, root_set):
    """The directory whose entries are the sibling candidates beside own output.

    Siblings live beside a candidate, so the container is the parent of the
    innermost root that encloses own output: the worktrees root for a worktree
    target, and the target itself for a nested output such as a trybuild
    directory. Walking own output's own ancestors is a single pass, and each
    step is one set lookup. With no enclosing candidate, own output sits in its
    own container, so its parent is the container.
    """
    for path in (own_output, *own_output.parents):
        if path in root_set:
            return path.parent
    return own_output.parent


def _names_ancestor(token, own_output, container):
    """True when a token provably resolves to a strict ancestor above the container.

    Only an existing path counts. An absent or unreadable spelling is not proof
    of an ancestor, so it keeps the raw veto: unknown or unreadable stays blind.
    """
    forms = [token]
    if b'=' in token:
        forms.append(token.split(b'=', 1)[1])
    for form in forms:
        if not form or b'/' not in form:
            continue
        text = os.fsdecode(form)
        if not os.path.isabs(text):
            continue
        try:
            real = Path(os.path.realpath(text, strict=True))
        except (OSError, ValueError):
            continue
        if real in own_output.parents and real != container:
            return True
    return False


def foreign(process, proc, roots, deadline, excused=None, refused=None, own_output=None):
    """Allow only a complete observation without a managed-path reference.

    `excused` is the one environment or argument token that names this build's
    own output root, and only that exact token is exempt from the raw byte test.
    It is never a root, a subtree or a process, so naming its own target cannot
    excuse a second reference beside it.

    No process is signaled and no contents are logged. Response files are read,
    not interpreted or executed; unsupported inputs remain the caller's refusal.

    Stability is judged per process, because a tree that churns continuously can
    still hold nothing of ours. A foreign build launches and reaps compilers for
    its whole run, so a rule that waits for the whole tree to be still refuses
    every pass for as long as that build lasts, and that turned an unrelated
    build into a whole cleanup outage. A process that exits is read as holding
    nothing; a PID that changes hands is read afresh; a live process is judged on
    its own references, and one that references a managed path or cannot be read
    still vetoes.

    Late children are not followed by a second pass. The caller's census walks
    every process, so each live Cargo or compiler is judged here as its own root
    and a path reference is caught there. What this walk must not do is turn
    churn into a refusal, because churn says nothing about which paths a process
    holds.
    """
    raw_roots = [os.fsencode(path.resolve()) for path in roots]
    resolved_roots = [Path(path.resolve()) for path in roots]
    excused = None if excused is None else os.fsencode(excused)
    container = None if own_output is None else _container(own_output, set(resolved_roots))

    def references(value, source=None):
        # `source(index, token)` names the place a token was read from. It never
        # repeats the value, because an argument or an environment value can
        # carry a credential, and the refusal must not copy one.
        name = source or (lambda index, token: 'a process source')
        tokens = value.split(b'\0')
        if resolves_in(tokens, resolved_roots, deadline, excused, refused=refused,
                       source=name, own_output=own_output):
            return True
        # The raw substring test runs on every token, exactly as on main. The
        # one token that *is* this build's own-output source is exempt, because
        # naming where it writes is the target being used. Every other token
        # that carries a managed-root form refuses, whatever it resolves to, so
        # a second reference spelled beneath an ancestor cannot be excused.
        for index, token in enumerate(tokens):
            if excused is not None and token == excused:
                continue
            if any(root in token for root in raw_roots):
                if own_output is not None and _names_ancestor(token, own_output, container):
                    # Rule 2, same as for resolution: a token that provably names
                    # a strict ancestor above the candidate container is layout,
                    # not a claim on a sibling. It is skipped by the byte test
                    # only because it also resolved there; a token that does not
                    # resolve keeps its veto below.
                    continue
                matched = next(root for root in raw_roots if root in token)
                _note(refused, f'{name(index, token)} carries the managed root '
                               f'{os.fsdecode(matched)}')
                return True
        return False

    observed = set()
    responses = []
    pending = [process]
    while pending:
        remaining(deadline)
        current = pending.pop()
        if current.name in observed:
            continue
        if len(observed) >= MAX_PROCESSES:
            raise ValueError('Foreign process tree exceeds its bound')
        result = observe(current, references, deadline, responses)
        if result is None:
            observed.add(current.name)  # Provably gone; it holds nothing.
            continue
        clear, marker, descendants = result
        if not clear:
            return False
        observed.add(current.name)
        pending.extend(proc / pid for pid in descendants if pid not in observed)
    return True
