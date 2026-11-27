"""Counterexamples for the provenance checks, including the one that was vacuous.

RUN: python -m pytest scripts/daily-use/test_provenance.py -q
  (or: python scripts/daily-use/test_provenance.py, which runs them plainly)

THE BAR THESE ARE WRITTEN TO. A witness earns its place only if it goes RED
when the thing it names is removed. `test_the_original_check_was_vacuous`
exists specifically to hold the corrected code against the defect: it runs the
ORIGINAL predicate against a real-shaped receipt for entirely different bytes
and asserts that predicate accepts -- documenting, executably, that the shipped
check established nothing -- and then asserts the replacement refuses the same
input. Delete `host_provenance`'s nested-field requirement and that second half
fails.
"""

import argparse
import hashlib
import importlib.util
import json
import shutil
import subprocess
import sys
import unittest
from unittest.mock import patch
from pathlib import Path
from tempfile import TemporaryDirectory

sys.path.insert(0, str(Path(__file__).resolve().parent))

from assemble_candidate import assembly_provenance, build_provenance, host_provenance  # noqa: E402
from receipts import (  # noqa: E402
    MAX_RECEIPT_BYTES,
    ProvenanceError,
    digest,
    load_receipt,
    read_bounded,
)

HOST_BYTES = b"desktop host bytes, standing in for 27 MB of real ones"
HOST_SHA = hashlib.sha256(HOST_BYTES).hexdigest()
OTHER_SHA = hashlib.sha256(b"some entirely different artifact").hexdigest()
COMMIT = "fba3e7993fd54e0f70ff85074f24694a18585b5d"


def write(path: Path, payload) -> Path:
    path.write_text(json.dumps(payload), encoding="utf-8")
    return path


class AssemblyProvenance(unittest.TestCase):
    def test_first_unstaged_path_keeps_its_first_character(self):
        with patch("assemble_candidate.subprocess.check_output", return_value=" M scripts/daily-use/assemble_candidate.py\n"):
            result = assembly_provenance(Path("."), COMMIT)
        self.assertFalse(result["clean"])
        self.assertEqual(result["uncommitted_paths"], ["scripts/daily-use/assemble_candidate.py"])


