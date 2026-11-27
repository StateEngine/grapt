#!/usr/bin/env bash
# Source this before starting a scratch service. One EXIT owner also handles
# INT/TERM and the optional TUI tmux session; preserve the journey's exit code.
EVIDENCE_SERVE_HELPER=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/serve_group.py
EVIDENCE_SERVE_PIDFILE=
EVIDENCE_TMUX_SESSION=

evidence_serve_cleanup() {
    local result=$?
    trap - EXIT INT TERM
    if [[ -n $EVIDENCE_TMUX_SESSION ]]; then
        tmux kill-session -t "$EVIDENCE_TMUX_SESSION" 2>/dev/null || true
    fi
    if [[ -n $EVIDENCE_SERVE_PIDFILE ]]; then
        if ! python3 "$EVIDENCE_SERVE_HELPER" stop --pidfile "$EVIDENCE_SERVE_PIDFILE" --owner "$$"; then
            [[ $result -ne 0 ]] || result=1
        fi
    fi
    exit "$result"
}

trap evidence_serve_cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

evidence_serve_start() {
    local evidence=$1 binary=$2 service=$3
    EVIDENCE_SERVE_PIDFILE=$evidence/serve-owner.json
    python3 "$EVIDENCE_SERVE_HELPER" start --binary "$binary" --service "$service" \
        --log "$evidence/serve.log" --pidfile "$EVIDENCE_SERVE_PIDFILE" --owner "$$"
}
