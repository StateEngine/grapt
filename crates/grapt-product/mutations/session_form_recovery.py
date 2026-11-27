"""Restart handoff and stale-writer mutations; use the shared isolated runner."""
import json
from pathlib import Path

import run


def main():
    rows = json.loads(Path(__file__).with_suffix(".json").read_text(encoding="utf-8"))["mutations"]
    if [row["id"] for row in rows] != [f"SCFORMR{number:02}" for number in range(1, 5)]:
        run.fail("Session-form mutation inventory is not exact")
    if run.run(["git", "status", "--porcelain"]).stdout.strip():
        run.fail("Session-form mutations require a clean committed candidate")
    revision = run.run(["git", "rev-parse", "HEAD"])
    if revision.returncode:
        run.fail(revision.stderr)
    for row in rows:
        run.validate_cargo_row(row)
    run.run_rows("SCFORMR", revision.stdout.strip(), rows)


if __name__ == "__main__":
    main()