class HostProvenance(unittest.TestCase):
    def setUp(self) -> None:
        self._temp = TemporaryDirectory()
        self.root = Path(self._temp.name)
        self.source = self.root / "grapt-desktop.exe"
        self.source.write_bytes(HOST_BYTES)
        self.copied = self.root / "out" / "grapt-desktop.exe"
        self.copied.parent.mkdir()
        self.copied.write_bytes(HOST_BYTES)

    def tearDown(self) -> None:
        self._temp.cleanup()

    def receipt(self, payload) -> Path:
        return write(self.root / "desktop-build.json", payload)

    def good(self) -> dict:
        """The shape the governed generator actually writes."""
        return {
            "source_commit": COMMIT,
            "host": {"sha256": HOST_SHA, "bytes": len(HOST_BYTES)},
        }

    def check(self, receipt: Path, approved=None):
        return host_provenance(receipt, self.source, self.copied, approved or {HOST_SHA})

    # -- the positive, without which every refusal below is satisfiable by
    # -- refusing everything.
    def test_a_matching_governed_receipt_is_accepted(self):
        described = self.check(self.receipt(self.good()))
        self.assertEqual(described["sha256"], HOST_SHA)
        self.assertEqual(described["bytes"], len(HOST_BYTES))
        self.assertEqual(described["source_commit"], COMMIT)

    # -- ABSENT ------------------------------------------------------------
    def test_a_receipt_with_no_artifact_identity_is_refused(self):
        with self.assertRaises(ProvenanceError) as caught:
            self.check(self.receipt({"source_commit": COMMIT}))
        # THE REASON, not merely a refusal. A missing field that degrades to
        # `None` also refuses -- one step later, as a format complaint -- and
        # asserting only `ProvenanceError` would call that a pass.
        self.assertIn("has no host", str(caught.exception))

    def test_a_receipt_with_no_source_identity_is_refused(self):
        with self.assertRaises(ProvenanceError) as caught:
            self.check(self.receipt({"host": {"sha256": HOST_SHA, "bytes": len(HOST_BYTES)}}))
        self.assertIn("has no source_commit", str(caught.exception))

    def test_an_empty_receipt_is_refused(self):
        with self.assertRaises(ProvenanceError) as caught:
            self.check(self.receipt({}))
        self.assertIn("has no host", str(caught.exception))

    # -- MALFORMED ---------------------------------------------------------
    def test_a_receipt_that_is_not_an_object_is_refused(self):
        """Refused AS A SHAPE, not as a pile of absent fields.

        A list coerced to `{}` also refuses, complaining that `host` is
        missing -- a true statement about the wrong problem, and it would let
        the coercion back in unnoticed.
        """
        with self.assertRaises(ProvenanceError) as caught:
            self.check(self.receipt([{"host": {"sha256": HOST_SHA}}]))
        self.assertIn("not a receipt object", str(caught.exception))

    def test_unparseable_json_is_refused(self):
        path = self.root / "desktop-build.json"
        path.write_text("{not json", encoding="utf-8")
        with self.assertRaises(ProvenanceError):
            self.check(path)

    def test_a_non_digest_identity_is_refused(self):
        payload = self.good()
        payload["host"]["sha256"] = "a81ae07d"
        with self.assertRaises(ProvenanceError) as caught:
            self.check(self.receipt(payload))
        # Refused for its SHAPE. Dropping the format check leaves this
        # refusing too -- as a value mismatch -- so the reason is the witness.
        self.assertIn("is not a sha256 digest", str(caught.exception))

    def test_a_null_byte_count_is_refused(self):
        payload = self.good()
        payload["host"]["bytes"] = None
        with self.assertRaises(ProvenanceError) as caught:
            self.check(self.receipt(payload))
        self.assertIn("is not a byte count", str(caught.exception))

    def test_a_boolean_byte_count_is_refused(self):
        """`True` is an `int` in Python and equals 1. It is not a length."""
        payload = self.good()
        payload["host"]["bytes"] = True
        with self.assertRaises(ProvenanceError) as caught:
            self.check(self.receipt(payload))
        self.assertIn("is not a byte count", str(caught.exception))

    def test_a_truncated_commit_is_refused(self):
        payload = self.good()
        payload["source_commit"] = "fba3e79"
        with self.assertRaises(ProvenanceError) as caught:
            self.check(self.receipt(payload))
        self.assertIn("is not a commit identity", str(caught.exception))

    # -- MISMATCHED --------------------------------------------------------
    def test_a_receipt_for_other_bytes_is_refused(self):
        payload = self.good()
        payload["host"]["sha256"] = OTHER_SHA
        with self.assertRaises(ProvenanceError) as caught:
            self.check(self.receipt(payload), approved={OTHER_SHA})
        self.assertIn("says sha256", str(caught.exception))

    def test_a_receipt_whose_length_disagrees_with_the_artifact_is_refused(self):
        payload = self.good()
        payload["host"]["bytes"] = len(HOST_BYTES) + 1
        with self.assertRaises(ProvenanceError) as caught:
            self.check(self.receipt(payload))
        self.assertIn("bytes", str(caught.exception))

    def test_a_copy_that_differs_from_the_source_is_refused(self):
        """The source can match while the copy does not: a truncated write."""
        self.copied.write_bytes(HOST_BYTES[:10])
        with self.assertRaises(ProvenanceError) as caught:
            self.check(self.receipt(self.good()))
        self.assertIn("host copy", str(caught.exception))

    def test_a_source_that_differs_from_the_receipt_is_refused_at_the_source(self):
        """The SOURCE check, witnessed independently of the copy check.

        Every other mismatch case here has copy == source, so the copy
        comparison catches them all and deleting the source comparison changes
        nothing -- the resurrection battery said so, as a survivor. This is the
        one arrangement that separates them: the copy in the output matches the
        receipt, and the artifact we were POINTED AT does not. That is copying
        one artifact and shipping another's provenance, and it is refused
        naming the source.
        """
        self.source.write_bytes(HOST_BYTES + b"drifted after the copy was taken")
        with self.assertRaises(ProvenanceError) as caught:
            self.check(self.receipt(self.good()))
        self.assertIn("host source", str(caught.exception))

    def test_an_unapproved_host_identity_is_refused(self):
        """Internally consistent is not the same as approved.

        Every field here agrees with every other -- receipt, source and copy
        are one artifact. It is simply not the host we are allowed to reuse.
        """
        with self.assertRaises(ProvenanceError) as caught:
            self.check(self.receipt(self.good()), approved={OTHER_SHA})
        self.assertIn("approved", str(caught.exception))


