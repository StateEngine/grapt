from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import publication_scan


def git(root, *args):
    return subprocess.check_output(["git", *args], cwd=root, text=True).strip()


class AddedLineTests(unittest.TestCase):
    def test_a_moved_file_adds_only_the_lines_its_move_edits(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            git(root, "init", "-q")
            git(root, "config", "user.email", "fixture@example.invalid")
            git(root, "config", "user.name", "fixture")
            body = "".join(f"line {n}\n" for n in range(40))
            (root / "old").mkdir()
            (root / "old" / "lib.rs").write_text(body)
            git(root, "add", "-A")
            git(root, "commit", "-q", "-m", "seed")
            (root / "new").mkdir()
            git(root, "mv", "old/lib.rs", "new/lib.rs")
            (root / "new" / "lib.rs").write_text(body.replace("line 7\n", "line seven\n"))
            git(root, "add", "-A")
            git(root, "commit", "-q", "-m", "move")
            with patch.object(publication_scan, "_git",
                              lambda *args: subprocess.check_output(["git", *args], cwd=root).decode()):
                added = publication_scan.commit_added_lines(git(root, "rev-parse", "HEAD"))
            self.assertEqual(added, [("new/lib.rs", "line seven")])

    def test_rename_aware_exempts_only_lines_whose_change_is_the_rename(self):
        script = Path(__file__).resolve().parents[1] / "rename" / "grapt.py"
        # Spelled indirectly so the rename this case judges cannot rewrite it.
        old = "se" + "code"
        crate, ident = f"{old}-demo", f"{old}_demo"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            git(root, "init", "-q")
            git(root, "config", "user.email", "fixture@example.invalid")
            git(root, "config", "user.name", "fixture")
            (root / "scripts" / "rename").mkdir(parents=True)
            (root / "scripts" / "rename" / "grapt.py").write_text(script.read_text())
            (root / "crates" / crate / "src").mkdir(parents=True)
            (root / "crates" / crate / "Cargo.toml").write_text(f'[package]\nname = "{crate}"\n')
            (root / "crates" / crate / "src" / "lib.rs").write_text(f"use {ident}::Thing;\n")
            git(root, "add", "-A")
            git(root, "commit", "-q", "-m", "seed")
            (root / "crates" / crate / "src" / "lib.rs").write_text(
                "use grapt_demo::Thing;\nlet fresh = 1;\n"
            )
            git(root, "add", "-A")
            git(root, "commit", "-q", "-m", "rename")
            head = git(root, "rev-parse", "HEAD")
            with patch.object(publication_scan, "_git",
                              lambda *args: subprocess.check_output(["git", *args], cwd=root).decode()):
                plain = publication_scan.commit_added_lines(head)
                aware = publication_scan.commit_added_lines(head, rename_aware=True)
            lib = f"crates/{crate}/src/lib.rs"
            self.assertEqual(plain, [(lib, "use grapt_demo::Thing;"), (lib, "let fresh = 1;")])
            self.assertEqual(aware, [(lib, "let fresh = 1;")])



class HygieneTests(unittest.TestCase):
    # Flagged words are assembled so this file never carries them literally.
    WORD = "Del" + "ta"

    def test_a_short_message_id_is_flagged(self):
        self.assertEqual(publication_scan._hygiene_violation("see msg-" + "0123abcd", "fixture"),
                         "floor-msg-id")
        self.assertIsNone(publication_scan._hygiene_violation("see msg-" + "0123abc", "fixture"))

    def test_only_the_exact_identifier_forms_are_exempt(self):
        for line in (f"pub(super) struct {self.WORD} {{", f"fn diff() -> {self.WORD} {{",
                     f"        {self.WORD} {{", f"    {self.WORD}(#[from] Instruction{self.WORD}Error),",
                     f"Err(InstructionError::{self.WORD}(x))"):
            self.assertIsNone(publication_scan._hygiene_violation(line, "fixture"), line)
        for line in (f"{self.WORD} ruled it", f"struct {self.WORD} reviewed by {self.WORD}",
                     f"let name = \"{self.WORD}\";"):
            self.assertEqual(publication_scan._hygiene_violation(line, "fixture"), "seat-name", line)


    def test_only_the_exact_fixture_bound_lines_are_exempt(self):
        name = "Be" + "ta"
        exact = f'    svc.answer(&ev("evt_r")?, &id, &json!({{"name": "{name}"}}), 0)?,'
        self.assertIsNone(publication_scan._hygiene_violation(exact, "fixture"))
        for line in (exact + " // " + name, exact.replace("evt_r", "evt_s")):
            self.assertEqual(publication_scan._hygiene_violation(line, "fixture"), "seat-name", line)


    def test_model_release_stages_are_exempt_only_in_their_exact_form(self):
        alpha, beta = "Al" + "pha", "Be" + "ta"
        for line in (f'        {alpha} => "alpha",', f'        {beta} => "beta",'):
            self.assertIsNone(publication_scan._hygiene_violation(line, "fixture"), line)
        self.assertEqual(publication_scan._hygiene_violation(
            f'        {alpha} => "alpha", // {alpha} ruled', "fixture"), "seat-name")

    def test_upstream_data_exempts_only_its_words_on_its_exact_path(self):
        alpha, beta = "Al" + "pha", "Be" + "ta"
        message = "msg-" + "0123abcd"
        cases = {
            "crates/grapt-connections/src/catalog/models.dev-2026-11-03.json":
                f'{{"name":"Grok 4.20 {beta} Reasoning","description":"Aleph {alpha}\'s model"}}',
            "third-party/regress-0.10.5/src/unicodetables.rs":
                f'        "{alpha}" | "{alpha}betic" => Some({alpha}betic),',
            "third-party/regress-0.10.5/tests/unicode_property_escapes.rs":
                f'const REGEXES: [&str; 1] = ["^\\\\p{{{alpha}}}+$"];',
        }
        for path, line in cases.items():
            self.assertIsNone(publication_scan._hygiene_violation(line, "fixture", path), path)
            self.assertEqual(publication_scan._hygiene_violation(
                line + f" // {alpha} ruled this change", "fixture", path), "seat-name", path)
            self.assertEqual(publication_scan._hygiene_violation(line + " " + message, "fixture", path),
                             "floor-msg-id", path)
            self.assertEqual(publication_scan._hygiene_violation(line, "fixture", path + ".copy"),
                             "seat-name", path)
            self.assertEqual(publication_scan._hygiene_violation(line, "fixture"), "seat-name", path)


    def test_held_finding_keys_exempt_only_the_complete_identifier(self):
        seat = "E" + "TA"
        for path, key in [
            ("crates/grapt-runtime/mutations/manifest.json", f"{seat}-R1-F1"),
            ("crates/grapt-runtime/mutations/manifest.json", f"{seat}-R1-F2"),
            ("crates/grapt-runtime/src/provider_source/tests.rs", f"U2-{seat}-R1-F1"),
            ("crates/grapt-permission/mutations/manifest.json", f"{seat}-R2-01"),
        ]:
            with self.subTest(path=path, key=key):
                self.assertIsNone(publication_scan._hygiene_violation(f'"{key}"', "fixture", path))
                for suffix in ("0", "A", "_copy", "-copy"):
                    with self.subTest(suffix=suffix):
                        self.assertEqual(publication_scan._hygiene_violation(
                            f'"{key}{suffix}"', "fixture", path), "seat-folder")
                self.assertEqual(publication_scan._hygiene_violation(
                    f'"{key}"', "fixture", path+".copy"), "seat-folder")

    def test_held_windows_root_does_not_exempt_a_sibling_directory(self):
        root = "E:/temp/" + "ka" + "ppa-perf-bench02-20261024"
        for path in ("scripts/build/windows_native_worker.py",
                     "scripts/stateengine-native/WINDOWS-BOUND-MCP.md"):
            with self.subTest(path=path):
                self.assertIsNone(publication_scan._hygiene_violation(
                    root+"/.pc-run.lock", "fixture", path))
                self.assertEqual(publication_scan._hygiene_violation(
                    root+"-copy/.pc-run.lock", "fixture", path), "seat-folder")
                self.assertEqual(publication_scan._hygiene_violation(
                    root+"/.pc-run.lock", "fixture", path+".copy"), "seat-folder")

    def test_a_bare_home_root_is_not_an_account(self):
        # A closing quote after the root, or a Windows root ending in a
        # separator, is not an account name: only a name character starts one.
        for line in ('const ROOTS: [&str; 1] = ["/home/"];',
                     'keeps("path /home/ then a space", &["/home/"]);',
                     r'const WINDOWS: &str = "C:\\Users\\";',
                     "/home/" + "example/project", "/home/" + "user/project"):
            with self.subTest(line=line):
                self.assertIsNone(publication_scan._hygiene_violation(line, "fixture"), line)
        # A real account still fails, in prose and in a quoted path alike.
        # The account is assembled so this file never carries the literal.
        account = "ali" + "ce"
        for line in ("cannot read /home/" + account + "/project/file",
                     'r#"cannot read /home/' + account + '/project/file"#',
                     'r#"cannot open C:\\Users\\' + account + '\\project\\x"#'):
            with self.subTest(line=line):
                self.assertEqual(
                    publication_scan._hygiene_violation(line, "fixture"), "real-path", line)

    def test_an_account_segment_may_start_with_a_non_ascii_character(self):
        # A real account name is not restricted to ASCII: an ASCII-only first
        # character would exempt this segment from the scan entirely.
        team = "\u00e9" + "quipe"
        for line in ("cannot read /home/" + team + "/project/file",
                     'r#"cannot read /home/' + team + '/project/file"#',
                     'r#"cannot open C:\\Users\\' + team + '\\project\\x"#'):
            with self.subTest(line=line):
                self.assertEqual(
                    publication_scan._hygiene_violation(line, "fixture"), "real-path", line)


class ExemptDataThroughCommitsTests(unittest.TestCase):
    def test_an_unrelated_attribution_beside_catalog_data_is_still_flagged(self):
        path = "crates/grapt-connections/src/catalog/models.dev-2026-11-03.json"
        beta, alpha = "Be" + "ta", "Al" + "pha"
        clean = f'{{"name":"Grok 4.20 {beta} Reasoning"}}'
        for added, expected in ((clean, []), (clean + f" // {alpha} ruled this change", ["seat-name"])):
            with patch.object(publication_scan, "commit_added_lines", return_value=[(path, added)]), \
                 patch.object(publication_scan, "_scan_commit", return_value=[]):
                found = publication_scan.check_commits(["0" * 40], ("fixture", "fixture@example.invalid"))
            self.assertEqual([line.split(": ")[3] for line in found], expected, added)


class ContributorTests(unittest.TestCase):
    def test_a_contributor_pull_request_is_judged_by_its_content_alone(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            git(root, "init", "-q")
            git(root, "config", "user.email", "1+fixture@users.noreply.github.com")
            git(root, "config", "user.name", "Fixture")
            (root / "notes.txt").write_text("seed\n")
            git(root, "add", "-A")
            git(root, "commit", "-q", "-m", "docs(notes): seed\n\nCo-Authored-By: Claude <noreply@anthropic.com>")
            base = git(root, "rev-parse", "HEAD")
            outsider = ["-c", "user.name=Outside Contributor", "-c", "user.email=outside@example.invalid"]
            (root / "notes.txt").write_text("seed\nclearer wording\n")
            git(root, "add", "-A")
            git(root, *outsider, "commit", "-q", "-m",
                "Fix a typo\n\nSigned-off-by: Outside Contributor <outside@example.invalid>")
            run = (lambda *args: subprocess.check_output(["git", *args], cwd=root).decode())
            with patch.object(publication_scan, "_git", run), \
                 patch.object(publication_scan, "REQUIRED_AUTHOR_NAME", "Fixture"), \
                 patch.object(publication_scan, "REQUIRED_AUTHOR_EMAIL", "1+fixture@users.noreply.github.com"):
                self.assertEqual(publication_scan.check(base, contributor=True), [])
                strict = publication_scan.check(base)
                self.assertTrue(any("author-identity" in line for line in strict), strict)
                self.assertTrue(any("trailer" in line for line in strict), strict)
                (root / "notes.txt").write_text("seed\nclearer wording\nsee msg-" + "0123abcd\n")
                git(root, "add", "-A")
                git(root, *outsider, "commit", "-q", "-m", "More notes")
                leaked = publication_scan.check(base, contributor=True)
            self.assertEqual([line.split(": ")[-2] for line in leaked], ["floor-msg-id"], leaked)


if __name__ == "__main__":
    unittest.main()
