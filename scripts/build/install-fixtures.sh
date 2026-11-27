#!/usr/bin/env bash
# Export the pinned acceptance fixtures without walking or changing the controller tree.
set -euo pipefail
source "$(dirname -- "${BASH_SOURCE[0]}")/operator_environment.sh"
[[ $# -le 1 ]] || { echo 'Usage: install-fixtures.sh [local-oracle-clone]' >&2; exit 2; }
root=$(git rev-parse --show-toplevel)
source "$root/scripts/build-cache.sh"
cache=${SECODE_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/secode}
mkdir -p "$cache/fixtures"
work=$(mktemp -d "$cache/fixtures/.export-XXXXXX")
trap 'rm -rf -- "$work"' EXIT
# A supplied local oracle clone avoids SSH. Both paths use read-only Git archive.
if [[ $# == 1 ]]; then
    git -C "$1" archive "$SECODE_FIXTURE_COMMIT" fixtures/rust-reconstruction > "$work/fixtures.tar"
else
    oracle=${SECODE_WINDOWS_ORACLE_ROOT:?Set SECODE_WINDOWS_ORACLE_ROOT}
    oracle=${oracle//\\//}
    ssh "${SECODE_WINDOWS_HOST:?Set SECODE_WINDOWS_HOST}" "git -C \"$oracle\" archive $SECODE_FIXTURE_COMMIT fixtures/rust-reconstruction" > "$work/fixtures.tar"
fi
mkdir "$work/content"
tar -xf "$work/fixtures.tar" --strip-components=2 -C "$work/content"
# Re-running the setup checks the supplied snapshot rather than changing an active one.
destination=$cache/fixtures/$SECODE_FIXTURE_COMMIT
if [[ -d "$destination" ]]; then
    diff -rq -- "$work/content" "$destination"
else
    mv -- "$work/content" "$destination"
fi
echo "Pinned fixtures ready: $destination"
