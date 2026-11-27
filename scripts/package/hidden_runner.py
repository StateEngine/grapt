"""Reinstall the same hidden policy before an owned child script imports tools."""
from pathlib import Path
import runpy
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import hidden_policy

if len(sys.argv) < 2 or not Path(sys.argv[1]).is_absolute():
    raise SystemExit('Hidden runner requires an absolute script')
hidden_policy.install()
script = sys.argv[1]
sys.argv = sys.argv[1:]
sys.path.insert(0, str(Path(script).parent))
runpy.run_path(script, run_name='__main__')
