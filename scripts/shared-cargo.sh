#!/usr/bin/env bash
# Ordinary Cargo commands use the same defaults as the delivery gate.
set -euo pipefail
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "$script_dir/build-cache.sh"
exec python3 "$script_dir/build/evict_targets.py" "$@"