class BuildProvenance(unittest.TestCase):
    """The source-to-build link, which previously did not exist.

    The first version wrote `"build": "cargo build -p grapt-product ..."` --
    a string the assembler composed from its own arguments. It named a command
    nobody observed and would have been written verbatim for bytes built by
    anything, anywhere, in any profile. These require the receipt to describe
    THESE bytes, at THIS commit, from a clean tree, by a recorded command.
    """

    PROFILE = "debug"
    TARGET = "x86_64-pc-windows-msvc"
    HEAD = "36cad6e822d400e90c7ede29efabdf66bde3a0ed"

    def setUp(self) -> None:
        self._temp = TemporaryDirectory()
        self.root = Path(self._temp.name)
        self.product = self.root / "grapt-server.exe"
        self.product.write_bytes(b"product bytes")
        self.copies = {"grapt-server.exe": (self.product, digest(self.product))}

    def tearDown(self) -> None:
        self._temp.cleanup()

    def good(self) -> dict:
        sha, size = self.copies["grapt-server.exe"][1]
        return {
            "schema": "secode.daily-use.candidate-build.v1",
            "source_commit": self.HEAD,
            "tree_dirty": False,
            "profile": self.PROFILE,
            "target": self.TARGET,
            "commands": [
                {"argv": ["cargo", "build"], "cwd": ".", "produces": "grapt-server.exe"}
            ],
            "outputs": {
                "grapt-server.exe": {
                    "sha256": sha,
                    "bytes": size,
                    "package": "grapt-product",
                    "bin": "grapt-server",
                }
            },
        }

    def check(self, payload, head=None):
        path = write(self.root / "candidate-build.json", payload)
        # The rust root is only consulted when the receipt's commit differs
        # from the checkout's; these cases hold them equal, so the allowlist
        # and descendant checks are deliberately not on this path.
        return build_provenance(
            path, self.root, head or self.HEAD, self.PROFILE, self.TARGET, self.copies
        )

    def test_a_matching_build_receipt_is_accepted(self):
        described = self.check(self.good())
        entry = described["grapt-server.exe"]
        self.assertEqual(entry["source_commit"], self.HEAD)
        self.assertEqual(entry["package"], "grapt-product")
        self.assertEqual(entry["build_commands"][0]["argv"], ["cargo", "build"])

    def test_a_receipt_for_another_commit_is_refused(self):
        with self.assertRaises(ProvenanceError) as caught:
            self.check(self.good(), head="0" * 40)
        self.assertIn("does not descend from the attested", str(caught.exception))

    def test_a_dirty_build_tree_is_refused(self):
        payload = self.good()
        payload["tree_dirty"] = True
        payload["tree_dirty_detail"] = " M crates/grapt-product/src/lib.rs"
        with self.assertRaises(ProvenanceError) as caught:
            self.check(payload)
        self.assertIn("clean tree at build time", str(caught.exception))

    def test_a_receipt_that_omits_the_tree_state_is_refused(self):
        """ABSENT is not CLEAN. `.get` returning None must not read as false."""
        payload = self.good()
        del payload["tree_dirty"]
        with self.assertRaises(ProvenanceError) as caught:
            self.check(payload)
        self.assertIn("clean tree at build time", str(caught.exception))

    def test_a_receipt_for_a_different_profile_is_refused(self):
        payload = self.good()
        payload["profile"] = "release"
        with self.assertRaises(ProvenanceError) as caught:
            self.check(payload)
        self.assertIn("profile", str(caught.exception))

    def test_a_receipt_for_a_different_target_is_refused(self):
        payload = self.good()
        payload["target"] = "aarch64-pc-windows-msvc"
        with self.assertRaises(ProvenanceError) as caught:
            self.check(payload)
        self.assertIn("target", str(caught.exception))

    def test_a_receipt_describing_other_output_bytes_is_refused(self):
        payload = self.good()
        payload["outputs"]["grapt-server.exe"]["sha256"] = OTHER_SHA
        with self.assertRaises(ProvenanceError) as caught:
            self.check(payload)
        self.assertIn("says sha256", str(caught.exception))

    def test_a_receipt_with_no_entry_for_a_packaged_binary_is_refused(self):
        payload = self.good()
        payload["outputs"] = {}
        with self.assertRaises(ProvenanceError) as caught:
            self.check(payload)
        self.assertIn("has no outputs", str(caught.exception))

    def test_a_receipt_with_no_commands_is_refused(self):
        payload = self.good()
        payload["commands"] = []
        with self.assertRaises(ProvenanceError) as caught:
            self.check(payload)
        self.assertIn("records no build commands", str(caught.exception))

    def test_a_command_producing_SEVERAL_binaries_accounts_for_each(self):
        """One cargo invocation builds both binaries, so `produces` is a LIST.

        The governed driver records it that way. Requiring a bare string would
        have made this check unsatisfiable against the run's own evidence --
        the check would have been not merely weak but always-false.
        """
        payload = self.good()
        payload["commands"][0]["produces"] = ["grapt-server.exe", "grapt.exe"]
        described = self.check(payload)
        self.assertEqual(described["grapt-server.exe"]["source_commit"], self.HEAD)

    def test_a_list_that_omits_this_binary_is_still_refused(self):
        payload = self.good()
        payload["commands"][0]["produces"] = ["grapt.exe"]
        with self.assertRaises(ProvenanceError) as caught:
            self.check(payload)
        self.assertIn("no command producing", str(caught.exception))

    def test_a_null_produces_does_not_read_as_a_match(self):
        payload = self.good()
        payload["commands"][0]["produces"] = None
        with self.assertRaises(ProvenanceError) as caught:
            self.check(payload)
        self.assertIn("no command producing", str(caught.exception))

    def test_a_binary_no_recorded_command_claims_to_produce_is_refused(self):
        """A receipt may record commands and still not account for this file."""
        payload = self.good()
        payload["commands"][0]["produces"] = "grapt.exe"
        with self.assertRaises(ProvenanceError) as caught:
            self.check(payload)
        self.assertIn("no command producing", str(caught.exception))


