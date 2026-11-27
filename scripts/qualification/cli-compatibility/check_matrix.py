#!/usr/bin/env python3
"""Validate the CLI compatibility matrix (CLI-COMPATIBILITY-MATRIX-01 J01).

`docs/cli-compatibility/matrix.json` is the matrix; `MATRIX.md` is rendered from
it. The checker refuses:

- an oracle surface entry, or an extra oracle behaviour, that no row covers;
- a row naming an oracle entry that does not exist;
- a `gap` row, or any class outside compatible/implemented/redesigned/refused;
- a compatible, implemented or redesigned row without evidence, or whose
  evidence does not resolve: `test:<file>::<fn>` must name a function in that
  file, `receipt:<file>` must exist. A claim never rests on source text alone;
- a redesigned row without its reason;
- a refused row whose refusal surface is not in the CLI's refusal table, or is
  not exercised by the refusal witness;
- a MATRIX.md that differs from its rendering.

`--write` re-renders MATRIX.md. `--self-test` proves each refusal above fires.
"""

import argparse
import copy
import json
import pathlib
import re
import sys
import tempfile

MATRIX = "docs/cli-compatibility/matrix.json"
RENDERED = "docs/cli-compatibility/MATRIX.md"
REFUSALS = "crates/grapt-cli/src/compatibility.rs"
REFUSAL_WITNESS = (
    "crates/grapt-cli/tests/public_seams/compatibility.rs",
    "every_typescript_surface_outside_the_contract_is_refused_by_name",
)
CLASSES = ("compatible", "implemented", "redesigned", "refused")

HEADER = """# CLI compatibility matrix (H7)

Generated from `matrix.json` by `scripts/qualification/cli-compatibility/check_matrix.py --write`;
do not edit by hand. Oracle: the frozen TypeScript CLI at `{oracle}` (`{spec}`, git blob
`{spec_blob}`, sha256 of its committed bytes `{spec_sha}`). Classes:

{classes}

| Row | TypeScript | Class | Grapt | Evidence |
| - | - | - | - | - |
"""


def refusals_of(row):
    refusal = row.get("refusal")
    if refusal is None:
        return []
    return refusal if isinstance(refusal, list) else [refusal]


def cell(text):
    return text.replace("|", "\\|").replace("\n", " ")


def render(matrix, inventory):
    classes = "\n".join(f"- **{name}**: {text}" for name, text in matrix["classes"].items())
    lines = [
        HEADER.format(
            oracle=matrix["oracle"],
            spec=inventory["spec"],
            spec_blob=inventory["spec_blob"],
            spec_sha=inventory["spec_sha256"],
            classes=classes,
        )
    ]
    for row in matrix["rows"]:
        rust = row["rust"]
        if row.get("reason"):
            rust += f" (reason: {row['reason']})"
        if refusals_of(row):
            rust += " (refusal: " + ", ".join(f"`{surface}`" for surface in refusals_of(row)) + ")"
        evidence = "<br>".join(f"`{item}`" for item in row.get("evidence", []))
        lines.append(
            f"| {row['id']} | {cell(row['typescript'])} | {row['class']} | {cell(rust)} | {evidence} |\n"
        )
    return "".join(lines)


def resolve(repo, item):
    kind, _, target = item.partition(":")
    if kind == "test":
        path, _, function = target.partition("::")
        file = repo / path
        if not function or not file.is_file():
            return f"test file absent: {path}"
        if not re.search(rf"\bfn {re.escape(function)}\s*[<(]", file.read_text()):
            return f"test {function} is not defined in {path}"
        return None
    if kind == "receipt":
        return None if (repo / target).is_file() else f"receipt absent: {target}"
    return f"unknown evidence kind: {item}"


