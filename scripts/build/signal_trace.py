"""Name the sender of a termination signal before the wrapped child dies.

A gate child killed by a stray signal today leaves no record of who sent it:
the kernel keeps no audit trail for signals, so the failure is unnameable after
the fact. This wrapper runs one long child, records the sender from `siginfo`
(pid and uid) and the sender's `/proc` command line, and then lets the child and
this wrapper die from that same signal.

It never hides, converts or swallows the failure. The wrapper ends with the
child's own status: an ordinary exit keeps the child's code, and a child that
died from a signal makes this wrapper die from that same signal, so the parent
reads the wait status it would have read without the wrapper.

One limit is worth stating plainly: a signal delivered to the child alone is not
observable here. The kernel attributes a signal to a sender only when the
receiver takes it, so this wrapper never sees such a signal in `sigtimedwait`
and cannot name who sent it. The child's death is still reported faithfully by
wait status, which is the half the gate depends on.

Both signals stay blocked in this process for the whole run and are explicitly
unblocked in the child, so a signal cannot be delivered to the default handler
before the trace line is written.
"""
import os
import signal
import subprocess
import sys
from pathlib import Path

TRACED = (signal.SIGTERM, signal.SIGINT)
CMDLINE_LIMIT = 4096
POLL_SECONDS = 0.2
# Unblock the traced signals and exec the real command in place. `preexec_fn`
# would be simpler but is unsafe in a process with threads; this bootstrap
# replaces itself, so it leaves no extra process behind.
BOOTSTRAP = ('import os,signal,sys; '
             'signal.pthread_sigmask(signal.SIG_SETMASK, set()); '
             'os.execv(sys.argv[1], sys.argv[1:])')


def sender_command(pid):
    """The sender's command line, or None when it is gone or unreadable."""
    if not pid:
        return None
    try:
        raw = Path('/proc', str(pid), 'cmdline').read_bytes()
    except (FileNotFoundError, ProcessLookupError, PermissionError):
        return None
    return raw.replace(b'\0', b' ').decode('utf-8', 'replace').strip()[:CMDLINE_LIMIT]


def describe(signum, info):
    """One line naming the signal, its sender, and what the sender was running."""
    pid = getattr(info, 'si_pid', None) or 0
    return 'signal-trace: pid={} signal={} sender_pid={} sender_uid={} sender_cmdline={!r}'.format(
        os.getpid(), signum, pid, getattr(info, 'si_uid', None), sender_command(pid))


def lent_descriptors():
    """The descriptors this wrapper's parent deliberately handed it.

    The gate passes its queue lease, RAM claim and disk admission descriptors
    by inheritance: it marks each one inheritable and names it in `pass_fds`.
    `subprocess` closes every descriptor a child is not told to keep, so a
    wrapper that spawns without forwarding them changes what the traced command
    sees, and the traced command is the gate's longest-lived child.

    The sweep reads the live descriptor table rather than counting upward, so a
    descriptor sitting at any number is found. A bounded numeric range would
    silently miss the high numbers the gate actually lends.
    """
    lent = []
    for entry in os.listdir('/proc/self/fd'):
        try:
            descriptor = int(entry)
        except ValueError:
            continue
        if descriptor < 3:
            continue
        try:
            # Only a descriptor the parent deliberately marked inheritable was
            # lent to this wrapper. A close-on-exec descriptor belongs to some
            # other component; naming it in `pass_fds` would clear that flag
            # and hand it to the traced command, which the parent never did.
            if not os.get_inheritable(descriptor):
                continue
            os.fstat(descriptor)
        except OSError:
            continue
        lent.append(descriptor)
    return lent


def finish_like_child(code, previous):
    """End with the child's own status, never an invented one.

    A child that died from a signal must make this wrapper die from that same
    signal, so its parent reads the identical wait status. Handing a negative
    code to `sys.exit` would instead truncate it to 241, which is neither the
    child's signal nor a status the child chose.
    """
    if code >= 0:
        return code
    signum = -code
    try:
        signal.signal(signum, signal.SIG_DFL)
    except (OSError, ValueError):
        # SIGKILL and SIGSTOP have no handler to replace; they already end or
        # stop this process.
        pass
    signal.pthread_sigmask(signal.SIG_SETMASK, previous)
    os.kill(os.getpid(), signum)
    raise AssertionError('the child signal did not terminate the wrapper')


def run(argv):
    """Run argv, trace any traced signal, then report the child's own status."""
    blocked = set(TRACED)
    previous = signal.pthread_sigmask(signal.SIG_BLOCK, blocked)
    try:
        # The child must not inherit the block, or it could absorb the very
        # signal this wrapper exists to observe.
        #
        # The descriptors the parent lent this wrapper are forwarded unchanged,
        # exactly as the parent named them in its own `pass_fds`. Spawning with
        # only the block lifted would close them, so the traced command would
        # run without the admission its parent admitted it under. Naming a
        # bounded numeric range is not enough: an inherited descriptor can sit
        # at any number.
        child = subprocess.Popen([sys.executable, '-c', BOOTSTRAP, *argv],
                                 pass_fds=tuple(lent_descriptors()))
    except OSError:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous)
        raise
    while True:
        caught = signal.sigtimedwait(blocked, POLL_SECONDS)
        if caught is not None:
            break
        code = child.poll()
        if code is not None:
            return finish_like_child(code, previous)
    print(describe(caught.si_signo, caught), file=sys.stderr, flush=True)
    try:
        child.send_signal(caught.si_signo)
    except ProcessLookupError:
        pass
    return finish_like_child(child.wait(timeout=30), previous)


def wrapped(command):
    """argv that runs `command` under this wrapper."""
    return [sys.executable, '-B', str(Path(__file__).resolve()), '--', *command]


if __name__ == '__main__':
    arguments = sys.argv[1:]
    if arguments[:1] == ['--']:
        arguments = arguments[1:]
    if not arguments:
        raise SystemExit('signal_trace needs a command to wrap')
    sys.exit(run(arguments))
