#!/usr/bin/env bash
# Source once in an SE Code seat shell, naming its Rust checkout.
secode_env_helpers=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
secode_env_exports=$(python3 "$secode_env_helpers/build_environment.py" "${1:-$PWD}") || return 1
unset CARGO_TARGET_DIR SECODE_TARGET_ROOT SECODE_WORKTREE_TARGET SECODE_WORKTREE_ROOT
unset SECODE_WINDOWS_TARGET_DIR SCCACHE_BASEDIRS
eval "$secode_env_exports"
unset secode_env_helpers secode_env_exports
