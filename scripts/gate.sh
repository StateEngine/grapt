#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"
source scripts/build-cache.sh
if [[ -z ${CARGO_TARGET_DIR:-} ]]; then
    echo 'Gate needs a target: unset CARGO_TARGET_DIR so build-cache.sh chooses one, then rerun.' >&2
    exit 1
fi
if [[ ${SECODE_RAM_TARGET_RUN:-} && $(realpath -m -- "$CARGO_TARGET_DIR") == "$(realpath -m -- "$SECODE_RAM_TARGET_RUN")"/* ]]; then
    # A governed RAM gate owns its own target inside the reservation.
    exec python3 scripts/build/gate.py "$@"
fi
if [[ -z ${SECODE_WORKTREE_TARGET:-} ]]; then
    echo 'Gate refuses a target outside this checkout: unset CARGO_TARGET_DIR and rerun.' >&2
    exit 1
fi
if [[ $(realpath -m -- "$CARGO_TARGET_DIR") != $(realpath -m -- "$SECODE_WORKTREE_TARGET") ||
      $(realpath -m -- "$CARGO_TARGET_DIR") == $(realpath -m -- "$SECODE_TARGET_ROOT") ]]; then
    echo 'Gate refuses a target outside this checkout: unset CARGO_TARGET_DIR and rerun.' >&2
    exit 1
fi
exec python3 scripts/build/gate.py "$@"
