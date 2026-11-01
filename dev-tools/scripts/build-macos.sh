#!/usr/bin/env bash
# One-click macOS release build.
# Produces target/release/mac_release with the .dmg, the zipped Okytes.app and
# a tools archive (CLI, daemon, browser host, MCP and cloud servers).
#
# Usage:  bash dev-tools/scripts/build-macos.sh [--skip-install] [--universal]
# Needs:  Xcode Command Line Tools (xcode-select --install), Rust (rustup), Node.js 24, pnpm 11.
#
# Unsigned by default; Gatekeeper will block the first launch (right-click the
# app → Open). To sign and notarize, export APPLE_SIGNING_IDENTITY, APPLE_ID,
# APPLE_PASSWORD and APPLE_TEAM_ID before running; Tauri picks them up.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
skip_install=false
universal=false
for arg in "$@"; do
  case "$arg" in
    --skip-install) skip_install=true ;;
    --universal) universal=true ;;
    *) echo "Unknown option $arg" >&2; exit 1 ;;
  esac
done
case "$(uname -m)" in
  arm64) target=aarch64-apple-darwin ;;
  x86_64) target=x86_64-apple-darwin ;;
  *) echo "Unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac
$universal && target=universal-apple-darwin
out="target/release/mac_release"
step() { printf '\n\033[36m==> %s\033[0m\n' "$1"; }

step "Checking tools"
for tool in cargo node pnpm xcrun; do
  command -v "$tool" >/dev/null || { echo "$tool is required" >&2; exit 1; }
done
export UPDATE_SIGNING_ENABLED=false
if $universal; then
  rustup target add aarch64-apple-darwin x86_64-apple-darwin
  arches=(aarch64-apple-darwin x86_64-apple-darwin)
else
  rustup target add "$target"
  arches=("$target")
fi

step "Preparing vendored sources and web assets"
node dev-tools/scripts/prepare-vendor.mjs
$skip_install || pnpm --dir apps/desktop install --frozen-lockfile
pnpm --dir apps/desktop build:quick

step "Building companion programs (CLI, daemon, browser host, MCP, cloud)"
for arch in "${arches[@]}"; do
  cargo build --release --locked --target "$arch" \
    -p okytes-cli -p okytes-daemon -p okytes-native-host -p okytes-mcp -p okytes-cloud
done
if $universal; then
  # Tauri expects universal sidecars under the universal target name.
  mkdir -p target/universal-apple-darwin/release
  for tool in okytes-cli okytes-daemon okytes-native-host okytes-mcp okytes-cloud; do
    lipo -create -output "target/universal-apple-darwin/release/$tool" \
      "target/aarch64-apple-darwin/release/$tool" "target/x86_64-apple-darwin/release/$tool"
  done
fi
release="target/$target/release"
node packaging/companions.mjs "$release" "$target"
node packaging/configure.mjs macos .tools-release.json

step "Building Okytes.app and the .dmg"
pnpm --dir apps/desktop exec tauri build --target "$target" \
  --config "$root/packaging/macos/tauri.json" --config "$root/.tools-release.json" -- --locked

step "Collecting into $out"
rm -rf "$out" && mkdir -p "$out"
cp "$release"/bundle/dmg/*.dmg "$out"/
ditto -c -k --keepParent "$release/bundle/macos/Okytes.app" "$out/Okytes-$target.app.zip"
tar -czf "$out/Okytes-tools-$target.tar.gz" -C "$release" \
  okytes-cli okytes-daemon okytes-native-host okytes-mcp okytes-cloud

ls -lh "$out"
printf '\033[32mDone.\033[0m\n'