class AssemblyCheckout(unittest.TestCase):
    """When may a package be assembled at a LATER checkout than it was built at?

    Only when nothing that could change a binary moved. Asking whether
    `crates/`, `Cargo.toml` and `Cargo.lock` differ does not establish that:
    a toolchain file, `.cargo/config`, a build helper or an included asset
    changes the bytes without touching any of them. So the rule is an
    ALLOWLIST, and these fix which side of it the interesting paths fall on.
    """

    def repo(self, temp):
        root = Path(temp)
        run = lambda *a: subprocess.run(
            a, cwd=root, check=True, capture_output=True, text=True
        )
        run("git", "init", "-q")
        run("git", "config", "user.email", "witness@example.invalid")
        run("git", "config", "user.name", "witness")
        (root / "crates").mkdir()
        (root / "crates" / "a.rs").write_text("//\n", encoding="utf-8")
        run("git", "add", "-A")
        run("git", "commit", "-qm", "base")
        # The binary lives OUTSIDE the repository, as a governed run's returned
        # artifact does. Committing it into the tree made it a changed path and
        # the allowlist refused it -- a fault in this fixture, not the rule.
        artifacts = root.parent / "artifacts"
        artifacts.mkdir(exist_ok=True)
        built = subprocess.check_output(
            ("git", "rev-parse", "HEAD"), cwd=root, text=True
        ).strip()
        return root, run, built, artifacts

    def provenance(self, root, receipt_commit, head, copies):
        payload = {
            "source_commit": receipt_commit,
            "tree_dirty": False,
            "profile": "release",
            "target": "x86_64-pc-windows-msvc",
            "commands": [{"argv": ["cargo"], "cwd": ".", "produces": "grapt-server.exe"}],
            "outputs": {
                "grapt-server.exe": {
                    "sha256": copies["grapt-server.exe"][1][0],
                    "bytes": copies["grapt-server.exe"][1][1],
                    "package": "grapt-product",
                    "bin": "grapt-server",
                }
            },
        }
        path = write(root / "candidate-build.json", payload)
        return build_provenance(path, root, head, "release", "x86_64-pc-windows-msvc", copies)

    def test_a_fixture_only_later_checkout_is_accepted(self):
        with TemporaryDirectory() as temp:
            root, run, built, artifacts = self.repo(temp)
            binary = artifacts / "grapt-server.exe"
            binary.write_bytes(b"product")
            copies = {"grapt-server.exe": (binary, digest(binary))}
            (root / "scripts").mkdir()
            (root / "scripts" / "daily-use").mkdir()
            (root / "scripts" / "daily-use" / "s.json").write_text("{}", encoding="utf-8")
            run("git", "add", "-A")
            run("git", "commit", "-qm", "fixture only")
            head = subprocess.check_output(
                ("git", "rev-parse", "HEAD"), cwd=root, text=True
            ).strip()
            described = self.provenance(root, built, head, copies)
            self.assertEqual(described["grapt-server.exe"]["source_commit"], built)

    def test_a_later_checkout_touching_a_build_input_is_refused(self):
        """`rust-toolchain.toml` is the case an exclusion list would miss."""
        with TemporaryDirectory() as temp:
            root, run, built, artifacts = self.repo(temp)
            binary = artifacts / "grapt-server.exe"
            binary.write_bytes(b"product")
            copies = {"grapt-server.exe": (binary, digest(binary))}
            (root / "rust-toolchain.toml").write_text("[toolchain]\n", encoding="utf-8")
            run("git", "add", "-A")
            run("git", "commit", "-qm", "toolchain moved")
            head = subprocess.check_output(
                ("git", "rev-parse", "HEAD"), cwd=root, text=True
            ).strip()
            with self.assertRaises(ProvenanceError) as caught:
                self.provenance(root, built, head, copies)
            self.assertIn("outside the fixture allowlist", str(caught.exception))
            self.assertIn("rust-toolchain.toml", str(caught.exception))


