"""Install the checksum-pinned corpus input without relying on a local checkout."""
import argparse
import hashlib
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]


def configuration():
    config = tomllib.loads((ROOT / "scripts/ci/fixture-input.toml").read_text(encoding="utf-8"))
    config["sha256"] = os.environ["CI_FIXTURE_SHA256"]
    pins = re.findall(r"^export SECODE_FIXTURE_COMMIT=([0-9a-f]{40})$",
                      (ROOT / "scripts/build-cache.sh").read_text(encoding="utf-8"), re.MULTILINE)
    if pins != [config["commit"]] or not re.fullmatch(r"[0-9a-f]{64}", config["sha256"]):
        raise ValueError("CI fixture input differs from the native gate pin")
    return config


def install(archive, destination, config):
    if archive.stat().st_size > 256 * 1024**2:
        raise ValueError("Frozen fixture archive exceeds its bound")
    if hashlib.sha256(archive.read_bytes()).hexdigest() != config["sha256"]:
        raise ValueError("Frozen fixture archive checksum differs")
    with tarfile.open(archive) as source:
        members = source.getmembers()
        if len(members) > 20000 or sum(m.size for m in members) > 256 * 1024**2:
            raise ValueError("Frozen fixture inventory exceeds its bound")
        for member in members:
            path = PurePosixPath(member.name)
            if (not (member.isfile() or member.isdir()) or path.is_absolute() or
                    ".." in path.parts or "\\" in member.name or ":" in member.name):
                raise ValueError("Frozen fixtures must be regular portable input files")
        destination.mkdir(parents=True, exist_ok=False)
        source.extractall(destination, members=members, filter="data")
    if (destination / ".source-commit").read_text(encoding="utf-8").strip() != config["commit"]:
        raise ValueError("Frozen fixture source marker differs")
    return {"SECODE_FIXTURES": str(destination.resolve()), "SECODE_FIXTURE_COMMIT": config["commit"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    config = configuration()
    destination = args.output or Path(os.environ["RUNNER_TEMP"]) / "ci-fixtures" / config["commit"]
    with tempfile.TemporaryDirectory() as raw:
        archive = args.archive
        if archive is None:
            name = "fixtures-" + config["commit"] + ".tar.gz"
            subprocess.run(["gh", "release", "download", "ci-fixtures-" + config["commit"],
                            "--repo", os.environ["GITHUB_REPOSITORY"], "--pattern", name, "--dir", raw], check=True)
            archive = Path(raw) / name
        environment = install(archive, destination, config)
    with open(os.environ["GITHUB_ENV"], "a", encoding="utf-8") as output:
        for key, value in environment.items():
            output.write(f"{key}={value}\n")
    print("Frozen corpus input installed from the native gate pin")


if __name__ == "__main__":
    main()
