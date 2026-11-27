# MCP pane evidence driver

Use `mcp_pane.sh` for the Linux real-product pane captures. It owns the scratch
serve and its stdio MCP children as one private process group, records
`serve-owner.json`, and stops that group and the TUI tmux session on normal exit,
failed assertions, SIGINT and SIGTERM. Cleanup preserves the journey's exit code;
a cleanup failure makes an otherwise successful journey fail. The pidfile is
removed only after the owned group has stopped. SIGKILL cannot run an exit trap.

The three historical drivers under the `mcp-pane-20261010` evidence directory
remain archived evidence. They attach to an already-running serve and do not own
its cleanup. Use this driver for subsequent captures rather than starting a
separate serve for those scripts.

Supply a prepared **scratch** service, the binary directory containing
`grapt-server` and `grapt-tui`, a repository, and a new capture directory:

```bash
bash scripts/daily-use/mcp_pane.sh "$BIN" "$SCRATCH" "$REPO" "$NEW_EVIDENCE" valid 140
bash scripts/daily-use/mcp_pane.sh "$BIN" "$SCRATCH" "$REPO" "$NEW_EVIDENCE" refusal 80
```

The valid-config fixture names its local stdio server `journey` and its missing
executable server `doomed` (`/nonexistent/mcppane-fail-server`). The refusal
fixture carries an invalid `grapt.jsonc`. Do not copy a previous ready receipt or service
registration into a new scratch profile. A service with a ready receipt is
refused before launch. Captures, serve output and the connected process snapshot
are retained after cleanup.

The helper can also be sourced by another Bash journey. Source
`evidence_serve.sh`, call `evidence_serve_start EVIDENCE BINARY SCRATCH_SERVICE`,
and optionally set `EVIDENCE_TMUX_SESSION` to the session the driver created.
The installed traps are its single cleanup owner; do not replace them.

Run every real-product journey under `/home/example/secode-heavy.lock`, the same
explicit flock used by Cargo gates. Concurrent servers compete for the gate's
disk and can invalidate both runs.

Run the real lifecycle witness with explicit retained inputs:

```bash
~/.local/lib/secode-release/heavy --owner "$STATEENGINE_AGENT_ID" -- python3 scripts/daily-use/test_serve_teardown.py \
  --binary-dir "$BIN" --profile "$SCRATCH_TEMPLATE" --repository "$REPO" \
  --evidence "$NEW_EVIDENCE"
```

It copies the template without modifying it, redirects the MCP transcript into
each new service directory, and makes no LLM call. Normal completion, forced
failure after MCP connection, SIGINT and SIGTERM must leave no live group or
service-directory process and allow the observed port to be rebound. Removing
the EXIT trap must reproduce the orphan and fail that same check; the witness
then stops its own mutant group and repeats the restored failure path.
