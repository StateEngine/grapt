#!/usr/bin/env bash
# Package the same clean source for Windows and Linux with the existing builder.
set -euo pipefail
if [[ $# == 2 ]]; then
    exec "$HOME/.local/lib/secode-release/heavy" -- bash "$0" --locked "$@"
fi
if [[ $# != 3 || $1 != --locked ]]; then
    echo 'usage: build_daily.sh NEW_OUTPUT PINNED_NODE_CACHE' >&2
    exit 2
fi
shift
daily_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
daily_output=$(realpath -m -- "$1")
daily_runtime=$(realpath -m -- "$2")
cd "$daily_root"
[[ ! -e $daily_output ]] || { echo 'output must be a new directory' >&2; exit 2; }
[[ -z $(git status --porcelain -- crates Cargo.toml Cargo.lock scripts/package scripts/release) ]] || {
    echo 'commit the package source before building' >&2
    exit 2
}

# Call only after the coordinator's quiet benchmark window has ended.
source scripts/build-cache.sh
mkdir -- "$daily_output"
daily_commit=$(git rev-parse HEAD)
daily_version=$(python3 -c 'import json; print(json.load(open("packages/plugin-bridge/runtime-pins.json"))["version"])')
for daily_target in x86_64-unknown-linux-gnu x86_64-pc-windows-msvc; do
    if [[ $daily_target == x86_64-unknown-linux-gnu ]]; then
        daily_node=$daily_runtime/node-$daily_version-linux-x64/bin/node
        daily_archive=$daily_runtime/node-$daily_version-linux-x64.tar.xz
        daily_license=$daily_runtime/node-$daily_version-linux-x64/LICENSE
    else
        daily_node=$daily_runtime/node-$daily_version-win-x64/node.exe
        daily_archive=$daily_runtime/node-$daily_version-win-x64.zip
        daily_license=$daily_runtime/node-$daily_version-win-x64/LICENSE
    fi
    [[ -f $daily_node && -f $daily_archive && -f $daily_license ]] || {
        echo "pinned Node assets missing for $daily_target under $daily_runtime" >&2
        exit 2
    }
    python3 scripts/release/build_artifacts.py --root "$daily_root" \
        --target "$daily_target" --jobs 4 --output "$daily_output/$daily_target-artifacts"
    [[ $(git rev-parse HEAD) == "$daily_commit" ]] || { echo 'source changed between target builds' >&2; exit 1; }
    python3 scripts/release/package.py --rust-root "$daily_root" --target "$daily_target" \
        --artifacts "$daily_output/$daily_target-artifacts" \
        --build-receipt "$daily_output/$daily_target-artifacts/build-receipt.json" \
        --output "$daily_output/$daily_target" --require-plugin-runtime \
        --plugin-runtime "$daily_node" --plugin-runtime-archive "$daily_archive" \
        --plugin-runtime-license "$daily_license"
    cp docs/daily-driver/JOURNEYS.md "$daily_output/$daily_target/JOURNEYS.md"
done
printf 'Common source %s packaged for Windows and Linux at %s\n' "$daily_commit" "$daily_output"
