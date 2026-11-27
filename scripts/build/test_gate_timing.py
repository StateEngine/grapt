"""Serial timing must preserve test output, including adjacent partial lines."""
import io
import unittest
from gate_timing import Observer


class TimingTests(unittest.TestCase):
    def test_a_slow_result_is_reported_only_after_stream_completion(self):
        output=io.StringIO(); now=[0.0]
        observer=Observer(output, lambda: now[0])
        observer.feed(b'Running tests/example.rs (/tmp/example-0123)\ntest suite::slow ... ')
        now[0]=61.25
        observer.feed(b'ok\ntest suite::fast ... ')
        # Reporting inline here would split the next test's result line.
        self.assertEqual(output.getvalue(), '')
        observer.feed(b'ok\n')
        observer.finish()
        self.assertIn('suite::slow 61.250s', output.getvalue())
        self.assertNotIn('suite::fast', output.getvalue())

    def test_bulk_completed_output_is_not_reported_as_a_slow_test(self):
        output=io.StringIO()
        observer=Observer(output, lambda: 100.0)
        observer.feed(b'Running tests/example.rs (/tmp/example-0123)\ntest suite::fast ... ok\n')
        observer.finish()
        self.assertEqual(output.getvalue(), '')

    def test_streaming_log_preserves_utf8_split_across_pipe_reads(self):
        import subprocess
        import sys
        import tempfile
        import time
        from pathlib import Path
        import heavy
        import gate_timing
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary); marker=root/'ack'
            # The writer waits for acknowledgement of the first byte, forcing
            # two reads regardless of machine load.
            script=r'''import sys,time
from pathlib import Path
sys.stdout.buffer.write(b'\xe2');sys.stdout.buffer.flush()
deadline=time.monotonic()+3
while not Path(sys.argv[1]).exists():
    if time.monotonic()>deadline:raise SystemExit(3)
    time.sleep(0.005)
sys.stdout.buffer.write(b'\x82\xac');sys.stdout.buffer.flush()
'''
            with (root/'output').open('w') as output:
                class Buffer:
                    def write(self, data):
                        output.buffer.write(data);output.buffer.flush();marker.touch()
                    def flush(self):output.buffer.flush()
                class Log:
                    buffer=Buffer()
                    def write(self, text):
                        output.write(text);output.flush();marker.touch()
                    def flush(self):output.flush()
                with heavy.owned_process([sys.executable,'-c',script,str(marker)],stdout=subprocess.PIPE) as process:
                    with process.stdout:
                        self.assertEqual(gate_timing.wait(process,Log(),time.monotonic()+5),0)
            self.assertEqual((root/'output').read_bytes(),'€'.encode())
