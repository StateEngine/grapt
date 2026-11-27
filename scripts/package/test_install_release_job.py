"""The release unit carries the identity that sends its failure mail."""
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts/build'))
sys.path.insert(0, str(ROOT / 'scripts/package'))
import install_release_job as installer


class ReleaseUnitTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)

    def units(self, actor=None):
        return installer.units(self.base / 'repository', self.base / 'assets.json',
                               self.base / 'output', self.base / 'operator',
                               self.base / 'controller', actor)

    def test_the_actor_and_status_root_reach_the_unit(self):
        service = self.units('agt-fixture-123')['secode-release.service']
        self.assertIn('Environment=GRAPT_RELEASE_ACTOR=agt-fixture-123', service)
        self.assertIn(
            f'Environment=GRAPT_RELEASE_STATUS_ROOT={self.base}/operator/.local/state/secode/release-status',
            service)
        self.assertIn(f'ExecStart=/usr/bin/python3 {self.base}/controller/release_job.py', service)

    def test_without_an_actor_the_unit_sets_no_empty_identity(self):
        service = self.units()['secode-release.service']
        self.assertNotIn('GRAPT_RELEASE_ACTOR', service)

    def test_an_invalid_actor_is_refused(self):
        with self.assertRaises(ValueError):
            self.units('not-an-agent')


if __name__ == '__main__':
    unittest.main()
