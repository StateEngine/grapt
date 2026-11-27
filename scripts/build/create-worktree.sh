#!/usr/bin/env bash
# New producer checkouts belong on NVMe; the caller names an accepted base.
set -euo pipefail
source "$(dirname -- "${BASH_SOURCE[0]}")/operator_environment.sh"
if [[ $# != 2 || ! $1 =~ ^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,95}$ || ! $2 =~ ^[0-9a-f]{40}$ ]]; then
    echo 'usage: create-worktree.sh NAME ACCEPTED_COMMIT (from the source Rust repository)' >&2
    exit 2
fi
source_root=$(git rev-parse --show-toplevel)
[[ -f $source_root/crates/grapt-types/Cargo.toml ]] || { echo 'Use an SE Code Rust repository.' >&2; exit 2; }
parent=${SECODE_WORKTREE_PARENT:-${SECODE_DATA_ROOT:-${XDG_DATA_HOME:-$HOME/.local/share}/secode}/worktrees}
[[ $parent == /* ]] || { echo 'Worktree parent must be absolute.' >&2; exit 2; }
mkdir -p -- "$parent"
destination=$parent/$1
[[ ! -e $destination && ! -L $destination ]] || { echo 'Preserve the existing destination.' >&2; exit 2; }
git cat-file -e "$2^{commit}"
git worktree add -b "$1" "$destination" "$2"
if ! (cd "$destination" && bash "$source_root/scripts/build/configure-worktree.sh"); then
    echo "Worktree preserved at $destination; correct its local build configuration before building." >&2
    exit 1
fi
printf '%s\n' "$destination"
