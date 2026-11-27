"""Resume the uncredited tool-vertical rows; preserve earlier receipt custody."""
import runpy
from pathlib import Path
r = runpy.run_path(str(Path(__file__).resolve().parents[1] / "crates/grapt-product/mutations/run.py"))
rows = r["load_rows"]("DTV")
assert [row["id"] for row in rows] == [f"DTV{i:02}" for i in range(1, 14)]
head = r["run"](["git", "rev-parse", "HEAD"])
assert head.returncode == 0, head.stderr
revision = head.stdout.strip()
r["run_rows"]("DTV10-13", revision, rows[9:])
print("residue backstop:", r["residue_backstop"](revision), "targets byte-identical")
print("Completed DGH/RMH/LH and DTV01-09 retain their separate receipts; no repeat or whole-family claim here")