class Bounds(unittest.TestCase):
    """Bounds are witnessed BY ORDER, not by outcome.

    Asserting only that an oversize input raises would pass with the bound
    checked after the whole file had been read into memory -- which is the
    defect, not the fix. So these count actual read calls and require the
    refusal to arrive having consumed ZERO bytes.
    """

    def test_an_oversize_receipt_is_refused_WITHOUT_BEING_READ(self):
        with TemporaryDirectory() as temp:
            path = Path(temp) / "huge.json"
            with path.open("wb") as handle:
                handle.truncate(MAX_RECEIPT_BYTES + 1)

            opened: list[str] = []
            texts: list[str] = []
            real_open, real_read_text = Path.open, Path.read_text

            def counted_open(self_path, *args, **kwargs):
                opened.append(str(self_path))
                return real_open(self_path, *args, **kwargs)

            def counted_read_text(self_path, *args, **kwargs):
                texts.append(str(self_path))
                return real_read_text(self_path, *args, **kwargs)

            Path.open, Path.read_text = counted_open, counted_read_text
            try:
                with self.assertRaises(ProvenanceError) as caught:
                    load_receipt(path)
            finally:
                Path.open, Path.read_text = real_open, real_read_text

            self.assertIn("bound", str(caught.exception))
            self.assertEqual([], texts, "the oversize receipt must never be read")
            self.assertEqual([], opened, "the oversize receipt must never be opened")

    def test_an_oversize_artifact_is_refused_BEFORE_ANY_BYTE_IS_READ(self):
        with TemporaryDirectory() as temp:
            path = Path(temp) / "big.bin"
            path.write_bytes(b"0123456789")
            opened: list[str] = []
            real_open = Path.open

            def counted_open(self_path, *args, **kwargs):
                opened.append(str(self_path))
                return real_open(self_path, *args, **kwargs)

            Path.open = counted_open
            try:
                with self.assertRaises(ProvenanceError):
                    digest(path, bound=4)
            finally:
                Path.open = real_open
            self.assertEqual(
                [], opened, "the artifact must never be opened once over bound"
            )

    def test_a_file_that_GROWS_AFTER_ITS_SIZE_IS_CHECKED_is_refused(self):
        """ALPHA'S COUNTEREXAMPLE, verbatim in shape.

        He ran my 80ab614 receipts.py with an 8-byte bound and a file that
        reports 1 byte at stat and supplies 21 at read. `load_receipt`
        ACCEPTED the oversized payload, because I checked stat and then called
        the unbounded `read_text`. The stat was a preflight I had described as
        a limit, and no enormous file is needed to show it -- the two
        observations are simply of different instants.

        This reproduces that race deterministically by lying at stat, and
        requires the refusal to come from the READ.
        """
        with TemporaryDirectory() as temp:
            path = Path(temp) / "grows.json"
            path.write_bytes(b'{"host":{"sha256":"x"}}')  # 23 bytes on disk

            real_stat = Path.stat

            def understating_stat(self_path, *args, **kwargs):
                result = real_stat(self_path, *args, **kwargs)
                if self_path == path:
                    class Understated:
                        st_size = 1
                    return Understated()
                return result

            Path.stat = understating_stat
            try:
                with self.assertRaises(ProvenanceError) as caught:
                    read_bounded(path, 8)
            finally:
                Path.stat = real_stat
            self.assertIn("after its size was checked", str(caught.exception))

    def test_an_oversized_receipt_is_never_fully_ALLOCATED(self):
        """The cap on the READ, witnessed by what it allocates.

        Refusing an oversized file is not the same as refusing it cheaply.
        `handle.read()` and `handle.read(bound + 1)` both end in the same
        refusal, so outcome alone cannot tell them apart -- the resurrection
        battery said exactly that, as a survivor. What separates them is the
        allocation: this records every read size and requires that nothing
        larger than one byte past the bound was ever pulled into memory.
        """
        bound = 32
        with TemporaryDirectory() as temp:
            path = Path(temp) / "oversized.json"
            path.write_bytes(b"[" + b'"padding",' * 500 + b'"end"]')

            # The stat preflight would refuse this file before any read, so it
            # is understated exactly as in the review's counterexample. That is the
            # arrangement where the read cap is the ONLY thing standing
            # between an oversized file and full allocation.
            real_stat = Path.stat

            def understating_stat(self_path, *args, **kwargs):
                if self_path == path:
                    class Understated:
                        st_size = 1

                    return Understated()
                return real_stat(self_path, *args, **kwargs)

            sizes: list[int] = []
            real_open = Path.open

            class Recording:
                def __init__(self, handle):
                    self._handle = handle

                def read(self, size=-1):
                    payload = self._handle.read(size)
                    sizes.append(len(payload))
                    return payload

                def __enter__(self):
                    return self

                def __exit__(self, *exc):
                    self._handle.close()
                    return False

            def recording_open(self_path, *args, **kwargs):
                return Recording(real_open(self_path, *args, **kwargs))

            Path.open, Path.stat = recording_open, understating_stat
            try:
                with self.assertRaises(ProvenanceError):
                    read_bounded(path, bound)
            finally:
                Path.open, Path.stat = real_open, real_stat

            self.assertTrue(sizes, "the file must actually have been read")
            self.assertLessEqual(
                max(sizes),
                bound + 1,
                f"no read may allocate past the bound; saw {max(sizes)} bytes",
            )

    def test_a_symbolic_link_is_refused_rather_than_followed(self):
        """`resolve()` and `is_file()` both follow links, so neither is this."""
        with TemporaryDirectory() as temp:
            target = Path(temp) / "real.json"
            target.write_bytes(b"{}")
            link = Path(temp) / "link.json"
            try:
                link.symlink_to(target)
            except (OSError, NotImplementedError):
                self.skipTest("this platform/user cannot create symbolic links")
            with self.assertRaises(ProvenanceError) as caught:
                load_receipt(link)
            self.assertIn("not an ordinary file", str(caught.exception))

    def test_a_directory_is_refused_as_a_receipt(self):
        with TemporaryDirectory() as temp:
            with self.assertRaises(ProvenanceError) as caught:
                load_receipt(Path(temp))
            self.assertIn("not an ordinary file", str(caught.exception))

    def test_a_digest_counts_the_length_it_actually_hashed(self):
        with TemporaryDirectory() as temp:
            path = Path(temp) / "artifact.bin"
            path.write_bytes(HOST_BYTES)
            self.assertEqual(digest(path), (HOST_SHA, len(HOST_BYTES)))


