#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE_DIR="$ROOT/assets/icons"
ICONSET_DIR="$(mktemp -d)/AppIcon.iconset"

cleanup() {
  /bin/rm -rf "$(dirname "$ICONSET_DIR")"
}
trap cleanup EXIT

for tool in rsvg-convert iconutil; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "$tool is required to regenerate Paneru icon assets." >&2
    exit 1
  fi
done

/bin/mkdir -p "$ICONSET_DIR"

render_png() {
  local size="$1"
  local output="$2"
  rsvg-convert -w "$size" -h "$size" "$SOURCE_DIR/AppIcon.svg" -o "$output"
}

render_png 16 "$ICONSET_DIR/icon_16x16.png"
render_png 32 "$ICONSET_DIR/icon_16x16@2x.png"
render_png 32 "$ICONSET_DIR/icon_32x32.png"
render_png 64 "$ICONSET_DIR/icon_32x32@2x.png"
render_png 128 "$ICONSET_DIR/icon_128x128.png"
render_png 256 "$ICONSET_DIR/icon_128x128@2x.png"
render_png 256 "$ICONSET_DIR/icon_256x256.png"
render_png 512 "$ICONSET_DIR/icon_256x256@2x.png"
render_png 512 "$ICONSET_DIR/icon_512x512.png"
render_png 1024 "$ICONSET_DIR/icon_512x512@2x.png"

iconutil -c icns "$ICONSET_DIR" -o "$SOURCE_DIR/AppIcon.icns"

for state in Managed Unmanaged NoWindow; do
  rsvg-convert \
    -f pdf \
    "$SOURCE_DIR/Status${state}Template.svg" \
    -o "$SOURCE_DIR/Status${state}Template.pdf"
done

echo "Generated AppIcon.icns and menu bar template PDFs in $SOURCE_DIR"
