"""Real mount-boundary checks with synthetic inputs and no provider access."""
from pathlib import Path
import shlex
import shutil
import sys
import tempfile
import unittest

import run


class SandboxTests(unittest.TestCase):
    def test_declared_mounts_survive_while_sibling_scratch_stays_hidden(self):
        for parent in [Path(tempfile.gettempdir()), Path.cwd()]:
            with self.subTest(parent=parent), tempfile.TemporaryDirectory(dir=parent) as temporary:
                root = Path(temporary)
                private = root / 'unrelated-state'
                private.write_text('synthetic-private-sentinel')
                binaries, output = root / 'binaries', root / 'output'
                snapshot, service = output / 'repo', output / 'service'
                for directory in [binaries, snapshot, service]:
                    directory.mkdir(parents=True, exist_ok=True)
                binary = binaries / 'native-harness'
                helper = binaries / 'codex-code-mode-host'
                product = binaries / 'product'
                shutil.copy2(shutil.which('sh'), binary)
                for target in [helper, product]:
                    shutil.copy2(shutil.which('true'), target)
                extra = root / 'adapter.py'
                account = root / 'synthetic-account.json'
                credential = service / 'provider.credential'
                for path in [extra, account, credential]:
                    path.write_text('synthetic-selected-input')
                evidence = output / 'saved-state'
                evidence.write_text('synthetic-hidden-evidence')
                selected = [binary, helper, product, extra, account, credential]
                code = f'''import errno
from pathlib import Path
import subprocess
for name in {[str(private), str(evidence)]!r}:
    assert not Path(name).exists(), 'unrelated scratch readable: ' + name
for name in {[str(path) for path in selected]!r}:
    path = Path(name)
    assert path.is_file(), 'declared mount missing: ' + name
    try:
        with path.open('wb') as output:
            output.write(b'changed')
    except OSError as error:
        assert error.errno == errno.EROFS, (name, error)
    else:
        raise AssertionError('declared read-only mount became writable: ' + name)
for name in {[str(binary), str(helper), str(product)]!r}:
    subprocess.run([name], check=True)
for name in {[str(snapshot / 'answer'), str(service / 'session-state')]!r}:
    Path(name).write_text('allowed write')
'''
                environment = run.runtime_environment()
                command = run.agent_sandbox(
                    [str(binary), '-c', f'exec {shlex.quote(sys.executable)} -c {shlex.quote(code)}'],
                    'codex', snapshot, output,
                    environment, product_binary=str(product), account_override=account,
                    service_root=str(service), extra_paths=[extra])
                log = root / 'probe.log'
                status, exit_code, _ = run.bounded_process(
                    command, snapshot, '', log, 10, environment)
                self.assertEqual((status, exit_code), ('completed', 0), log.read_text())
                self.assertEqual(private.read_text(), 'synthetic-private-sentinel')
                self.assertEqual(evidence.read_text(), 'synthetic-hidden-evidence')
                self.assertEqual((snapshot / 'answer').read_text(), 'allowed write')
                self.assertEqual((service / 'session-state').read_text(), 'allowed write')
                for path in [extra, account, credential]:
                    self.assertEqual(path.read_text(), 'synthetic-selected-input')
