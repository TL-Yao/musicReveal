#!/usr/bin/env bash
# 一条命令产出签名的 dist/musicReveal.app 与 dist/musicReveal_<版本>_aarch64.dmg。
# 依次：构建 ffmpeg（有缓存则复用）→ 生成 Rust 依赖许可 → 安装前端依赖（缺失时）
# → tauri build（含前端构建）
# → 自签名 → hdiutil 打包。可以重复运行。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP_DIR="$ROOT/app"
DIST="$ROOT/dist"
TRIPLE=aarch64-apple-darwin

[[ "$(uname -s)-$(uname -m)" == Darwin-arm64 ]] || { echo "只支持在 Apple Silicon Mac 上打包" >&2; exit 1; }

"$ROOT/scripts/build-ffmpeg.sh"
mkdir -p "$APP_DIR/src-tauri/binaries"
cp "$ROOT/build/ffmpeg/out/ffmpeg" "$APP_DIR/src-tauri/binaries/ffmpeg-$TRIPLE"

command -v cargo-about >/dev/null || { echo "缺少 cargo-about，请先运行：cargo install --locked cargo-about" >&2; exit 1; }
mkdir -p "$ROOT/build/licenses"
cargo about generate --locked -m "$ROOT/Cargo.toml" -c "$ROOT/scripts/about.toml" \
  "$ROOT/scripts/about.hbs" -o "$ROOT/build/licenses/rust-dependencies.md"

cd "$APP_DIR"
[[ -d node_modules ]] || npm ci
VERSION="$(node -p 'require("./src-tauri/tauri.conf.json").version')"
# sidecar 与许可文件只在打包时加入配置，日常 cargo build / CI 不需要 ffmpeg。
npx tauri build --target "$TRIPLE" --bundles app --config '{
  "bundle": {
    "externalBin": ["binaries/ffmpeg"],
    "resources": {
      "../../build/ffmpeg/out/licenses/": "licenses/",
      "../../LICENSE": "licenses/LICENSE",
      "../../THIRD_PARTY_NOTICES.md": "licenses/THIRD_PARTY_NOTICES.md",
      "../../build/licenses/rust-dependencies.md": "licenses/rust-dependencies.md"
    }
  }
}'

APP="$DIST/musicReveal.app"
DMG="$DIST/musicReveal_${VERSION}_aarch64.dmg"
rm -rf "$DIST"
mkdir -p "$DIST"
ditto "$ROOT/target/$TRIPLE/release/bundle/macos/musicReveal.app" "$APP"
"$ROOT/scripts/sign-local.sh" "$APP"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
ditto "$APP" "$STAGE/musicReveal.app"
ln -s /Applications "$STAGE/Applications"
hdiutil create -quiet -volname musicReveal -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG"

echo "产物："
ls -lh "$APP/Contents/MacOS" "$DMG"
