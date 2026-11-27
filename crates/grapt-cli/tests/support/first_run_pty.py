"""The real launcher delegates fresh-profile setup to the engine before auth."""
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import termios
import time


def main():
    binary = Path(sys.argv[1]).resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="grapt-first-run-pty-") as temp:
        root = Path(temp)
        workspace, install, user = (root / name for name in ("workspace", "install", "user"))
        for directory in (workspace, install, user):
            directory.mkdir(mode=0o700)
        shipped = install / "grapt"
        shutil.copy2(binary, shipped)
        calls = root / "engine-calls.jsonl"
        server = install / "grapt-server"
        # Startup stops at the engine boundary; installed TUI captures cover
        # authentication. The stub cannot perform a provider request.
        server.write_text(f'''#!{sys.executable}
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
with open({str(calls)!r}, 'a') as stream:
    stream.write(json.dumps(args) + '\\n')
if args[:2] == ['init-profile', '--service'] and len(args) == 3:
    profile = Path(args[2])
    profile.mkdir(mode=0o700)
    for name, value in [('service.conf', 'slot=product\\ninstance=terminal\\nenvironment=local\\nversion=0.1.0\\n'), ('basic.credential', 'terminal:fixture-service-auth')]:
        fd = os.open(profile / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, 'w') as stream: stream.write(value)
    sys.exit(0)
sys.exit(17)
''')
        server.chmod(0o700)
        profile = root / "profiles" / "fresh"
        env = {"PATH": os.environ["PATH"], "HOME": str(user), "USERPROFILE": str(user),
               "XDG_CONFIG_HOME": str(user / "config"), "XDG_DATA_HOME": str(user / "data"),
               "XDG_CACHE_HOME": str(user / "cache"), "TERM": "xterm-256color"}
        master, slave = pty.openpty()
        before = termios.tcgetattr(slave)
        child = None
        try:
            child = subprocess.Popen([str(shipped), "code", "--profile", str(profile), str(workspace)],
                                     cwd=workspace, env=env, stdin=slave, stdout=slave, stderr=slave,
                                     start_new_session=True,
                                     preexec_fn=lambda: fcntl.ioctl(0, termios.TIOCSCTTY, 0))
            output = bytearray()
            deadline = time.monotonic() + 10
            while child.poll() is None:
                assert time.monotonic() < deadline, "fresh launcher did not reach its engine"
                if select.select([master], [], [], 0.05)[0]:
                    try:
                        output.extend(os.read(master, 4096))
                    except OSError as error:
                        if error.errno != errno.EIO: raise
                    assert len(output) <= 65536, "launcher output exceeded its bound"
            child.wait(timeout=1)
            commands = [json.loads(line) for line in calls.read_text().splitlines()]
            assert commands[0] == ["init-profile", "--service", str(profile)], commands
            assert len(commands) == 2 and commands[1][0] == "serve-terminal", commands
            assert profile.is_dir() and (profile / "basic.credential").is_file()
            assert not (profile / "provider.credential").exists()
            assert not (profile / "chatgpt.oauth.json").exists()
            assert b"Choose 1, 2 or 3" not in output and b"API key (input hidden)" not in output
            assert termios.tcgetattr(slave) == before, "launcher changed terminal input mode"
            assert child.returncode != 0, "fixture must stop before a real service or TUI runs"
            print("PASS empty_profile")
        finally:
            if child is not None:
                try: os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError: pass
                child.wait(timeout=2)
            termios.tcsetattr(slave, termios.TCSANOW, before)
            os.close(master)
            os.close(slave)


if __name__ == "__main__":
    main()
