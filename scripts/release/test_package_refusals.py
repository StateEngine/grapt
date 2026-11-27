"""Every refusal `scripts/release/package.py` claims, driven by a counterexample.

## Why this file is the point rather than a formality

`package.py` is built almost entirely out of refusals, and a refusal that has
never fired is a comment. A packager that cannot fail proves nothing about the
packages it produces: it would emit exactly the same confident manifest whether
its guards worked or not, because the happy path never touches them.

So each guard here is reached by CONSTRUCTING the state it exists to reject, and
the assertion is on the refusal's own message -- not merely that something
raised. A guard firing for the wrong reason is a guard that is not protecting
what its name says, and a bare `pytest.raises(PackageError)` cannot tell the
two apart.

## What the fixture is

A minimal but STRUCTURALLY REAL package input: an artifacts directory holding
one small file per shipped binary, and a build receipt that truthfully describes
those files -- correct digests, correct sizes, a recorded command claiming each
one, a clean tree, and this checkout's real HEAD as `source_commit`. The happy
case must pass first; otherwise a later refusal could be firing on the fixture's
own defect rather than on the mutation under test, and every assertion below it
would be measuring nothing.

Each test then applies EXACTLY ONE mutation to that valid input. That is what
makes the result attributable: the only difference between the accepted package
and the refused one is the fact the guard names.
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
import time
import zipfile
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))

import notices  # noqa: E402
import package  # noqa: E402
import shipped  # noqa: E402
import stateengine_payload as se_payload  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
TARGET = "x86_64-unknown-linux-gnu"


def head() -> str:
    return subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
    ).strip()


def build_fixture(
    tmp_path: Path, *, names: list[str] | None = None, target: str = TARGET
) -> tuple[Path, Path]:
    """A valid artifacts directory and build receipt. Returns (artifacts, receipt)."""
    names = shipped.names() if names is None else names
    artifacts = tmp_path / "artifacts"
    artifacts.mkdir()
    outputs = {}
    coordinates = shipped.cargo_coordinates()
    for name in names:
        payload = f"ELF-ish bytes for {name}\n".encode()
        (artifacts / name).write_bytes(payload)
        coordinate = name.removesuffix(".exe")
        package_name, binary = coordinates.get(coordinate, ("unknown-package", coordinate))
        outputs[name] = {
            "sha256": hashlib.sha256(payload).hexdigest(),
            "bytes": len(payload),
            "package": package_name,
            "bin": binary,
        }
    receipt = {
        "schema": "secode.release.build.v1",
        "source_commit": head(),
        "tree_dirty": False,
        "profile": "release",
        "target": target,
        "toolchain": "1.97.1",
        "commands": [{"argv": ["cargo", "build"], "produces": sorted(names)}],
        "outputs": outputs,
    }
    receipt_path = tmp_path / "build-receipt.json"
    receipt_path.write_text(json.dumps(receipt, indent=2), encoding="utf-8")
    return artifacts, receipt_path


def run_packager(
    tmp_path: Path,
    artifacts: Path,
    receipt: Path,
    output: str = "package",
    *,
    target: str = TARGET,
    publisher: str | None = None,
    stateengine: Path | None = None,
) -> int:
    sys.argv = [
        "package.py",
        "--rust-root", str(ROOT),
        "--artifacts", str(artifacts),
        "--build-receipt", str(receipt),
        "--output", str(tmp_path / output),
        "--target", target,
    ]
    if publisher is not None:
        sys.argv.extend(["--publisher", publisher])
    if stateengine is not None:
        sys.argv.extend(["--stateengine-payload", str(stateengine)])
    return package.main()


def mutate_receipt(receipt: Path, **changes: object) -> None:
    loaded = json.loads(receipt.read_text(encoding="utf-8"))
    for key, value in changes.items():
        if value is package.__class__:  # never; keeps the signature honest
            continue
        loaded[key] = value
    receipt.write_text(json.dumps(loaded, indent=2), encoding="utf-8")


def refusal(tmp_path: Path, artifacts: Path, receipt: Path, *, target: str = TARGET) -> str:
    with pytest.raises(package.PackageError) as raised:
        run_packager(tmp_path, artifacts, receipt, target=target)
    return str(raised.value)


def test_missing_dependency_register_refuses_before_copying(tmp_path: Path, monkeypatch) -> None:
    artifacts, receipt = build_fixture(tmp_path)
    monkeypatch.setattr(package.licenses, 'NOTICE', tmp_path / 'absent-notices.html')
    assert 'dependency license register is missing or stale' in refusal(tmp_path, artifacts, receipt)
    assert not (tmp_path / 'package' / 'release-manifest.json').exists()


def test_stale_dependency_register_refuses_before_copying(tmp_path: Path, monkeypatch) -> None:
    artifacts, receipt = build_fixture(tmp_path)
    stale = tmp_path / 'stale-notices.html'
    stale.write_text('<!-- Cargo.lock SHA-256: obsolete -->')
    monkeypatch.setattr(package.licenses, 'NOTICE', stale)
    assert 'Dependency licenses are stale' in refusal(tmp_path, artifacts, receipt)
    assert not (tmp_path / 'package' / 'release-manifest.json').exists()


# ---------------------------------------------------------------- the control


def test_a_valid_package_is_accepted(tmp_path: Path) -> None:
    """THE CONTROL, and it runs first for a reason.

    If the fixture could not be packaged, every refusal below might be firing on
    the fixture rather than on the mutation, and the whole battery would be
    green for the wrong reason.
    """
    artifacts, receipt = build_fixture(tmp_path)
    assert run_packager(tmp_path, artifacts, receipt) == 0

    manifest = json.loads((tmp_path / "package" / "release-manifest.json").read_text(encoding="utf-8"))
    assert manifest["schema"] == "secode.release.package.v1"
    assert manifest["entry_points"] == shipped.names()
    assert manifest["engine_version"] == head()
    # The disclosure the disposition requires must be IN the manifest, not
    # only in a document beside it.
    assert manifest["entry_point_roles"]["grapt-engined"]["role"] == "auxiliary"
    assert "not even /health" in manifest["entry_point_roles"]["grapt-engined"]["disclosure"]
    assert manifest["entry_point_roles"]["grapt-server"]["role"] == "product-server"
    assert manifest["excluded_binaries"] == shipped.EXCLUDED
    assert manifest["publisher"] is None

    # THE PACKAGED BINARIES MUST BE EXECUTABLE.
    #
    # `shutil.copyfile` copies bytes and not mode, so packaged binaries arrived
    # without the executable bit and the provider canary died with
    # `PermissionError` running the product it had just packaged. The operator
    # recipe had an instruction to restore the bit by hand, which was
    # documentation standing in for a defect. This is the counterexample that
    # keeps it fixed.
    # Asserted only where the bit EXISTS. Windows has no executable permission
    # and `chmod` there only toggles read-only, so this assertion would fail on
    # a host whose packages are fine -- and passing it by weakening it to
    # something Windows satisfies would stop it testing anything on Linux, which
    # is where the release artifacts are built and where the defect appeared.
    # The governed `release-packaging-checks` command runs this on the carrier.
    if os.name == "posix":
        for name in shipped.names():
            mode = (tmp_path / "package" / name).stat().st_mode
            assert mode & stat.S_IXUSR, f"{name} was packaged without the executable bit"
    # Unsigned is stated, never omitted and never softened.
    for state in manifest["signing"].values():
        assert state["signed"] is False
        assert state["reason"]


# ------------------------------------------------------------- the completeness


@pytest.mark.parametrize("target", list(shipped.SUFFIX))
def test_the_package_archive_includes_the_guide_and_license_texts(tmp_path: Path, target: str) -> None:
    artifacts, receipt = build_fixture(
        tmp_path, names=shipped.names(shipped.suffix_for(target)), target=target
    )
    assert run_packager(tmp_path, artifacts, receipt, target=target) == 0
    archive = shutil.make_archive(str(tmp_path / "release"), "zip", tmp_path / "package")
    with zipfile.ZipFile(archive) as package_archive:
        guide = package_archive.read("QUICKSTART.md")
        assert guide == (ROOT / "docs/release-packaging/QUICKSTART.md").read_bytes()
        assert package_archive.read("THIRD_PARTY_NOTICES.md") == notices.assemble(ROOT)
        for name in ("LICENSE-MIT", "LICENSE-APACHE"):
            assert package_archive.read(name) == (ROOT / name).read_bytes()
        for path in (ROOT / "licenses").glob("*/*"):
            assert package_archive.read(str(path.relative_to(ROOT))) == path.read_bytes()
        for binary in shipped.names(shipped.suffix_for(target)):
            assert binary in package_archive.namelist()
        manifest = json.loads(package_archive.read("release-manifest.json"))
        assert {"LICENSE-MIT", "LICENSE-APACHE"} <= set(
            manifest["assembly_provenance"]["observed_paths"]
        )


@pytest.mark.parametrize("name", ["LICENSE-MIT", "LICENSE-APACHE"])
def test_a_missing_project_license_refuses_before_copying(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, name: str
) -> None:
    artifacts, receipt = build_fixture(tmp_path)
    missing = ROOT / name
    is_file = Path.is_file
    monkeypatch.setattr(Path, "is_file", lambda path: path != missing and is_file(path))
    message = refusal(tmp_path, artifacts, receipt)
    assert str(missing) in message
    assert "absent" in message
    assert not (tmp_path / "package").exists()


def test_a_missing_shipped_binary_is_refused(tmp_path: Path) -> None:
    """The defect a record of true statements cannot catch on its own.

    Everything the receipt says about the four remaining binaries is correct.
    The package is simply missing one, and without an explicit completeness
    check that package would ship with a fully honest manifest.
    """
    artifacts, receipt = build_fixture(tmp_path, names=[n for n in shipped.names() if n != "grapt-migrate"])
    message = refusal(tmp_path, artifacts, receipt)
    assert "does not carry the declared shipped set" in message
    assert "missing ['grapt-migrate']" in message



@pytest.mark.parametrize("target", list(shipped.SUFFIX))
def test_a_missing_product_server_is_refused(tmp_path: Path, target: str) -> None:
    server = "grapt-server" + shipped.suffix_for(target)
    artifacts, receipt = build_fixture(
        tmp_path, names=[name for name in shipped.names(shipped.suffix_for(target)) if name != server],
        target=target,
    )
    assert f"missing ['{server}']" in refusal(tmp_path, artifacts, receipt, target=target)


def test_an_undeclared_extra_binary_is_refused(tmp_path: Path) -> None:
    """An extra is not a bonus: nobody declared it, and an operator is asked to trust it."""
    artifacts, receipt = build_fixture(tmp_path, names=[*shipped.names(), "cache-usage-benchmark"])
    message = refusal(tmp_path, artifacts, receipt)
    assert "undeclared ['cache-usage-benchmark']" in message


# ------------------------------------------------------------------- the custody


def test_compiled_recovery_script_is_inside_source_cleanliness_scope(tmp_path, monkeypatch):
    import build_artifacts
    root = tmp_path / "source"
    script = root / "scripts/windows/recover-legacy-sign-in.ps1"
    script.parent.mkdir(parents=True)
    script.write_text("# tracked recovery operator\n", encoding="utf-8")
    subprocess.run(["git", "init", "-q", str(root)], check=True)
    subprocess.run(["git", "-C", str(root), "add", "--", str(script)], check=True)
    subprocess.run(["git", "-C", str(root), "-c", "user.name=fixture", "-c",
                    "user.email=fixture@example.invalid", "commit", "-qm", "fixture",
                    "--", "scripts/windows/recover-legacy-sign-in.ps1"], check=True)
    monkeypatch.setattr(build_artifacts, "ROOT", root)
    before, clean = build_artifacts.custody()
    assert not clean
    script.write_text("# uncommitted credential behavior\n", encoding="utf-8")
    after, dirty = build_artifacts.custody()
    assert before == after and "scripts/windows/recover-legacy-sign-in.ps1" in dirty
    assert "scripts/windows/recover-legacy-sign-in.ps1" in package.ASSEMBLY_SCOPE


def test_a_dirty_build_tree_is_refused(tmp_path: Path) -> None:
    artifacts, receipt = build_fixture(tmp_path)
    mutate_receipt(receipt, tree_dirty=True)
    assert "does not attest a clean tree at build time" in refusal(tmp_path, artifacts, receipt)


def test_a_tree_dirty_that_is_merely_falsey_is_refused(tmp_path: Path) -> None:
    """`is not False`, deliberately.

    A receipt whose `tree_dirty` is `null`, `0` or `""` has not attested a clean
    tree -- it has failed to say anything -- and a truthiness test would read
    every one of those as clean.
    """
    for index, absent in enumerate((None, 0, "")):
        # Each case needs its OWN directory: they share a packager output path,
        # and `str("")` and `str(0)` do not even give distinct names.
        case = tmp_path / f"case{index}"
        case.mkdir()
        artifacts, receipt = build_fixture(case)
        mutate_receipt(receipt, tree_dirty=absent)
        message = refusal(case, artifacts, receipt)
        assert "does not attest a clean tree at build time" in message, absent


def test_a_wrong_profile_is_refused(tmp_path: Path) -> None:
    artifacts, receipt = build_fixture(tmp_path)
    mutate_receipt(receipt, profile="debug")
    assert "attests profile 'debug', not 'release'" in refusal(tmp_path, artifacts, receipt)


def test_a_wrong_target_is_refused(tmp_path: Path) -> None:
    artifacts, receipt = build_fixture(tmp_path)
    mutate_receipt(receipt, target="aarch64-apple-darwin")
    assert "attests target 'aarch64-apple-darwin'" in refusal(tmp_path, artifacts, receipt)


def test_a_malformed_source_commit_is_refused(tmp_path: Path) -> None:
    artifacts, receipt = build_fixture(tmp_path)
    mutate_receipt(receipt, source_commit="HEAD")
    assert "malformed source_commit" in refusal(tmp_path, artifacts, receipt)


def test_a_source_commit_this_checkout_cannot_describe_is_refused(tmp_path: Path) -> None:
    """Well-formed, and still not a commit here.

    40 hex characters is a shape, not a history. These binaries would belong to
    a history this checkout cannot describe, and a manifest that named it would
    be pointing at nothing.
    """
    artifacts, receipt = build_fixture(tmp_path)
    mutate_receipt(receipt, source_commit="0" * 40)
    message = refusal(tmp_path, artifacts, receipt)
    assert "is not a commit in" in message
    assert "a history this checkout cannot describe" in message


# ----------------------------------------------------------------- the provenance


def test_a_binary_no_command_claims_is_refused(tmp_path: Path) -> None:
    """A directory listing is not provenance."""
    artifacts, receipt = build_fixture(tmp_path)
    mutate_receipt(receipt, commands=[{"argv": ["cargo", "build"], "produces": ["grapt-server"]}])
    message = refusal(tmp_path, artifacts, receipt)
    assert "claims to have produced" in message
    assert "a directory listing is not provenance" in message


def test_a_command_with_no_produces_key_claims_nothing(tmp_path: Path) -> None:
    """`None` must not read as a match.

    `claims()` returns False for anything that is not a string or a list of
    strings. If a missing `produces` were treated as "produced everything", an
    unaccounted-for binary would pass through the one guard written to catch it.
    """
    artifacts, receipt = build_fixture(tmp_path)
    mutate_receipt(receipt, commands=[{"argv": ["cargo", "build"]}])
    assert "claims to have produced" in refusal(tmp_path, artifacts, receipt)


def test_commands_that_are_not_a_list_are_refused(tmp_path: Path) -> None:
    artifacts, receipt = build_fixture(tmp_path)
    mutate_receipt(receipt, commands={"argv": ["cargo", "build"]})
    assert "does not carry a list of build commands" in refusal(tmp_path, artifacts, receipt)


# --------------------------------------------------------------------- the bytes


def test_a_digest_mismatch_is_refused(tmp_path: Path) -> None:
    """The bytes on disk win over the bytes the receipt claims.

    This is the mutation that matters most: the receipt is internally consistent
    and the binary is simply not the one it describes.
    """
    artifacts, receipt = build_fixture(tmp_path)
    (artifacts / "grapt-tui").write_bytes(b"different bytes entirely\n")
    message = refusal(tmp_path, artifacts, receipt)
    assert "grapt-tui does not match what" in message
    assert "attests" in message


def test_a_size_mismatch_alone_is_refused(tmp_path: Path) -> None:
    """Both halves are checked, not just the digest."""
    artifacts, receipt = build_fixture(tmp_path)
    loaded = json.loads(receipt.read_text(encoding="utf-8"))
    loaded["outputs"]["grapt"]["bytes"] = 999_999
    receipt.write_text(json.dumps(loaded, indent=2), encoding="utf-8")
    assert "grapt does not match what" in refusal(tmp_path, artifacts, receipt)


def test_an_absent_artifact_is_refused(tmp_path: Path) -> None:
    artifacts, receipt = build_fixture(tmp_path)
    (artifacts / "grapt-migrate").unlink()
    assert "is absent" in refusal(tmp_path, artifacts, receipt)


def test_an_oversized_artifact_is_refused_before_any_copy(tmp_path: Path) -> None:
    """Measured BEFORE the copy: refusing after paying the disk write is waste."""
    artifacts, receipt = build_fixture(tmp_path)
    oversized = b"x" * (package.MAX_ARTIFACT_BYTES + 1)
    (artifacts / "grapt").write_bytes(oversized)
    message = refusal(tmp_path, artifacts, receipt)
    assert "over the" in message and "byte bound" in message
    assert not (tmp_path / "package").exists(), "the package directory was created before the bound was checked"


# ------------------------------------------------------------------ the receipt


def test_an_unknown_receipt_schema_is_refused(tmp_path: Path) -> None:
    artifacts, receipt = build_fixture(tmp_path)
    mutate_receipt(receipt, schema="grapt.something.else.v9")
    message = refusal(tmp_path, artifacts, receipt)
    assert "which this packager does not consume" in message


def test_a_receipt_that_is_not_json_is_refused(tmp_path: Path) -> None:
    artifacts, receipt = build_fixture(tmp_path)
    receipt.write_text("not json at all", encoding="utf-8")
    assert "could not be read as JSON" in refusal(tmp_path, artifacts, receipt)


def test_a_receipt_with_no_outputs_is_refused(tmp_path: Path) -> None:
    artifacts, receipt = build_fixture(tmp_path)
    mutate_receipt(receipt, outputs={})
    assert "carries no outputs" in refusal(tmp_path, artifacts, receipt)


# ------------------------------------------------------------------- the output


def test_an_existing_output_directory_is_refused(tmp_path: Path) -> None:
    """A package is assembled once.

    Reusing a directory would mean the manifest describes bytes another run left
    behind, which is the quietest way for a package to become a lie.
    """
    artifacts, receipt = build_fixture(tmp_path)
    (tmp_path / "package").mkdir()
    assert "already exists" in refusal(tmp_path, artifacts, receipt)


def test_notice_fragments_assemble_in_path_order_and_refuse_unusable_input(tmp_path: Path) -> None:
    (tmp_path / "notices/b").mkdir(parents=True)
    (tmp_path / "notices/b/2-x.md").write_bytes(b"two\r\n")
    (tmp_path / "notices/a.md").write_bytes(b"one\n")
    assert notices.assemble(tmp_path) == b"one\ntwo\r\n"
    (tmp_path / "notices/loose.md").write_bytes(b"no newline")
    with pytest.raises(ValueError, match="newline"):
        notices.assemble(tmp_path)
    (tmp_path / "notices/loose.md").unlink()
    (tmp_path / "notices/link.md").symlink_to(tmp_path / "notices/a.md")
    with pytest.raises(ValueError, match="regular"):
        notices.assemble(tmp_path)


def se_fixture(tmp_path, *, native=False, target=TARGET):
    payload = tmp_path / 'payload'
    payload.mkdir()
    bodies = {
        'se': b'#!/bin/sh\nexec "$(dirname "$0")/node" "$@"\n',
        'node': b'#!/bin/sh\nprintf "0.1.15\\n"\n',
        'LICENSE': b'Separately licensed fixture\n',
        'THIRD_PARTY_NOTICES.md': b'Fixture component notices\n',
        'licenses/Node-LICENSE.txt': b'Node fixture license\n',
        'engine/data.txt': b'fixture data\n',
    }
    if native:
        bodies.pop('node')
        bodies.pop('licenses/Node-LICENSE.txt')
        bodies['NOTICE.txt'] = bodies.pop('LICENSE')
        bodies.pop('THIRD_PARTY_NOTICES.md')
        bodies['se'] = b'#!/bin/sh\nprintf "0.1.15\\n"\n'
    if target.endswith('windows-msvc'):
        bodies['se.exe'] = bodies.pop('se')
    for name, body in bodies.items():
        path = payload / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(body)
        path.chmod(0o700 if name in {'se', 'node'} else 0o600)
    document = {
        'schema': 'stateengine.native-payload/v1' if native else 'stateengine.se-payload.v1',
        'name': 'stateengine', 'version': '0.1.15',
        'source_commit': 'a' * 40, 'target': target,
        'entry': 'se.exe' if target.endswith('windows-msvc') else 'se',
        'node': {'version': '24.15.0', 'source_url': 'https://nodejs.org/fixture.tar.xz',
                 'source_sha256': 'b' * 64, 'license_path': 'licenses/Node-LICENSE.txt'},
        'license_path': 'LICENSE', 'notices_path': 'THIRD_PARTY_NOTICES.md',
        'files': {name: {'sha256': hashlib.sha256(body).hexdigest(), 'bytes': len(body)}
                  for name, body in bodies.items()},
    }
    if native:
        document.pop('node')
        document['runtime'] = 'rust-embedded'
        document['license_path'] = document['notices_path'] = 'NOTICE.txt'
    se_store(payload, document)
    return payload, document


def se_store(payload, document):
    name = ('native-payload.json' if document.get('schema') == 'stateengine.native-payload/v1'
            else 'stateengine-payload.json')
    (payload / name).write_text(json.dumps(document))
    return {'manifest_sha256': hashlib.sha256((payload / name).read_bytes()).hexdigest(),
            'version': document['version'], 'source_commit': document['source_commit']}


def test_the_committed_pin_document_matches_the_schema_the_code_expects():
    # The suite's other pin fixtures are written with PIN_SCHEMA itself, so they
    # cannot notice the constant and the committed data file disagreeing. This
    # one reads the real document: a rename once rewrote the constant and left
    # packages/stateengine/ alone, and every assembly refused for a reason that
    # had nothing to do with the payload.
    committed = json.loads((ROOT/'packages/stateengine/payload-pins.json').read_text())
    assert committed['schema'] == se_payload.PIN_SCHEMA


@pytest.mark.parametrize('native', [False, True])
def test_stateengine_real_shape_and_separate_licenses(tmp_path, native):
    payload, document = se_fixture(tmp_path, native=native)
    pin = se_store(payload, document)
    assert se_payload.verify(payload, TARGET, pin)[1] == pin
    repository = tmp_path / 'repo'
    pins = repository / 'packages/stateengine/payload-pins.json'
    pins.parent.mkdir(parents=True)
    committed = json.loads((ROOT / 'packages/stateengine/payload-pins.json').read_text())
    committed['targets'] = {TARGET: pin}
    pins.write_text(json.dumps(committed))
    output = tmp_path / 'package'
    result = se_payload.assemble(repository, output, TARGET, payload)
    assert result['status'] == 'included'
    assert result['entry'] == 'stateengine/se'
    assert result['smoke']['exit'] == 0
    assert result['license'].startswith('separately licensed')
    license_path = document['license_path']
    assert (output / 'stateengine' / license_path).read_bytes() == (payload / license_path).read_bytes()
    manifest = 'native-payload.json' if native else 'stateengine-payload.json'
    assert set(result['assets']) == {'stateengine/' + p for p in document['files']} | {
        'stateengine/' + manifest}
    assert (output / 'stateengine' / manifest).read_bytes() == (payload / manifest).read_bytes()
    assert se_payload.assemble(repository, tmp_path / 'absent', TARGET, None)['status'] == 'not-included'


@pytest.mark.parametrize('target', [TARGET, 'x86_64-pc-windows-msvc'])
def test_stateengine_native_producer_contract_has_no_node(tmp_path, target):
    payload, document = se_fixture(tmp_path, native=True, target=target)
    pin = se_store(payload, document)
    assert se_payload.verify(payload, target, pin) == (document, pin)
    assert 'node' not in document and 'node' not in document['files']


@pytest.mark.parametrize('fault', ['schema', 'runtime', 'node', 'filename', 'ambiguous',
                                  'digest', 'extra', 'reserved'])
def test_stateengine_native_contract_refusals(tmp_path, fault):
    payload, document = se_fixture(tmp_path, native=True)
    pin = se_store(payload, document)
    manifest = payload / 'native-payload.json'
    if fault in {'schema', 'runtime', 'node'}:
        document[fault] = 'unsupported'
        manifest.write_text(json.dumps(document))
    elif fault == 'filename':
        manifest.rename(payload / 'stateengine-payload.json')
    elif fault == 'ambiguous':
        (payload / 'stateengine-payload.json').write_bytes(manifest.read_bytes())
    elif fault == 'digest':
        (payload / 'engine/data.txt').write_text('corrupt')
    elif fault == 'extra':
        (payload / 'unlisted').write_text('unlisted')
    else:
        document['files']['stateengine-payload.json'] = document['files'][document['license_path']]
        manifest.write_text(json.dumps(document))
    with pytest.raises(se_payload.PayloadError):
        se_payload.verify(payload, TARGET)
    with pytest.raises(se_payload.PayloadError):
        se_payload.verify(payload, TARGET, pin)


@pytest.mark.parametrize(('field', 'value', 'message'), [
    ('schema', 'wrong', 'identity'), ('name', 'another', 'identity'),
    ('target', 'another', 'identity'), ('source_commit', 'short', 'identity'),
    ('version', '../version', 'identity'), ('entry', '../entry', 'relative path'),
    ('entry', 'absent', 'absent from its inventory'),
    ('license_path', 'absent', 'absent from its inventory'),
    ('notices_path', 'absent', 'absent from its inventory'),
    ('files', [], 'count bound'),
])
def test_stateengine_manifest_refusals(tmp_path, field, value, message):
    payload, document = se_fixture(tmp_path)
    document[field] = value
    pin = se_store(payload, document)
    with pytest.raises(se_payload.PayloadError, match=message):
        se_payload.verify(payload, TARGET, pin)


@pytest.mark.parametrize('name', ['../escape', '/absolute', 'dir/../escape', 'dir\\escape',
                                  'dir//file', 'C:drive', './relative', 'control\nname'])
def test_stateengine_inventory_path_refusals(tmp_path, name):
    payload, document = se_fixture(tmp_path)
    document['files'][name] = document['files']['LICENSE']
    pin = se_store(payload, document)
    with pytest.raises(se_payload.PayloadError, match='relative path'):
        se_payload.verify(payload, TARGET, pin)


@pytest.mark.parametrize(('field', 'value'), [('version', 'not-a-version'),
                                            ('source_sha256', 'short'),
                                            ('source_url', 'http://nodejs.org/archive'),
                                            ('license_path', 'missing')])
def test_stateengine_node_provenance_refusals(tmp_path, field, value):
    payload, document = se_fixture(tmp_path)
    document['node'][field] = value
    pin = se_store(payload, document)
    with pytest.raises(se_payload.PayloadError, match='provenance|absent from its inventory'):
        se_payload.verify(payload, TARGET, pin)


def test_stateengine_pin_digest_size_and_unlisted_refusals(tmp_path):
    payload, document = se_fixture(tmp_path)
    pin = se_store(payload, document)
    with pytest.raises(se_payload.PayloadError, match='committed pin'):
        se_payload.verify(payload, TARGET, dict(pin, manifest_sha256='0' * 64))
    (payload / 'extra').write_text('unlisted')
    with pytest.raises(se_payload.PayloadError, match='unlisted'):
        se_payload.verify(payload, TARGET, pin)
    (payload / 'extra').unlink()
    (payload / 'engine/data.txt').write_text('changed')
    with pytest.raises(se_payload.PayloadError, match='digest or byte count'):
        se_payload.verify(payload, TARGET, pin)
    document['files']['node']['bytes'] += 1
    pin = se_store(payload, document)
    with pytest.raises(se_payload.PayloadError, match='digest or byte count'):
        se_payload.verify(payload, TARGET, pin)


@pytest.mark.parametrize(('bound', 'value', 'message'), [
    ('MAX_FILES', 1, 'count bound'), ('MAX_TOTAL', 1, 'total byte bound'),
    ('MAX_FILE', 1, 'attestation'), ('MAX_MANIFEST', 1, 'byte bound'),
    ('MAX_DEPTH', 1, 'relative path'),
])
def test_stateengine_admission_bounds(tmp_path, monkeypatch, bound, value, message):
    payload, document = se_fixture(tmp_path)
    pin = se_store(payload, document)
    monkeypatch.setattr(se_payload, bound, value)
    with pytest.raises(se_payload.PayloadError, match=message):
        se_payload.verify(payload, TARGET, pin)


def test_stateengine_links_nonregular_duplicate_and_missing_refusals(tmp_path):
    payload, document = se_fixture(tmp_path)
    pin = se_store(payload, document)
    alias = tmp_path / 'alias'
    alias.symlink_to(payload, target_is_directory=True)
    with pytest.raises(se_payload.PayloadError, match='real directory'):
        se_payload.verify(alias, TARGET, pin)
    (payload / 'extra').symlink_to(tmp_path)
    with pytest.raises(se_payload.PayloadError, match='symlink or reparse'):
        se_payload.verify(payload, TARGET, pin)
    (payload / 'extra').unlink()
    if hasattr(os, 'mkfifo'):
        os.mkfifo(payload / 'fifo')
        with pytest.raises(se_payload.PayloadError, match='nonregular'):
            se_payload.verify(payload, TARGET, pin)
        (payload / 'fifo').unlink()
    document['files']['license'] = document['files']['LICENSE']
    pin = se_store(payload, document)
    with pytest.raises(se_payload.PayloadError, match='duplicate file'):
        se_payload.verify(payload, TARGET, pin)
    document['files'].pop('license')
    document['files']['node']['bytes'] = True
    pin = se_store(payload, document)
    with pytest.raises(se_payload.PayloadError, match='attestation'):
        se_payload.verify(payload, TARGET, pin)
    (payload / se_payload.MANIFEST).write_text('{"schema":1,"schema":2}')
    with pytest.raises(se_payload.PayloadError, match='duplicate key'):
        se_payload.verify(payload, TARGET, pin)


def test_stateengine_smoke_refuses_ambient_node_and_writes(tmp_path):
    payload, document = se_fixture(tmp_path)
    (payload / 'se').write_text('#!/bin/sh\nexec node "$@"\n')
    with pytest.raises(se_payload.PayloadError, match='ambient Node'):
        se_payload.smoke(payload, document)
    (payload / 'se').write_text('#!/bin/sh\ntouch "$HOME/state"\nprintf "0.1.15\\n"\n')
    with pytest.raises(se_payload.PayloadError, match='wrote user state'):
        se_payload.smoke(payload, document)


@pytest.mark.parametrize('variable', ['LOCALAPPDATA', 'APPDATA', 'XDG_CACHE_HOME', 'TEMP', 'TMP'])
def test_stateengine_smoke_isolates_every_user_state_directory(tmp_path, variable):
    payload, document = se_fixture(tmp_path)
    (payload / 'se').write_text(
        '#!/bin/sh\nmkdir -p "$' + variable + '"\nprintf "0.1.15\\n"\n')
    with pytest.raises(se_payload.PayloadError, match='wrote user state'):
        se_payload.smoke(payload, document)


def test_stateengine_version_probe_never_inherits_seat_identity(tmp_path, monkeypatch):
    payload, document = se_fixture(tmp_path)
    names = ['STATEENGINE_AGENT_ID', 'stateengine_project_id', 'SECODE_STATEENGINE_URL',
             'secode_stateengine_token', 'SECODE_SESSION_ID', 'secode_session_id', 'node_options']
    for name in names:
        monkeypatch.setenv(name, 'synthetic caller identity')
    monkeypatch.setenv('GRAPT_PACKAGING_FIXTURE', 'preserved')
    async def intercepted(entry, environment, directory):
        assert all(name not in environment for name in names)
        assert environment['GRAPT_PACKAGING_FIXTURE'] == 'preserved'
        return 0, b'0.1.15\n'
    monkeypatch.setattr(se_payload, 'version_output', intercepted)
    se_payload.smoke(payload, document)


@pytest.mark.parametrize('contents', ['[]', '{', 'null'])
def test_stateengine_bad_json_is_not_a_manifest(tmp_path, contents):
    payload, _document = se_fixture(tmp_path)
    (payload / se_payload.MANIFEST).write_text(contents)
    with pytest.raises(se_payload.PayloadError, match='malformed|object'):
        se_payload.verify(payload, TARGET)


def test_stateengine_missing_and_linked_manifest_refuse(tmp_path):
    payload, _document = se_fixture(tmp_path)
    manifest = payload / se_payload.MANIFEST
    original = manifest.read_bytes()
    manifest.unlink()
    with pytest.raises(FileNotFoundError):
        se_payload.verify(payload, TARGET)
    outside = tmp_path / 'outside'
    outside.write_bytes(original)
    manifest.symlink_to(outside)
    with pytest.raises(se_payload.PayloadError, match='linked'):
        se_payload.verify(payload, TARGET)


@pytest.mark.parametrize('row', [None, {}, {'sha256': 'short', 'bytes': 1},
                               {'sha256': 'b' * 64, 'bytes': -1},
                               {'sha256': 'b' * 64, 'bytes': 1.0},
                               {'sha256': 'b' * 64, 'bytes': 1, 'extra': 0}])
def test_stateengine_malformed_attestation_refuses_before_copy(tmp_path, row):
    payload, document = se_fixture(tmp_path)
    document['files']['LICENSE'] = row
    se_store(payload, document)
    with pytest.raises(se_payload.PayloadError, match='attestation'):
        se_payload.verify(payload, TARGET)


def test_stateengine_reserved_manifest_and_missing_assets_refuse(tmp_path):
    payload, document = se_fixture(tmp_path)
    document['files'][se_payload.MANIFEST] = document['files']['LICENSE']
    se_store(payload, document)
    with pytest.raises(se_payload.PayloadError, match='reserved'):
        se_payload.verify(payload, TARGET)
    document['files'].pop(se_payload.MANIFEST)
    se_store(payload, document)
    (payload / 'node').unlink()
    with pytest.raises(se_payload.PayloadError, match='missing'):
        se_payload.verify(payload, TARGET)
    with pytest.raises(se_payload.PayloadError, match='unsupported'):
        se_payload.verify(payload, 'unsupported')


def test_stateengine_unlisted_directories_are_bounded(tmp_path, monkeypatch):
    payload, _document = se_fixture(tmp_path)
    for index in range(15):
        (payload / f'empty-{index}').mkdir()
    monkeypatch.setattr(se_payload, 'MAX_FILES', 8)
    with pytest.raises(se_payload.PayloadError, match='census'):
        se_payload.verify(payload, TARGET)


def test_stateengine_copy_refuses_replacement_and_never_follows_links(tmp_path):
    payload, document = se_fixture(tmp_path)
    source = payload / 'LICENSE'
    row = document['files']['LICENSE']
    (tmp_path / 'outside').write_bytes(source.read_bytes())
    source.unlink()
    source.symlink_to(tmp_path / 'outside')
    with pytest.raises(se_payload.PayloadError, match='linked'):
        se_payload.copy_regular(source, tmp_path / 'copy', row)
    assert not (tmp_path / 'copy').exists()
    source.unlink()
    source.write_bytes(b'wrong content')
    with pytest.raises(se_payload.PayloadError, match='attestation|grew'):
        se_payload.copy_regular(source, tmp_path / 'copy', row)


def test_stateengine_output_and_time_are_bounded(tmp_path, monkeypatch):
    payload, document = se_fixture(tmp_path)
    (payload / 'se').write_text('#!/bin/sh\nprintf "%05000d" 0\n')
    with pytest.raises(se_payload.PayloadError, match='output'):
        se_payload.smoke(payload, document)
    (payload / 'se').write_text('#!/bin/sh\nsleep 30\n')
    monkeypatch.setattr(se_payload, 'SMOKE_SECONDS', .05)
    with pytest.raises(se_payload.PayloadError, match='bound'):
        se_payload.smoke(payload, document)


def test_stateengine_exit_with_live_stdout_descendant_is_reaped(tmp_path, monkeypatch):
    payload, document = se_fixture(tmp_path)
    pid_path = tmp_path / 'child-pid'
    (payload / 'se').write_text(f'#!/bin/sh\nsleep 30 &\necho $! > "{pid_path}"\nexit 0\n')
    monkeypatch.setattr(se_payload, 'SMOKE_SECONDS', .05)
    with pytest.raises(se_payload.PayloadError, match='bound'):
        se_payload.smoke(payload, document)
    pid = int(pid_path.read_text())
    # A dead orphan can briefly remain a zombie until the host reaper joins it.
    status = Path(f'/proc/{pid}/stat')
    deadline = time.monotonic() + 1
    while True:
        try:
            state = status.read_text().split()[2]
        except (FileNotFoundError, ProcessLookupError):
            break  # The exact child exited before or during the observation.
        if state == 'Z':
            break
        assert time.monotonic() < deadline, 'owned descendant survived bounded teardown'
        time.sleep(.005)


def test_stateengine_requires_a_target_pin_before_copy(tmp_path):
    payload, _document = se_fixture(tmp_path)
    pins = tmp_path / 'repo/packages/stateengine/payload-pins.json'
    pins.parent.mkdir(parents=True)
    pins.write_text(json.dumps({'schema': se_payload.PIN_SCHEMA, 'targets': {}}))
    output = tmp_path / 'output'
    with pytest.raises(se_payload.PayloadError, match='pinned'):
        se_payload.assemble(tmp_path / 'repo', output, TARGET, payload)
    assert not output.exists()


@pytest.mark.parametrize('native', [False, True])
def test_stateengine_integrates_into_the_complete_archive_without_relicensing(tmp_path, monkeypatch, native):
    artifacts, receipt = build_fixture(tmp_path)
    payload, document = se_fixture(tmp_path, native=native)
    pin = se_store(payload, document)
    load = se_payload.load
    def fixture_pin(path):
        if path == ROOT / 'packages/stateengine/payload-pins.json':
            return {'schema': se_payload.PIN_SCHEMA, 'targets': {TARGET: pin}}, b''
        return load(path)
    monkeypatch.setattr(se_payload, 'load', fixture_pin)
    assert run_packager(tmp_path, artifacts, receipt, stateengine=payload) == 0
    output = tmp_path / 'package'
    manifest = json.loads((output / 'release-manifest.json').read_text())
    row = manifest['stateengine']
    assert row['status'] == 'included'
    assert row['source_commit'] == document['source_commit']
    assert row['manifest_sha256'] == pin['manifest_sha256']
    assert 'separately licensed' in row['license']
    archive = shutil.make_archive(str(tmp_path / 'release'), 'zip', output)
    with zipfile.ZipFile(archive) as bundled:
        license_path = document['license_path']
        assert bundled.read('stateengine/' + license_path) == (payload / license_path).read_bytes()
        assert bundled.read('LICENSE-MIT') == (ROOT / 'LICENSE-MIT').read_bytes()
        assert (
            b'StateEngine is a separate public project (currently\nAGPL-3.0). '
            b'Each bundled version is governed by the LICENSE and third-party\n'
            b'notices shipped with that version'
        ) in bundled.read('THIRD_PARTY_NOTICES.md')
        assert b'StateEngine component is excluded' in bundled.read('licenses/dependencies/NOTICE.html')
        assert all(name in bundled.namelist() for name in row['assets'])


def test_stateengine_packager_pin_refusal_precedes_any_payload_execution(tmp_path):
    artifacts, receipt = build_fixture(tmp_path)
    payload, _document = se_fixture(tmp_path)
    with pytest.raises(package.PackageError, match='StateEngine payload:.*committed pin'):
        run_packager(tmp_path, artifacts, receipt, stateengine=payload)


def test_stateengine_assembly_excludes_foreign_writers_even_with_a_permissive_umask(tmp_path, monkeypatch):
    artifacts, receipt = build_fixture(tmp_path)
    payload, document = se_fixture(tmp_path)
    pin = se_store(payload, document)
    (payload / 'se').chmod(0o777)
    load = se_payload.load
    def fixture_pin(path):
        if path == ROOT / 'packages/stateengine/payload-pins.json':
            return {'schema': se_payload.PIN_SCHEMA, 'targets': {TARGET: pin}}, b''
        return load(path)
    monkeypatch.setattr(se_payload, 'load', fixture_pin)
    previous = os.umask(0)
    try:
        assert run_packager(tmp_path, artifacts, receipt, stateengine=payload) == 0
    finally:
        os.umask(previous)
    output = tmp_path / 'package'
    assert stat.S_IMODE(output.stat().st_mode) == 0o700
    for name in ['grapt', 'stateengine/se', 'stateengine/node', 'stateengine/LICENSE']:
        assert (output / name).stat().st_mode & 0o022 == 0
