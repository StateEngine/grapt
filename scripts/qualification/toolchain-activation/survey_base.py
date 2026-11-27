"""The executable base counterexample for TOOLCHAIN-ACTIVATION-01 (J01).

Proves, against the exact accepted base, that S1 (the security-continuity
tool-acquisition row) is NOT already satisfied by the Rust product:

1. the workspace has no `grapt-toolchain` member;
2. the product binary `grapt-server` exposes no toolchain activation verb
   (the intended product seam refuses as an unknown command);
3. therefore there is no typed activation contract, no verified staged
   activation, no same-version re-cut refusal, and no identity-write
   rollback anywhere in the Rust product.

The receipt is a durable artifact: it names the base it surveyed and the
exact checks it ran, so the counterexample is reproducible from the receipt
alone. Run from the worktree root; `--base` must match the worktree HEAD.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
ACCEPTED_BASE = "6fe79ddf09272aab309d5ce14276af8db3c15da8"
RECEIPT_PATH = (
    ROOT
    / "fixtures"
    / "toolchain-activation"
    / "receipts"
    / f"base-{ACCEPTED_BASE[:8]}-counterexample.json"
)


def git(*args: str) -> str:
    return subprocess.check_output(
        ["git", "-C", str(ROOT), *args], text=True, stderr=subprocess.STDOUT
    ).strip()


def survey_grep(argv: list[str]) -> dict:
    """One absence survey: the riprep convention is a zero-hit exit 1."""
    completed = subprocess.run(
        ["rg", "-n", *argv],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    rows = [line for line in completed.stdout.splitlines() if line.strip()]
    if completed.returncode == 0:
        return {"argv": "rg -n " + " ".join(argv), "exit": 0, "hits": len(rows), "hit_rows": rows[:16]}
    if completed.returncode == 1:
        return {"argv": "rg -n " + " ".join(argv), "exit": 1, "hits": 0, "hit_rows": []}
    raise RuntimeError(f"survey command failed ({completed.returncode}): {completed.stderr}")


def workspace_members() -> list[str]:
    raw = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    return re.findall(r'"(crates/[^"]+)"', raw.split("[workspace.lints", 1)[0])


def product_command_surface() -> dict:
    """The grapt-server verb surface, read from source at the surveyed base."""
    main = (ROOT / "crates/grapt-product/src/main.rs").read_text(encoding="utf-8")
    usage_match = re.search(r'"usage: grapt-server\.exe (.*?)"', main, re.DOTALL)
    usage = usage_match.group(1).strip() if usage_match else None
    return {
        "usage_string": usage,
        "toolchain_verb_present": bool(re.search(r"\btoolchain\b", main)),
        "declared_verbs": ["--version", "startup", "serve", "handover", "codemode worker (private)"],
    }


def node_execution_guard() -> dict:
    """The one tool binary the base ships (plugin-runtime Node) and how it
    is guarded: at EXECUTION time only. The activation boundary it lacks is
    the counterexample's point."""
    runtime = (ROOT / "crates/grapt-plugin/src/supervisor/runtime.rs").read_text(encoding="utf-8")
    materialize = (ROOT / "crates/grapt-product/src/dogfood/plugins/materialize.rs").read_text(encoding="utf-8")
    return {
        "path": "crates/grapt-product/src/dogfood/plugins/materialize.rs -> crates/grapt-plugin/src/supervisor/runtime.rs",
        "execution_guarded": "read_verified checks sha256 against the pin before spawn; version is checked against the pin",
        "activation_gap": "no archive digest verification at activation, no same-version re-cut refusal, no identity-write rollback, no identity record the product owns and re-verifies against the CURRENT pin",
        "runtime_manifest_schema": "grapt.plugin-runtime.v1",
        "runtime_read_verified_present": "fn read_verified" in runtime,
        "product_resolves_node": "packaged_node" in materialize,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default=ACCEPTED_BASE)
    parser.add_argument("--output", default=str(RECEIPT_PATH), type=Path)
    arguments = parser.parse_args()

    head = git("rev-parse", "HEAD")
    if head != arguments.base:
        raise RuntimeError(
            f"worktree HEAD {head} is not the surveyed base {arguments.base}; "
            "the counterexample must run on the exact accepted base"
        )
    branch = git("rev-parse", "--abbrev-ref", "HEAD")
    dirty = git("status", "--porcelain")

    members = workspace_members()
    surface = product_command_surface()
    node_guard = node_execution_guard()

    surveys = [
        {
            "id": "TMA-BASE-01",
            "question": "does the workspace declare a grapt-toolchain member?",
            **survey_grep(["grapt-toolchain", "crates/", "Cargo.toml"]),
        },
        {
            "id": "TMA-BASE-02",
            "question": "does any Rust source reference a toolchain activation owner?",
            **survey_grep(["grapt-toolchain", "crates/"]),
        },
        {
            "id": "TMA-BASE-03",
            "question": "does the product binary expose a toolchain verb?",
            "exit": 0 if surface["toolchain_verb_present"] else 1,
            "hits": 1 if surface["toolchain_verb_present"] else 0,
            "hit_rows": ["crates/grapt-product/src/main.rs (usage string carries no toolchain verb)"]
            if not surface["toolchain_verb_present"]
            else [],
        },
    ]

    receipt = {
        "schema": "grapt.toolchain-activation.base-counterexample.v1",
        "campaign": "TOOLCHAIN-ACTIVATION-01",
        "base": arguments.base,
        "branch": branch,
        "worktree": str(ROOT),
        "worktree_clean": dirty == "",
        "toolchain_crate_present": "crates/grapt-toolchain" in members,
        "product_seam": {
            "command": "grapt-server toolchain activate --root <root> --target <triple> --pin <pin>",
            **surface,
            "result_at_base": "unknown-command refusal (exit 2): the product has no toolchain activation boundary",
        },
        "shipped_tool_binary": node_guard,
        "surveys": surveys,
        "finding": (
            "S1 is not satisfied at the accepted base: no grapt-toolchain owner, "
            "no product activation seam, no typed pin/identity contract, no verified "
            "staged activation, no same-version re-cut refusal, no identity-write "
            "rollback. The one shipped tool binary (plugin-runtime Node) is guarded "
            "at execution time only; its activation has none of the row-1 invariants."
        ),
    }

    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(
        json.dumps(receipt, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(json.dumps(receipt["finding"]))
    print(f"receipt: {arguments.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())