class TheOriginalDefect(unittest.TestCase):
    """The shipped check, run against a real-shaped receipt for other bytes.

    This is not a hypothetical. `receipt.get("sha256")` reads a key the
    governed generator does not write; it returned `None`; `None` was an
    accepted value. The predicate below is the shipped one, character for
    character in behaviour, and the first assertion shows it accepting a
    receipt that describes an artifact we do not have.
    """

    def test_the_original_check_was_vacuous_and_the_replacement_is_not(self):
        with TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / "grapt-desktop.exe"
            source.write_bytes(HOST_BYTES)
            copied = root / "out.exe"
            copied.write_bytes(HOST_BYTES)
            path = write(
                root / "desktop-build.json",
                {"source_commit": COMMIT, "host": {"sha256": OTHER_SHA, "bytes": 999}},
            )
            receipt = load_receipt(path)
            host_sha, _ = digest(source)

            shipped_would_refuse = receipt.get("sha256") not in (None, host_sha)
            self.assertFalse(
                shipped_would_refuse,
                "the shipped check accepted a receipt for entirely different bytes",
            )

            with self.assertRaises(ProvenanceError):
                host_provenance(path, source, copied, {OTHER_SHA})


REPO = Path(__file__).resolve().parents[2]
TARGET = "x86_64-pc-windows-msvc"
EXES = {
    "grapt-server.exe": b"release product bytes",
    "grapt.exe": b"release cli bytes",
    "grapt-migrate.exe": b"release migration bytes",
}


def load_module(name: str, path: Path, source: str | None = None):
    """Import a script by path, optionally from mutated source text."""
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    if source is None:
        spec.loader.exec_module(module)
    else:
        exec(compile(source, str(path), "exec"), module.__dict__)
    return module