def check(repo, matrix, inventory, rendered):
    problems = []
    known = {entry["id"] for entry in inventory["surface"]} | set(matrix.get("extras", {}))
    covered = set()
    table = (repo / REFUSALS).read_text()
    witness_path, witness_fn = REFUSAL_WITNESS
    witness = (repo / witness_path).read_text()
    ids = [row["id"] for row in matrix["rows"]]
    for duplicate in sorted({i for i in ids if ids.count(i) > 1}):
        problems.append(f"{duplicate}: row id repeated")
    for row in matrix["rows"]:
        name = row["id"]
        for oracle in row["oracle"]:
            if oracle not in known:
                problems.append(f"{name}: names unknown oracle entry {oracle}")
            covered.add(oracle)
        klass = row["class"]
        if klass not in CLASSES:
            problems.append(f"{name}: class {klass!r} is not admitted at delivery")
            continue
        evidence = row.get("evidence", [])
        if klass != "refused" and not evidence:
            problems.append(f"{name}: a {klass} row has no evidence")
        for item in evidence:
            why = resolve(repo, item)
            if why:
                problems.append(f"{name}: {why}")
        if klass == "redesigned" and not row.get("reason"):
            problems.append(f"{name}: a redesigned row states no reason")
        if klass == "refused":
            if not refusals_of(row):
                problems.append(f"{name}: a refused row names no refusal")
            if f"test:{witness_path}::{witness_fn}" not in evidence:
                problems.append(f"{name}: a refused row does not cite the refusal witness")
            for surface in refusals_of(row):
                if f'surface: "{surface}"' not in table:
                    problems.append(f"{name}: refusal {surface!r} is not in {REFUSALS}")
                if f'"{surface}"' not in witness:
                    problems.append(f"{name}: refusal {surface!r} is not exercised by {witness_fn}")
    for missing in sorted(known - covered):
        problems.append(f"no row covers oracle entry {missing}")
    if rendered is not None and rendered != render(matrix, inventory):
        problems.append(f"{RENDERED} differs from its rendering; run with --write")
    return problems


def load(repo):
    matrix = json.loads((repo / MATRIX).read_text())
    inventory = json.loads((repo / matrix["inventory"]).read_text())
    return matrix, inventory


def self_test(repo):
    """Each refusal the checker promises must fire on a matrix built to trip it."""
    matrix, inventory = load(repo)
    cases = []

    def case(label, mutate, expect):
        broken = copy.deepcopy(matrix)
        mutate(broken)
        found = check(repo, broken, inventory, None)
        cases.append((label, any(expect in problem for problem in found), found))

    first_compatible = next(i for i, row in enumerate(matrix["rows"]) if row["class"] == "compatible")
    first_refused = next(i for i, row in enumerate(matrix["rows"]) if row["class"] == "refused")
    first_redesigned = next(i for i, row in enumerate(matrix["rows"]) if row["class"] == "redesigned")
    case("compatible without evidence",
         lambda m: m["rows"][first_compatible].pop("evidence"), "has no evidence")
    case("evidence naming an absent test",
         lambda m: m["rows"][first_compatible]["evidence"].__setitem__(
             0, "test:crates/grapt-cli/tests/public_seams/compatibility.rs::no_such_witness"),
         "is not defined")
    case("a gap at delivery", lambda m: m["rows"][first_compatible].__setitem__("class", "gap"),
         "is not admitted")
    case("an uncovered oracle entry", lambda m: m["rows"].pop(first_refused), "no row covers")
    case("a refusal the CLI does not carry",
         lambda m: m["rows"][first_refused].__setitem__("refusal", "made-up"), "is not in")
    case("a redesign without its reason",
         lambda m: m["rows"][first_redesigned].pop("reason"), "states no reason")
    case("an unknown oracle entry",
         lambda m: m["rows"][first_compatible]["oracle"].append("command:nothing"), "unknown oracle entry")
    clean = check(repo, matrix, inventory, None)
    ok = not clean and all(fired for _, fired, _ in cases)
    for label, fired, found in cases:
        print(f"{'FIRED' if fired else 'SILENT'}: {label}" + ("" if fired else f" {found}"))
    if clean:
        print("the committed matrix itself fails: " + "; ".join(clean))
    print("CHECKER SELF-TEST " + ("PASSED" if ok else "FAILED"))
    return 0 if ok else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=pathlib.Path, default=pathlib.Path(__file__).resolve().parents[3])
    parser.add_argument("--write", action="store_true", help="re-render MATRIX.md")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    repo = args.repo
    if args.self_test:
        return self_test(repo)
    matrix, inventory = load(repo)
    if args.write:
        (repo / RENDERED).write_text(render(matrix, inventory))
    rendered_path = repo / RENDERED
    rendered = rendered_path.read_text() if rendered_path.exists() else ""
    problems = check(repo, matrix, inventory, rendered)
    for problem in problems:
        print(f"MATRIX REFUSED: {problem}")
    rows = len(matrix["rows"])
    entries = len(inventory["surface"]) + len(matrix.get("extras", {}))
    if problems:
        print(f"MATRIX CHECK FAILED: {len(problems)} problem(s)")
        return 1
    print(f"MATRIX CHECK PASSED: {rows} rows cover all {entries} oracle entries; every claim resolves")
    return 0


if __name__ == "__main__":
    sys.exit(main())
