#!/usr/bin/env bash
# Build the web UI into crates/web/dist (or $1). Needs the wasm32-unknown-unknown target and
# wasm-bindgen-cli matching the wasm-bindgen version in Cargo.lock.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
out="${1:-$here/dist}"
profile="${PROFILE:-wasm-release}"
cargo build --manifest-path "$here/Cargo.toml" --target wasm32-unknown-unknown --profile "$profile" -p duet-web
target_dir="$(cargo metadata --manifest-path "$here/Cargo.toml" --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
# Build next to the output and swap it in, so a running server never serves a half-written UI.
tmp="$out.new"
rm -rf "$tmp"
wasm-bindgen --target web --no-typescript --out-dir "$tmp" "$target_dir/wasm32-unknown-unknown/$profile/duet_web.wasm"
cp "$here"/static/* "$tmp"/
rm -rf "$out.old"
[ -d "$out" ] && mv "$out" "$out.old"
mv "$tmp" "$out"
rm -rf "$out.old"
