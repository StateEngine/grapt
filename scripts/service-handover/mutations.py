"""Run the handover allocation through the accepted product mutation runner."""
import importlib.util
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location(
    "product_mutations", ROOT / "crates/grapt-product/mutations/run.py"
)
if spec is None or spec.loader is None:
    raise SystemExit("SHV REFUSED: product mutation runner unavailable")
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)
runner.MANIFEST = ROOT / "crates/grapt-product/mutations/shv/manifest.json"
runner.EXACT_INVENTORIES = {"SHV": [f"SHV{number:02d}" for number in range(1, 18)]}
runner.RULED_ORDER = ("SHV",)
if sys.argv[1:] == ["--check"]:
    rows = runner.load_rows("SHV")
    for row in rows:
        edits = row.get("edits", [{key: row.get(key) for key in ("file", "old", "new")}])
        contents = {}
        for edit in edits:
            path = ROOT / edit["file"]
            source = contents.get(path, path.read_text(encoding="utf-8"))
            if source.count(edit["old"]) != 1 or edit["old"] == edit["new"]:
                raise SystemExit(f"SHV REFUSED: {row['id']} anchor is not exact once")
            contents[path] = source.replace(edit["old"], edit["new"], 1)
    print(f"SHV manifest: {len(rows)} exact rows, all anchors unique")
elif len(sys.argv) == 3 and sys.argv[1] == "--row":
    rows = runner.load_rows("SHV")
    selected = [row for row in rows if row["id"] == sys.argv[2]]
    if len(selected) != 1:
        raise SystemExit("SHV REFUSED: select one declared row")
    head = runner.run(["git", "rev-parse", "HEAD"])
    if head.returncode != 0:
        raise SystemExit(head.stderr)
    revision = head.stdout.strip()
    runner.run_rows("SHV-selected", revision, selected)
    checked = runner.residue_backstop(revision)
    print(f"selected-row evidence only; {checked} targets restored at {revision}")
else:
    runner.main()
