#!/usr/bin/env bash
# Own the scratch serve as well as the real TUI. Archived pane captures
# remain unchanged; this is their reusable valid/refusal journey with teardown.
set -euo pipefail
if [[ $# -lt 5 || $# -gt 6 ]]; then
    echo 'usage: mcp_pane.sh BIN_DIR SCRATCH_SERVICE REPOSITORY NEW_EVIDENCE valid|refusal [80|140]' >&2
    exit 2
fi
BIN=$1 SVC=$2 REPO=$3 EV=$4 MODE=$5 WIDTH=${6:-140}
[[ $MODE == valid || $MODE == refusal ]] || exit 2
[[ $WIDTH == 80 || $WIDTH == 140 ]] || exit 2
[[ -x $BIN/grapt-server && -x $BIN/grapt-tui && -d $SVC && -d $REPO ]] || exit 2
[[ ! -e $SVC/serve-ready.json ]] || { echo 'scratch service has a stale/live ready receipt' >&2; exit 2; }
mkdir -- "$EV" # Never mix captures from separate journeys.
source "$(dirname -- "${BASH_SOURCE[0]}")/evidence_serve.sh"
evidence_serve_start "$EV" "$BIN/grapt-server" "$SVC"

# Receipt publication and MCP boot are asynchronous. Readiness is bounded;
# failure here still invokes the very same EXIT cleanup as a later failure.
python3 - "$SVC" "$EV/serve-owner.json" <<'PY'
import json,sys,time
from pathlib import Path
service,owner=map(Path,sys.argv[1:]);pid=json.loads(owner.read_text())["pid"]
for _ in range(200):
    if not Path(f'/proc/{pid}').exists():
        raise SystemExit('owned serve exited before readiness; inspect serve.log')
    ready=service/"serve-ready.json"
    if ready.exists():
        try:
            value=json.loads(ready.read_text())
            if value.get("pid")==pid:
                break
        except json.JSONDecodeError:
            pass
    time.sleep(.1)
else:
    raise SystemExit('owned serve published no matching ready receipt within20s')
PY

EVIDENCE_TMUX_SESSION=mcp-pane-$$
tmux new-session -d -s "$EVIDENCE_TMUX_SESSION" -x "$WIDTH" -y 30 \
    "$(printf '%q ' "$BIN/grapt-tui" --service "$SVC" --directory "$REPO")"
cap() { tmux capture-pane -p -t "$EVIDENCE_TMUX_SESSION" > "$EV/$1"; }
sleep 2; cap phase1-startup.txt
# A witness-only fault: exits after the owned serve and real TUI are running.
[[ ${EVIDENCE_FAIL_AFTER_START:-0} != 1 ]] || { echo 'forced mid-journey failure' >&2; exit 23; }
tmux send-keys -t "$EVIDENCE_TMUX_SESSION" Enter; sleep 1.5; cap phase2-project.txt
tmux send-keys -t "$EVIDENCE_TMUX_SESSION" Enter; sleep 1.5; cap phase3-agent.txt
tmux send-keys -t "$EVIDENCE_TMUX_SESSION" Enter; sleep 1.5; cap phase4-model.txt
tmux send-keys -t "$EVIDENCE_TMUX_SESSION" Tab; sleep .5
tmux send-keys -t "$EVIDENCE_TMUX_SESSION" '/mcps'; sleep .5
tmux send-keys -t "$EVIDENCE_TMUX_SESSION" Enter; sleep 2
cap phase5-list.txt
if [[ $MODE == valid ]]; then
    for _ in {1..10}; do
        cap phase5-list.txt
        if rg -q '^> doomed  Failed' "$EV/phase5-list.txt" && \
           rg -q '^  journey  Connected' "$EV/phase5-list.txt"; then break; fi
        sleep 1
    done
    rg -q 'MCP SERVERS' "$EV/phase5-list.txt"
    rg -q '^> doomed  Failed' "$EV/phase5-list.txt"
    rg -q '^  journey  Connected' "$EV/phase5-list.txt"
    python3 "$EVIDENCE_SERVE_HELPER" inspect --pidfile "$EVIDENCE_SERVE_PIDFILE" \
        --output "$EV/live-processes.json"
    [[ ${EVIDENCE_FAIL_AFTER_MCP:-0} != 1 ]] || { echo 'forced mid-journey failure after MCP connected' >&2; exit 23; }
    [[ ${EVIDENCE_PAUSE_AFTER_MCP:-0} != 1 ]] || sleep 5
    tmux send-keys -t "$EVIDENCE_TMUX_SESSION" Enter; sleep 1.5
    cap phase6-detail.txt
    rg -q 'mcppane-fail-server' "$EV/phase6-detail.txt"
    rg -q 'os error 2' "$EV/phase6-detail.txt"
else
    rg -q 'grapt.jsonc' "$EV/phase5-list.txt"
    if [[ $WIDTH == 80 ]]; then rg -q '^  \.\.\.' "$EV/phase5-list.txt"; fi
fi
tmux send-keys -t "$EVIDENCE_TMUX_SESSION" C-q
echo 'MCP-PANE-WITNESS-PASS'
