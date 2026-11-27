"""Generate the checked-in scripts/ci/test-times.json from a run's JUnit XMLs.

Offline and deterministic: input files are sorted by path; skipped
observations are excluded before deduplication, so a skipped zero can
never replace a real duration; among executed observations of one
identity the first occurrence in sorted file order wins (the parser's
within-file retry rule stays separate: in one file a later observation
wins). A binary whose total exceeds SPLIT_VOLUME_FRACTION of the known
suite total carries its per-test map; every other binary is aggregate-
only (total). Durations must be finite and nonnegative.
"""
import argparse
import json
import math
import sys
from pathlib import Path

import junit_report

# Data-volume threshold that decides the checked-in file's shape, not the
# gate's balance advisory: a binary above it carries its per-test map.
SPLIT_VOLUME_FRACTION = 0.10
SCHEMA = 1


def collect(files):
    """(table, counters) merged across junit files in sorted path order."""
    table = {}
    counters = {"observations": 0, "skipped": 0, "absent_time": 0,
                "malformed": 0, "duplicates": 0, "cross_file_duplicates": 0}
    for path in sorted(files, key=lambda item: str(item)):
        with open(path, "rb") as stream:
            raw = stream.read(junit_report.MAX_BYTES + 1)
        if len(raw) > junit_report.MAX_BYTES:
            raise ValueError(f"JUnit file exceeds its bound: {path}")
        shard_table, shard_counters = junit_report.parse_junit(raw)
        for counter, count in shard_counters.items():
            counters[counter] += count
        for key, seconds in shard_table.items():
            if key in table:
                counters["cross_file_duplicates"] += 1
                continue
            table[key] = seconds
    return table, counters


def build(files, source):
    """The checked-in timing document, or ValueError with no executed data."""
    if (not isinstance(source, str) or not source.strip()
            or len(source) > 128 or "\n" in source or "\r" in source):
        raise ValueError("The source label must be a short single line")
    table, counters = collect(files)
    if not table:
        raise ValueError("No executed observations in the input junit files")
    totals = {}
    tests = {}
    for (binary, name), seconds in table.items():
        totals[binary] = totals.get(binary, 0.0) + seconds
        tests.setdefault(binary, {})[name] = seconds
    grand = sum(totals.values())
    if not math.isfinite(grand) or grand <= 0:
        raise ValueError("The observed suite total is not a positive finite number")
    binaries = {}
    for binary in sorted(totals):
        entry = {"total": totals[binary]}
        if totals[binary] > SPLIT_VOLUME_FRACTION * grand:
            entry["tests"] = dict(sorted(tests[binary].items()))
        binaries[binary] = entry
    document = {"schema": SCHEMA, "source": source, "binaries": binaries}
    return document, counters


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("paths", nargs="+",
                        help="JUnit XML files or directories searched for them")
    parser.add_argument("--source", required=True,
                        help="short provenance label, e.g. hosted-run-123456")
    parser.add_argument("--output",
                        default=str(Path(__file__).resolve().parent / "test-times.json"))
    args = parser.parse_args()
    files = []
    for item in args.paths:
        path = Path(item)
        if path.is_dir():
            files.extend(path.rglob("*.xml"))
        else:
            files.append(path)
    files = sorted(set(files), key=lambda item: str(item))
    if not files:
        raise SystemExit("No junit files found")
    document, counters = build(files, args.source)
    text = json.dumps(document, sort_keys=True, indent=2) + "\n"
    Path(args.output).write_text(text, encoding="utf-8")
    print("Timing input: " + json.dumps(counters), flush=True)
    mapped = sum("tests" in entry for entry in document["binaries"].values())
    print(f"Binaries: {len(document['binaries'])} ({mapped} with a per-test map); "
          f"threshold: {SPLIT_VOLUME_FRACTION:.0%} of the suite total; "
          f"wrote {args.output}", flush=True)


if __name__ == "__main__":
    main()
