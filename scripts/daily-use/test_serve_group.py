"""Light ownership checks; the real pane lifecycle is test_serve_teardown.py."""
import argparse
import errno
import json
import os
from pathlib import Path
import subprocess
import signal
import tempfile
import time
import unittest
from unittest.mock import mock_open, patch

import serve_group


class OwnedGroupTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="serve-group-")
        self.root = Path(self.directory.name)
        self.service = self.root / "service"
        self.service.mkdir()
        binary = self.root / "fixture"
        binary.write_text('''#!/usr/bin/env python3
import json,subprocess,sys,time
from pathlib import Path
service=Path(sys.argv[sys.argv.index('--service')+1])
child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'])
(service/'child.json').write_text(json.dumps({'pid':child.pid}))
if (service/'exit-leader').exists():
    time.sleep(.25)
else:
    time.sleep(60)
''')
        binary.chmod(0o700)
        self.args = argparse.Namespace(binary=binary, service=self.service,
                                       log=self.root / "serve.log",
                                       pidfile=self.root / "owner.json", owner=os.getpid())
        self.record = None
        self.child = None
        self.handlers = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM)}

    def start(self):
        self.child = serve_group.start(self.args)
        self.record = json.loads(self.args.pidfile.read_text())
        for _ in range(100):
            if (self.service / "child.json").exists():
                return
            time.sleep(.05)
        self.fail("fixture never published its child")

    def tearDown(self):
        if self.record:
            serve_group.stop(self.record)
        if self.child:
            self.child.wait(timeout=5)
        for sig, handler in self.handlers.items():
            signal.signal(sig, handler)
        self.directory.cleanup()

    def test_another_driver_cannot_overwrite_or_stop_the_live_owner(self):
        self.start()
        with self.assertRaises(FileExistsError):
            serve_group.start(self.args)
        result = subprocess.run([
            "python3", str(Path(serve_group.__file__)), "stop", "--pidfile",
            str(self.args.pidfile), "--owner", str(os.getpid() + 1)],
            capture_output=True, text=True, timeout=10)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("different evidence driver", result.stderr)
        self.assertTrue(serve_group.live_members(self.record["pid"]))

    def test_reused_leader_identity_is_refused_without_signalling_the_group(self):
        self.start()
        other = dict(self.record, start="not-this-process")
        with self.assertRaisesRegex(RuntimeError, "no longer identifies"):
            serve_group.stop(other)
        self.assertTrue(serve_group.live_members(self.record["pid"]))

    def test_children_are_stopped_when_the_serve_leader_already_exited(self):
        (self.service / "exit-leader").touch()
        self.start()
        for _ in range(100):
            leader = serve_group.process(self.record["pid"])
            if leader is None or leader["state"] == "Z":
                break
            time.sleep(.05)
        else:
            self.fail("fixture leader never exited")
        self.assertTrue(serve_group.live_members(self.record["pid"]))
        serve_group.stop(self.record)
        self.assertEqual(serve_group.live_members(self.record["pid"]), [])

    def interrupt_launch(self, signum):
        spawned = []
        execute = subprocess.Popen._execute_child

        def after_spawn(child, *args, **kwargs):
            execute(child, *args, **kwargs)
            spawned.append(child)
            os.kill(os.getpid(), signum)

        try:
            with patch.object(subprocess.Popen, "_execute_child", after_spawn):
                with self.assertRaisesRegex(InterruptedError, f"signal {signum}"):
                    serve_group.start(self.args)
            self.assertEqual(len(spawned), 1)
            self.assertIsNotNone(spawned[0].poll(), "interrupted launch did not reap its child")
            self.assertEqual(serve_group.live_members(spawned[0].pid), [])
            self.assertFalse(self.args.pidfile.exists())
        finally:
            for child in spawned:
                if child.poll() is None:
                    serve_group.stop({"pid": child.pid, "start": serve_group.process(child.pid)["start"]})
                child.wait(timeout=5)

    def test_int_after_spawn_before_return_cleans_up_and_preserves_failure(self):
        self.interrupt_launch(signal.SIGINT)

    def test_term_after_spawn_before_return_cleans_up_and_preserves_failure(self):
        self.interrupt_launch(signal.SIGTERM)


class ProcessReadTests(unittest.TestCase):
    def test_vanished_process_is_gone_but_unreadable_is_an_error(self):
        for reader in (lambda: serve_group.process(123),
                       lambda: serve_group.command(123, 4096)):
            for number in (errno.ENOENT, errno.ESRCH):
                with self.subTest(reader=reader, errno=number):
                    with patch.object(Path, "open", side_effect=OSError(number, "gone")):
                        self.assertIsNone(reader())
                    opened = mock_open()
                    opened.return_value.read.side_effect = OSError(number, "gone")
                    with patch.object(Path, "open", opened):
                        self.assertIsNone(reader())
            with patch.object(Path, "open", side_effect=PermissionError(errno.EACCES, "unreadable")):
                with self.assertRaises(PermissionError):
                    reader()
            opened = mock_open()
            opened.return_value.read.side_effect = PermissionError(errno.EACCES, "unreadable")
            with patch.object(Path, "open", opened):
                with self.assertRaises(PermissionError):
                    reader()


if __name__ == "__main__":
    unittest.main()
