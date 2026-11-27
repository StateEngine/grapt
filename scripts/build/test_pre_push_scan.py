from pathlib import Path
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

HOOK = Path(__file__).resolve().parent / "git_hooks" / "pre-push"
TRAILER = "Co-Authored-By: Claude <noreply@anthropic.com>"
ZERO = "0" * 40


class PrePushTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(("GIT_", "SECODE_"))}
        self.env.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1",
                        HOME=self.temporary.name, USERPROFILE=self.temporary.name)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "Fixture")
        self.git("config", "user.email", "1+fixture@users.noreply.github.com")
        self.commit("chore(seed): start\n\n" + TRAILER)
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, env=self.env,
                                       text=True).strip()

    def commit(self, message):
        self.git("commit", "-q", "--allow-empty", "-m", message)
        return self.git("rev-parse", "HEAD")

    def push(self, local, remote_sha=ZERO):
        line = f"refs/heads/x {local} refs/heads/x {remote_sha}\n"
        return subprocess.run(["sh", str(HOOK), "origin", "https://example.invalid/r.git"],
                              cwd=self.root, env=self.env, input=line,
                              capture_output=True, text=True)

    def test_a_clean_commit_passes(self):
        sha = self.commit("feat(a): add b\n\n" + TRAILER)
        self.assertEqual(self.push(sha).returncode, 0)

    def test_a_three_line_body_is_blocked(self):
        sha = self.commit("feat(a): add b\n\none\ntwo\nthree\n\n" + TRAILER)
        result = self.push(sha)
        self.assertEqual(result.returncode, 1)
        self.assertIn("body-length", result.stderr)
        self.assertNotIn("no-verify", result.stderr)

    def test_a_missing_trailer_is_blocked(self):
        sha = self.commit("feat(a): add b")
        result = self.push(sha)
        self.assertEqual(result.returncode, 1)
        self.assertIn("trailer", result.stderr)

    def test_a_deletion_is_skipped(self):
        self.commit("feat(a): add b")
        self.assertEqual(self.push(ZERO, "1" * 40).returncode, 0)

    def test_a_new_branch_scans_only_its_new_commits(self):
        bad = self.commit("bad subject")
        self.assertEqual(self.push(bad).returncode, 1)
        self.git("update-ref", "refs/remotes/origin/main", bad)
        sha = self.commit("feat(a): add b\n\n" + TRAILER)
        self.assertEqual(self.push(sha).returncode, 0)

    def test_a_new_branch_without_a_remote_main_scans_unpushed_commits(self):
        self.git("update-ref", "-d", "refs/remotes/origin/main")
        self.git("update-ref", "refs/remotes/origin/other", "HEAD")
        sha = self.commit("feat(a): add b")
        self.assertEqual(self.push(sha).returncode, 1)

    def test_an_installed_hook_blocks_an_actual_local_push(self):
        remote = self.root / "remote.git"
        self.git("init", "--bare", "-q", str(remote))
        self.git("remote", "add", "fixture", str(remote))
        self.git("push", "-q", "fixture", "HEAD:refs/heads/main")
        build = self.root / "scripts" / "build"
        (build / "git_hooks").mkdir(parents=True)
        shutil.copy2(HOOK, build / "git_hooks" / "pre-push")
        for name in ["pre_push_scan.py", "publication_scan.py", "install_hooks.py"]:
            shutil.copy2(HOOK.parent.parent / name, build / name)
        subprocess.check_call([sys.executable, str(build / "install_hooks.py")],
                              cwd=self.root, env=self.env)
        self.commit("feat(a): add b")
        result = subprocess.run(["git", "push", "fixture", "HEAD:refs/heads/review"],
                                cwd=self.root, env=self.env, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertIn("trailer", result.stderr)

    def test_fallback_does_not_rescan_a_merged_remote_branch(self):
        seed = self.git("rev-parse", "HEAD")
        self.git("update-ref", "-d", "refs/remotes/origin/main")
        known = self.commit("bad subject")
        self.git("update-ref", "refs/remotes/origin/other", known)
        self.git("checkout", "-q", "-b", "feature", seed)
        self.commit("feat(a): add b\n\n" + TRAILER)
        self.git("merge", "-q", "--no-ff", known,
                 "-m", "merge(a): join the remote branch\n\n" + TRAILER)
        result = self.push(self.git("rev-parse", "HEAD"))
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_known_remote_tip_wins_over_a_stale_tracking_ref(self):
        remote = self.git("rev-parse", "HEAD")
        sha = self.commit("feat(a): add b")
        self.git("update-ref", "refs/remotes/origin/x", sha)
        result = self.push(sha, remote)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("trailer", result.stderr)

    def test_missing_email_keeps_the_configured_author_name(self):
        sha = self.commit("feat(a): add b\n\n" + TRAILER)
        self.git("config", "user.name", "Selected Fixture")
        self.git("config", "--unset", "user.email")
        result = self.push(sha)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("author-identity", result.stderr)

    def test_no_base_skips_with_a_notice(self):
        self.git("update-ref", "-d", "refs/remotes/origin/main")
        sha = self.commit("bad subject")
        result = self.push(sha)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no publication scan base for refs/heads/x; skipping", result.stderr)


if __name__ == "__main__":
    unittest.main()
