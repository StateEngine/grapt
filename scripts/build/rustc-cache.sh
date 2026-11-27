#!/usr/bin/env bash
# Compatibility for old generated Cargo configs: ownership admission, then rustc.
set -euo pipefail
target=${CARGO_TARGET_DIR:-${SECODE_WORKTREE_TARGET:-}}
# A caller target outside the cache carries no managed-root claim, so nothing to
# admit; the ordinary command still runs.
if [[ -z ${SECODE_TARGET_ROOT:-} ]]; then
    exec "$@"
fi
# Nested Cargo (including trybuild) owns its target; never claim it as ours.
if [[ -n "${SECODE_WORKTREE_TARGET:-}" && "$target" == "$SECODE_WORKTREE_TARGET" ]]; then
    script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
    export CARGO_TARGET_DIR=$target
    exec python3 "$script_dir/evict_targets.py" --compiler "$@"
fi
exec "$@"
