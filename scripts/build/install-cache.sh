#!/usr/bin/env bash
set -euo pipefail
[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]] || { echo 'This installer supports Linux x86_64.' >&2; exit 1; }
version=0.18.0
asset=sccache-v$version-x86_64-unknown-linux-musl.tar.gz
url=https://github.com/mozilla/sccache/releases/download/v$version/$asset
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
curl --fail --location --silent --show-error "$url" -o "$work/$asset"
curl --fail --location --silent --show-error "$url.sha256" -o "$work/checksum"
(cd "$work"; sha256sum --check checksum)
tar -xzf "$work/$asset" -C "$work"
mkdir -p "$HOME/.local/bin"
install -m 755 "$work/${asset%.tar.gz}/sccache" "$HOME/.local/bin/sccache"
"$HOME/.local/bin/sccache" --version
