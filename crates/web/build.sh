#!/usr/bin/env bash
# Build the web UI into crates/web/dist (or $1). Needs the wasm32-unknown-unknown target and
# wasm-bindgen-cli matching the wasm-bindgen version in Cargo.lock.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
out="${1:-$here/dist}"
profile="${PROFILE:-wasm-release}"
cargo build --manifest-path "$here/Cargo.toml" --target wasm32-unknown-unknown --profile "$profile" -p duet-web
target_dir="$(cargo metadata --manifest-path "$here/Cargo.toml" --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
rm -rf "$out"
wasm-bindgen --target web --no-typescript --out-dir "$out" "$target_dir/wasm32-unknown-unknown/$profile/duet_web.wasm"
cp "$here"/static/* "$out"/
