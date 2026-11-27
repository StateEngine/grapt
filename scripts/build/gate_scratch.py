"""Keep disposable gate fixtures inside one supervisor-owned scratch directory."""
from contextlib import contextmanager
import os
import sys
import tempfile

import tmp_scratch

# The directory a live gate's suite writes into, or None outside scratch().
# main() judges residue inside this context, before the run may publish
# success evidence.
active = None
# The headless fixture base this gate run resolved, or None outside
# scratch(). The gate is the one selector: it resolves before children
# start, publishes the value to their environment, and main() judges this
# retained value after the run, never re-reading the environment.
active_headless_base = None


@contextmanager
def scratch():
    global active, active_headless_base
    keys = ('TMPDIR', 'TMP', 'TEMP')
    previous = {key: os.environ.get(key) for key in keys}
    previous_headless = os.environ.get(HEADLESS_BASE_ENV)
    # RAM gates already override these variables in their child scope. The
    # outer supervisor owns this directory through disk fallback and failure.
    # NVMe scratch keeps a full gate off the RAM-backed /tmp; the system
    # temp remains the fallback when NVMe is unavailable.
    temporary = tempfile.TemporaryDirectory(prefix='secode-gate-',
                                            dir=tmp_scratch.resolve_base())
    directory = temporary.name
    try:
        os.environ.update({key: directory for key in keys})
        active = directory
        # Children consume this value unchanged; they never choose another.
        active_headless_base = resolve_headless_base(directory)
        os.environ[HEADLESS_BASE_ENV] = active_headless_base
        yield directory
    finally:
        active = None
        active_headless_base = None
        for key, value in previous.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        if previous_headless is None:
            os.environ.pop(HEADLESS_BASE_ENV, None)
        else:
            os.environ[HEADLESS_BASE_ENV] = previous_headless
        try:
            temporary.cleanup()
        except OSError as error:
            # Scratch cleanup cannot replace a test failure or cancellation.
            print(f'Gate scratch cleanup incomplete: {error}', file=sys.stderr)


# Headless fixture base: the gate owns the selection (one resolver). The
# override is set-vs-absent: a present but empty value refuses. A governed
# run gets its scratch on Unix, or the drive-level temp on Windows, where
# the user profile's skill sources stay out of the fixture's ancestor
# chain. Nothing is ever created: a missing base refuses the run. A run
# that never enters scratch() is ungoverned, and the fixture keeps its own
# standalone default.
HEADLESS_BASE_ENV = 'SECODE_HEADLESS_FIXTURE_BASE'
HEADLESS_RESIDUE_PREFIX = 'secode-live-headless1-'
WINDOWS_HEADLESS_BASE = 'C:\\temp'


class HeadlessBaseRefused(Exception):
    """The gate refused the run before a child could start."""


def governed_absolute(value):
    """A fully absolute native path: rooted, and on Windows an ordinary
    disk-letter drive with a rooted path — drive-relative (`C:temp`),
    root-only current-drive (`C:`) and UNC/verbatim prefixes refuse. The
    fixture's resolver applies the same contract."""
    if not os.path.isabs(value):
        return False
    if os.name == 'nt':
        drive, path = os.path.splitdrive(value)
        return len(drive) == 2 and drive[1] == ':' and path != ''
    return True


def _is_directory(path):
    try:
        return os.path.isdir(path)
    except OSError:
        return False


def resolve_headless_base(scratch):
    """Select the headless fixture base for one governed run, before any
    child starts; the caller publishes the returned value unchanged."""
    override = os.environ.get(HEADLESS_BASE_ENV)
    if override is not None:
        if override == '':
            raise HeadlessBaseRefused(f'{HEADLESS_BASE_ENV} is set but empty; refusing')
        if not governed_absolute(override):
            raise HeadlessBaseRefused(
                f'{HEADLESS_BASE_ENV} must name a fully absolute (rooted, drive-qualified) '
                f'directory; refusing {override!r}')
        if not _is_directory(override):
            raise HeadlessBaseRefused(
                f'{HEADLESS_BASE_ENV} must name an existing directory; refusing {override!r}')
        return override
    base = WINDOWS_HEADLESS_BASE if os.name == 'nt' else scratch
    if not _is_directory(base):
        raise HeadlessBaseRefused(f'default headless base {base!r} does not exist; '
                                  f'refusing (no auto-creation)')
    return base


def judge_headless_residue(base, require_clean=False):
    """Judge the retained headless base after a governed run.

    Only the resolved value is judged; the environment is not re-read. An
    unobservable base fails a required-clean run: a passing gate must be
    able to say what its fixture base held.
    """
    if base is None:
        return 0
    try:
        entries = os.listdir(base)
    except OSError as error:
        print(f'Gate cannot observe the headless fixture base ({base}): {error}',
              file=sys.stderr)
        return 1 if require_clean else 0
    leftovers = sorted(entry for entry in entries
                       if entry.startswith(HEADLESS_RESIDUE_PREFIX))
    if not leftovers:
        return 0
    if require_clean:
        print('Gate left headless fixture residue (leak; a passing run must not grow its fixture base):',
              file=sys.stderr)
    else:
        print('Gate left headless fixture residue (leak; fails the gate once the headless suite is clean):',
              file=sys.stderr)
    for entry in leftovers[:64]:
        print(f'  {os.path.join(base, entry)}', file=sys.stderr)
    if len(leftovers) > 64:
        print(f'  ... and {len(leftovers) - 64} more', file=sys.stderr)
    return 1 if require_clean else 0


# Test scratch is named by its owner prefix; the gate's own fixtures use
# other names, so a passing gate may leave nothing with these prefixes.
TEST_RESIDUE_PREFIXES = ('secode-', 'evr-')

def residue(scratch, require_clean=False):
    """Check the test scratch a passing gate left behind.

    Every leftover is a leak: the gate's scratch directory is fresh per
    run, so any entry with a test prefix means a passing test grew the
    test TMPDIR. With require_clean the check fails the gate; until the
    crates are all clean the default warns and names the paths to fix,
    because a failing check would turn every batch gate red.
    """
    if scratch is None:
        return 0
    try:
        entries = os.listdir(scratch)
    except OSError as error:
        # The check must never replace a passing gate result.
        print(f'Passing gate left test scratch residue check unreadable ({scratch}): {error}',
              file=sys.stderr)
        return 0
    leftovers = sorted(
        entry for entry in entries
        if entry.startswith(TEST_RESIDUE_PREFIXES)
    )
    if not leftovers:
        return 0
    if require_clean:
        print('Passing gate left test scratch residue (leak; a passing run must not grow the test TMPDIR):',
              file=sys.stderr)
    else:
        print('Passing gate left test scratch residue (leak; fails the gate once all crates are clean):',
              file=sys.stderr)
    for entry in leftovers[:64]:
        print(f'  {os.path.join(scratch, entry)}', file=sys.stderr)
    if len(leftovers) > 64:
        print(f'  ... and {len(leftovers) - 64} more', file=sys.stderr)
    return 1 if require_clean else 0
