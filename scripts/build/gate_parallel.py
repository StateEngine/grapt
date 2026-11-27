"""Join native Windows qualification beside the Linux gate command sequence."""
from contextlib import contextmanager
from pathlib import Path
import signal
import subprocess
import time

import heavy
import windows_gate
from windows_native_result import CAPACITY_EXIT, CapacityUnavailable


def wait(process, deadline, check=None):
    while True:
        heavy.check_hold_timeout()
        if check is not None:
            check()
        remaining = deadline-time.monotonic()
        if remaining <= 0:
            raise subprocess.TimeoutExpired(process.args, 0)
        try:
            return process.wait(timeout=min(.2, remaining))
        except subprocess.TimeoutExpired:
            pass


class NativeLeg:
    def __init__(self, process, receipt, commit, packages, workspace, nightly, deadline):
        self.process, self.receipt, self.deadline = process, receipt, deadline
        self.scope = dict(commit=commit, packages=packages, workspace=workspace, nightly=nightly)

    def check(self):
        code = self.process.poll()
        if code == CAPACITY_EXIT:
            raise CapacityUnavailable('Native PC capacity unavailable; see '+str(self.receipt)+'.capacity.json')
        if code:
            raise subprocess.CalledProcessError(code, self.process.args)
        if time.monotonic() >= self.deadline:
            raise subprocess.TimeoutExpired(self.process.args, 0)

    def finish(self):
        wait(self.process, self.deadline, self.check)
        windows_gate.verify_native(self.receipt, **self.scope)


@contextmanager
def native(log, *, commit, packages, workspace=False, nightly=False, deadline, requested=False):
    # Normal qualification keeps Windows-target checks and the final hosted gate.
    if not requested:
        yield None
        return
    receipt = Path(str(log)+'.windows.json')
    # Leave the controller room to join its Windows job and collect capacity
    # results before this enclosing gate's own deadline expires.
    timeout = max(1, min(14400, int(deadline-time.monotonic())-120))
    command = (windows_gate.native_command(packages, commit=commit, receipt=receipt,
               workspace=workspace, nightly=nightly, timeout=timeout)
               if hasattr(windows_gate, 'native_command') else None)
    if command is None:
        yield None
        return
    runner = Path(__file__).resolve().with_name('windows_native.py')
    if Path(command[1]).name == 'windows_native.py' and Path(command[1]).resolve() != runner:
        command[1] = str(runner)
        command += ['--source-policy', str(Path(windows_gate.__file__).resolve().parents[2])]
    driver_log = Path(str(log)+'.windows.driver.log')
    with driver_log.open('x') as output, heavy.owned_process(command, stdout=output,
            stderr=subprocess.STDOUT, pass_fds=heavy.inherited_fds()) as process:
        leg = NativeLeg(process, receipt, commit, packages, workspace, nightly, deadline)
        try:
            yield leg
            leg.finish()
        finally:
            if process.poll() is None:
                # The Windows supervisor cooperatively joins its remote job before
                # exiting. Keep Linux custody while its cancellation completes.
                with heavy.cleanup_signals():
                    process.send_signal(signal.SIGTERM)
                    process.wait(timeout=110)
