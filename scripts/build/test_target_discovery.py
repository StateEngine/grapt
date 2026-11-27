"""Recognize retired layouts without treating unrelated qualification runs as ours."""
import contextlib
import hashlib
import io
from pathlib import Path
import tempfile
import time
import unittest

import target_discovery as discovery


class DiscoveryTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.home = Path(temporary.name)
        self.cache = self.home/'cache'; self.cache.mkdir()

    def target(self, path):
        (path/'debug').mkdir(parents=True)
        (path/'CACHEDIR.TAG').write_text('Cargo generated cache')
        return path

    def checkout(self, path):
        (path/'.git').mkdir(parents=True)
        (path/'Cargo.toml').write_text('[workspace]')
        (path/'crates/grapt-types').mkdir(parents=True)
        (path/'crates/grapt-types/Cargo.toml').write_text('[package]\nname="grapt-types"')
        return path

    def discover(self, qualification=None):
        return discovery.discover(self.cache, self.home, time.monotonic()+5, qualification)

    def hashed_target(self, owner):
        key = hashlib.sha256(str(owner.resolve()).encode()).hexdigest()
        target = self.target(self.cache/'target/toolchain/worktrees'/key)
        (target/'debug/artifact').write_bytes(b'disposable build output')
        return target

    def test_a_mismatched_marker_is_not_overridden_by_a_matching_checkout(self):
        owner = self.checkout(self.home/'checkout')
        other = self.checkout(self.home/'other-checkout')
        target = self.hashed_target(owner)
        marker = target/'.secode-worktree'
        marker.write_text(str(other)+'\n')
        report = io.StringIO()
        with contextlib.redirect_stderr(report):
            self.assertNotIn(target, self.discover())
        self.assertEqual(marker.read_text(), str(other)+'\n')
        self.assertIn(str(target), report.getvalue())
        self.assertFalse(discovery.still_owned(target, owner))

    def test_a_malformed_marker_preserves_the_target(self):
        owner = self.checkout(self.home/'checkout')
        target = self.hashed_target(owner)
        for malformed in [b'relative-owner\n', b'/invalid\0owner\n', b'\xff\n']:
            with self.subTest(marker=malformed):
                marker = target/'.secode-worktree'
                marker.write_bytes(malformed)
                report = io.StringIO()
                with contextlib.redirect_stderr(report):
                    self.assertNotIn(target, self.discover())
                self.assertEqual(marker.read_bytes(), malformed)
                self.assertIn(str(target), report.getvalue())
                self.assertFalse(discovery.still_owned(target, owner))

    def test_an_unmatched_markerless_target_is_preserved_and_reported(self):
        owner = self.home/'missing-checkout'
        target = self.hashed_target(owner)
        report = io.StringIO()
        with contextlib.redirect_stderr(report):
            self.assertNotIn(target, self.discover())
        self.assertIn(str(target), report.getvalue())
        self.assertIn('no live checkout matches', report.getvalue())
        self.assertEqual((target/'debug/artifact').read_bytes(), b'disposable build output')
        self.assertFalse((target/'.secode-worktree').exists())

    def test_a_markerless_target_is_attributed_to_its_live_checkout(self):
        owner = self.checkout(self.home/'checkout')
        target = self.hashed_target(owner)
        self.assertEqual(self.discover()[target], owner)
        self.assertTrue(discovery.still_owned(target, owner))
        self.assertFalse((target/'.secode-worktree').exists())

    def test_an_inferred_owner_is_revalidated_before_deletion(self):
        for change in ['move', 'foreign']:
            with self.subTest(change=change):
                owner = self.checkout(self.home/change)
                target = self.hashed_target(owner)
                self.assertEqual(self.discover()[target], owner)
                self.assertTrue(discovery.still_owned(target, owner))
                if change == 'move':
                    moved = owner.rename(self.home/'moved-checkout')
                    self.assertFalse(discovery.still_owned(target, moved))
                else:
                    (owner/'crates/grapt-types/Cargo.toml').unlink()
                self.assertFalse(discovery.still_owned(target, owner))

    def test_flat_hashed_orphan_and_direct_legacy_toolchain_are_discovered(self):
        missing = self.home/'rust-retired'
        key = hashlib.sha256(str(missing).encode()).hexdigest()
        orphan = self.target(self.cache/'target/worktrees'/key)
        (orphan/'.secode-worktree').write_text(str(missing)+'\n')
        legacy = self.target(self.cache/'target/1.97.1')
        observed = self.discover()
        self.assertEqual(observed[orphan], missing)
        self.assertEqual(observed[legacy], legacy)

    def test_seat_named_root_checkout_is_eligible_but_foreign_checkout_is_not(self):
        seat = self.checkout(self.home/'old-seat')
        target = self.target(seat/'target')
        foreign = self.home/'other-project'; (foreign/'.git').mkdir(parents=True)
        (foreign/'Cargo.toml').write_text('[workspace]')
        protected = self.target(foreign/'target')
        observed = self.discover()
        self.assertIn(target, observed); self.assertNotIn(protected, observed)

    def test_only_attributable_qualification_targets_are_discovered(self):
        qualification = self.home/'qualification'; qualification.mkdir()
        owner = self.checkout(self.home/'rust-qualification')
        claimed = self.target(qualification/'secode-run/target')
        (claimed/'.secode-worktree').write_text(str(owner)+'\n')
        foreign = self.target(qualification/'stateengine-run/target')
        observed = self.discover(qualification)
        self.assertEqual(observed[claimed], owner)
        self.assertNotIn(foreign, observed)

    def test_linked_layout_is_never_followed(self):
        external = self.target(self.home/'external')
        (self.cache/'target').mkdir()
        (self.cache/'target/1.97.1').symlink_to(external, target_is_directory=True)
        self.assertEqual(self.discover(), {})
