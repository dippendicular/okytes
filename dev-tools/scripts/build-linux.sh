#!/usr/bin/env bash
# One-click Linux release build (unsigned).
# Produces target/release/linux_release with .deb, .rpm, .AppImage, the
# self-checking .run installer, a portable tar.gz and standalone tools.
#
# Usage:  bash dev-tools/scripts/build-linux.sh [--skip-install]
# Needs:  Ubuntu/Debian build packages (see --help), Rust (rustup), Node.js 24, pnpm 11.
set -euo pipefail

if [[ "${1:-}" == "--help" ]]; then
  cat <<'EOF'
Install prerequisites once on Ubuntu/Debian:
  sudo apt-get install -y build-essential curl git file patch rpm libfuse2 \
    libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev patchelf
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  # Node.js 24 + pnpm 11:  corepack enable && corepack prepare pnpm@11.11.0 --activate
EOF
  exit 0
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
case "$(uname -m)" in
  x86_64) target=x86_64-unknown-linux-gnu ;;
  aarch64|arm64) target=aarch64-unknown-linux-gnu ;;
  *) echo "Unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac
release="target/$target/release"
out="target/release/linux_release"
step() { printf '\n\033[36m==> %s\033[0m\n' "$1"; }

step "Checking tools"
for tool in cargo node pnpm tar; do
  command -v "$tool" >/dev/null || { echo "$tool is required (see --help)" >&2; exit 1; }
done
if [[ "$(pwd)" == /mnt/* ]]; then
  echo "Warning: building on a Windows drive from WSL is slow. Copy the source to ~ for speed." >&2
fi
export UPDATE_SIGNING_ENABLED=false

step "Preparing vendored sources and web assets"
node dev-tools/scripts/prepare-vendor.mjs
[[ "${1:-}" == "--skip-install" ]] || pnpm --dir apps/desktop install --frozen-lockfile
pnpm --dir apps/desktop build:quick

step "Building companion programs (CLI, daemon, browser host)"
cargo build --release --locked --target "$target" -p okytes-cli -p okytes-daemon -p okytes-native-host
node packaging/companions.mjs "$release" "$target"
node packaging/configure.mjs linux .tools-release.json

step "Building the desktop app, .deb, .rpm and AppImage"
pnpm --dir apps/desktop exec tauri build --target "$target" \
  --config "$root/packaging/linux/tauri.json" --config "$root/.tools-release.json" -- --locked

step "Building server tools (MCP and cloud)"
cargo build --release --locked --target "$target" -p okytes-mcp -p okytes-cloud

step "Creating the portable archive and .run installer"
rm -rf portable
node packaging/stage.mjs "$release" portable "$target"
mkdir -p "$release/bundle/portable"
tar -czf "$release/bundle/portable/Okytes-portable.tar.gz" -C portable .
node packaging/linux/create-run.mjs "$release/bundle/portable/Okytes-portable.tar.gz" "$release/bundle/portable/Okytes.run"

step "Collecting into $out"
rm -rf "$out" && mkdir -p "$out"
cp "$release"/bundle/deb/*.deb "$release"/bundle/rpm/*.rpm "$release"/bundle/appimage/*.AppImage "$out"/
cp "$release/bundle/portable/Okytes.run" "$out/Okytes-$target.run"
cp "$release/bundle/portable/Okytes-portable.tar.gz" "$out/Okytes-portable-$target.tar.gz"
for tool in okytes-cli okytes-daemon okytes-mcp okytes-cloud; do cp "$release/$tool" "$out/"; done
rm -rf portable

ls -lh "$out"
printf '\033[32mDone. Packages are unsigned.\033[0m\n'