class MigrationPackage(unittest.TestCase):
    """R6-CUTOVER-SOAK-01 r13/r14: the migration binary through the whole chain.

    The governed crossbuild (`remote-compute/daily-use-windows-build.py`) builds
    it, `build_candidate.py` hashes it into the candidate-build receipt, and
    `assemble_candidate.py` packages it only if that receipt describes these
    bytes as `grapt-release`/`grapt-migrate`. Every step is the real code;
    only cargo, git and the governed carrier are stood in for.
    """

    def setUp(self) -> None:
        self._temp = TemporaryDirectory()
        base = Path(self._temp.name)
        self.rust = base / "rust"
        self.rust.mkdir()
        git = lambda *a: subprocess.run(("git", *a), cwd=self.rust, check=True, capture_output=True)
        git("init", "-q")
        git("config", "user.email", "witness@example.invalid")
        git("config", "user.name", "witness")
        (self.rust / "crates").mkdir()
        (self.rust / "crates" / "a.rs").write_text("//\n", encoding="utf-8")
        git("add", "-A")
        git("commit", "-qm", "base")
        self.head = subprocess.check_output(("git", "rev-parse", "HEAD"), cwd=self.rust, text=True).strip()
        self.run_dir = base / "governed-run"
        self.argv = self.governed_build(base / "cargo-target")
        self.host = base / "grapt-desktop.exe"
        self.host.write_bytes(HOST_BYTES)
        self.host_receipt = write(base / "host.json", {
            "host": {"sha256": HOST_SHA, "bytes": len(HOST_BYTES)}, "source_commit": COMMIT})
        self.notices = base / "THIRD-PARTY-NOTICES.txt"
        self.notices.write_text("notices\n", encoding="utf-8")
        self.base = base

    def tearDown(self) -> None:
        self._temp.cleanup()

    def governed_build(self, target_dir: Path) -> list:
        """Run the real crossbuild driver with cargo, rustup and git stood in for."""
        builder = load_module("windows_build", REPO / "remote-compute" / "daily-use-windows-build.py")
        release = target_dir / builder.TARGET / builder.PROFILE
        release.mkdir(parents=True)
        for name, data in EXES.items():
            (release / name).write_bytes(data)
        argvs = []

        def run(argv, timeout):
            argvs.append(argv)
            builder.STEPS.append({"argv": argv, "cwd": "."})

        with patch.object(builder, "OUTPUT", self.run_dir), \
                patch.object(builder, "run", side_effect=run), \
                patch.object(builder, "require_toolchain"), \
                patch.object(builder, "custody", return_value=(self.head, "")), \
                patch.dict("os.environ", {"CARGO_TARGET_DIR": str(target_dir)}), \
                patch("builtins.print"):
            self.assertEqual(builder.main(), 0)
        return argvs[-1]

    def capture(self) -> Path:
        adapter = load_module("build_candidate_under_test", Path(__file__).with_name("build_candidate.py"))
        receipt = self.base / "candidate-build.json"
        with patch("builtins.print"):
            adapter.capture(argparse.Namespace(governed_run=str(self.run_dir), receipt=str(receipt)))
        return receipt

    def assemble(self, receipt: Path, module=None) -> Path:
        assembler = module or load_module("assemble_under_test", Path(__file__).with_name("assemble_candidate.py"))
        output = self.base / "package"
        with patch("builtins.print"):
            assembler.assemble(argparse.Namespace(
                output=str(output), rust_root=str(self.rust), host=str(self.host),
                host_receipt=str(self.host_receipt), build_receipt=str(receipt), built=str(self.run_dir),
                approved_host_sha256=[HOST_SHA], notices=str(self.notices), accepted_base=self.head,
                profile="release", target=TARGET))
        return output

    def edit_receipt(self, receipt: Path, change) -> Path:
        payload = json.loads(receipt.read_text(encoding="utf-8"))
        change(payload)
        return write(receipt, payload)

    def test_the_crossbuild_builds_the_migration_binary_at_two_jobs(self):
        argv = self.argv
        self.assertEqual(argv[argv.index("-j") + 1], "2")
        self.assertIn("--release", argv)
        self.assertNotIn("--features", argv)
        pairs = [(argv[i + 1], argv[i + 3]) for i, token in enumerate(argv) if token == "-p"]
        self.assertIn(("grapt-release", "grapt-migrate"), pairs)
        evidence = json.loads((self.run_dir / "build-evidence.json").read_text(encoding="utf-8"))
        self.assertEqual(sorted(evidence["outputs"]), sorted(EXES))
        self.assertEqual(evidence["commands"][-1]["produces"], sorted(EXES))

    def test_a_complete_three_binary_package_succeeds(self):
        output = self.assemble(self.capture())
        manifest = json.loads((output / "dogfood-package.json").read_text(encoding="utf-8"))
        self.assertEqual(sorted(manifest["binaries"]), sorted([*EXES, "grapt-desktop.exe"]))
        migrate = manifest["binaries"]["grapt-migrate.exe"]
        self.assertEqual(migrate["package"], "grapt-release")
        self.assertEqual(migrate["source_commit"], self.head)
        self.assertEqual(migrate["sha256"], hashlib.sha256(EXES["grapt-migrate.exe"]).hexdigest())
        self.assertEqual((output / "grapt-migrate.exe").read_bytes(), EXES["grapt-migrate.exe"])

    def test_a_governed_run_without_the_migration_binary_is_refused_by_the_adapter(self):
        (self.run_dir / "grapt-migrate.exe").unlink()
        with self.assertRaises(ProvenanceError) as caught:
            self.capture()
        self.assertIn("grapt-migrate.exe", str(caught.exception))
        self.assertFalse((self.base / "candidate-build.json").exists())

    def test_a_receipt_without_migration_evidence_is_refused(self):
        receipt = self.edit_receipt(self.capture(), lambda p: p["outputs"].pop("grapt-migrate.exe"))
        with self.assertRaises(ProvenanceError) as caught:
            self.assemble(receipt)
        self.assertIn("outputs.grapt-migrate.exe", str(caught.exception))

    def test_a_substituted_migration_binary_is_refused(self):
        receipt = self.capture()
        # Same length, different bytes: only the digest comparison can see it.
        substitute = EXES["grapt-migrate.exe"].replace(b"migration", b"MIGRATION")
        self.assertEqual(len(substitute), len(EXES["grapt-migrate.exe"]))
        (self.run_dir / "grapt-migrate.exe").write_bytes(substitute)
        with self.assertRaises(ProvenanceError) as caught:
            self.assemble(receipt)
        self.assertIn("grapt-migrate.exe", str(caught.exception))
        self.assertIn("says sha256", str(caught.exception))

    def test_migration_evidence_naming_another_binary_is_refused(self):
        def rename(payload):
            payload["outputs"]["grapt-migrate.exe"].update(package="grapt-cli", bin="grapt")
        receipt = self.edit_receipt(self.capture(), rename)
        with self.assertRaises(ProvenanceError) as caught:
            self.assemble(receipt)
        self.assertIn("describes grapt-migrate.exe as grapt-cli/grapt", str(caught.exception))

    def test_a_migration_binary_copied_from_another_packaged_binary_is_refused(self):
        (self.run_dir / "grapt-migrate.exe").write_bytes(EXES["grapt.exe"])
        receipt = self.capture()
        with self.assertRaises(ProvenanceError) as caught:
            self.assemble(receipt)
        self.assertIn("are the same bytes", str(caught.exception))

    def test_a_built_directory_missing_the_migration_binary_is_refused_before_any_copy(self):
        receipt = self.capture()
        (self.run_dir / "grapt-migrate.exe").unlink()
        with self.assertRaises(ValueError) as caught:
            self.assemble(receipt)
        self.assertIn("grapt-migrate.exe is missing", str(caught.exception))
        self.assertFalse((self.base / "package").exists())

    def test_PKG_MIG1_the_identity_check_is_what_refuses_substituted_evidence(self):
        """Named mutation PKG-MIG1: remove the new package/bin identity check.

        Red: with the check disabled, migration evidence naming the CLI's
        package and bin is packaged. Restored green: the unmutated assembler
        refuses the same receipt. The anchor must occur exactly once.
        """
        path = Path(__file__).with_name("assemble_candidate.py")
        source = path.read_text(encoding="utf-8")
        anchor = "if name in EXPECTED and attested_identity != EXPECTED[name]:"
        self.assertEqual(source.count(anchor), 1, "PKG-MIG1 anchor must be unique")
        mutant = load_module("assemble_pkg_mig1", path, source.replace(anchor, "if False:", 1))

        def rename(payload):
            payload["outputs"]["grapt-migrate.exe"].update(package="grapt-cli", bin="grapt")
        receipt = self.edit_receipt(self.capture(), rename)
        output = self.assemble(receipt, module=mutant)
        packaged = json.loads((output / "dogfood-package.json").read_text(encoding="utf-8"))
        self.assertEqual(packaged["binaries"]["grapt-migrate.exe"]["package"], "grapt-cli",
                         "PKG-MIG1 red: the mutant packaged evidence for another binary")
        shutil.rmtree(output)
        with self.assertRaises(ProvenanceError):
            self.assemble(receipt)


class CrossbuildProfiles(unittest.TestCase):
    """The governed entry point returns every required crossbuild artifact."""

    ENTRY_POINTS = {
        "daily-use-linux.json": ("daily-use-windows-build", "daily-use-migrate"),
    }

    def test_each_entry_point_requires_the_migration_artifact_within_its_budget(self):
        for profile_name, (command, migrate_id) in self.ENTRY_POINTS.items():
            with self.subTest(profile=profile_name):
                profile = json.loads((REPO / "remote-compute" / profile_name).read_text(encoding="utf-8"))
                entry = profile["commands"][command]
                self.assertEqual([step["argv"] for step in entry["steps"]],
                                 [["python3", "remote-compute/daily-use-windows-build.py"]])
                declared = {a["id"]: a for a in profile["evidence"]["artifacts"]}
                self.assertIn(migrate_id, entry["artifacts"])
                self.assertEqual(declared[migrate_id]["path"], "artifacts/grapt-migrate.exe")
                self.assertTrue(declared[migrate_id]["required"])
                needed = sum(declared[a]["max_bytes"] for a in entry["artifacts"])
                self.assertLessEqual(needed, profile["evidence"]["max_total_bytes"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
