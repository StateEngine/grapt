"""Public cached crates are verified without copying Cargo credentials."""
import hashlib
import runpy
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import MagicMock, patch


class SeedCargoHomeTests(unittest.TestCase):
    def test_public_copy_refuses_corrupt_bytes_and_preserves_existing_destination(self):
        script = Path(__file__).with_name('seed-cargo-home.py')
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, destination = root / 'old-home', root / 'nvme-home'
            registry = 'index.crates.io-1949cf8c6b5b557f'
            archive = source / 'registry/cache' / registry / 'small-1.0.0.crate'
            archive.parent.mkdir(parents=True)
            archive.write_bytes(b'verified public dependency')
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            index = source / 'registry/index' / registry
            index.mkdir(parents=True)
            (index / 'config.json').write_text('{"dl":"https://static.crates.io/crates"}')
            (source / 'credentials.toml').write_text('local credential fixture; must remain here')
            lock = root / 'Cargo.lock'
            lock.write_text(f'[[package]]\nname="small"\nversion="1.0.0"\n'
                            f'source="registry+https://github.com/rust-lang/crates.io-index"\nchecksum="{digest}"\n')
            command = [sys.executable, str(script), '--lockfile', str(lock),
                       '--source', str(source), '--destination', str(destination)]
            subprocess.run(command, check=True, capture_output=True, timeout=15)
            copied = destination / archive.relative_to(source)
            self.assertEqual(copied.read_bytes(), archive.read_bytes())
            self.assertFalse((destination / 'credentials.toml').exists())
            archive.write_bytes(b'corrupt public dependency')
            refused = subprocess.run(command, capture_output=True, timeout=15)
            self.assertNotEqual(refused.returncode, 0)
            self.assertEqual(hashlib.sha256(copied.read_bytes()).hexdigest(), digest)

            # Stop enumeration at the first excess root rather than consuming
            # an unbounded registry before applying the advertised limit.
            def excessive_roots():
                for _ in range(33):
                    yield object()
                raise AssertionError('Registry enumeration exceeded its bound')
            entries = MagicMock()
            entries.__enter__.return_value = excessive_roots()
            seed = runpy.run_path(str(script))['seed']
            with patch('os.scandir', return_value=entries):
                with self.assertRaisesRegex(ValueError, 'registry cache layout'):
                    seed(lock, source, destination)
