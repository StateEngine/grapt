"""D5 convergence witnesses; unrelated CLI mutation credits are unchanged."""
import importlib.util
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("product_mutations", ROOT / "crates/grapt-product/mutations/run.py")
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("Missing governed mutation runner")
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


def main():
    result = RUNNER.run(["git", "rev-parse", "HEAD"])
    if result.returncode:
        raise RuntimeError(result.stderr)
    revision = result.stdout.strip()
    manifest = json.loads((ROOT / "crates/grapt-cli/mutations/r4ca/manifest.json").read_text(encoding="utf-8"))
    wanted = {"R4CA10", "R4CA11", "R4CA23", "R4CA24"}
    rows = [dict(row) for row in manifest["mutations"] if row["id"] in wanted]
    if len(rows) != len(wanted) or {row["id"] for row in rows} != wanted:
        raise RuntimeError("Affected convergence inventory is incomplete")
    rows.append({
        "id": "DDC01", "file": "crates/grapt-cli/src/run.rs",
        "test_target": "public_seams", "test": "runner_reconciles_sparse_advisories_from_durable_truth",
        "old": "                                convergence.admit_advisory(page)?;",
        "new": "                                convergence.admit_page(page)?;",
    })
    for row in rows:
        row.update(package="grapt-cli", features=["loopback-testkit"],
                   test_target=row.get("test_target", "contracts"),
                   red=f"test {row['test']} ... FAILED")
    RUNNER.run_rows("D5 affected convergence", revision, rows)
    for relative in {row["file"] for row in rows}:
        original = RUNNER.git_blob(revision, relative)
        if original.returncode or original.stdout.replace(b"\r\n", b"\n") != (ROOT / relative).read_bytes().replace(b"\r\n", b"\n"):
            raise RuntimeError("Mutation residue: " + relative)


if __name__ == "__main__":
    main()
