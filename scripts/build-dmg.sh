#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${1:-$(/usr/bin/awk -F '"' '/^version = / { print $2; exit }' "$ROOT/Cargo.toml")}"
APP_DIR="${PANERU_APP_PATH:-$ROOT/.build/release/Paneru.app}"
STAGE_DIR="$ROOT/.build/dmg-root"
DIST_DIR="$ROOT/dist"
DMG_PATH="$DIST_DIR/Paneru-$VERSION.dmg"
SIGN_IDENTITY="${PANERU_SIGN_IDENTITY:--}"

if [[ ! -d "$APP_DIR" ]]; then
  echo "Paneru.app was not found at $APP_DIR. Run scripts/build-app.sh first." >&2
  exit 1
fi

/bin/rm -rf "$STAGE_DIR" "$DMG_PATH"
/bin/mkdir -p "$STAGE_DIR" "$DIST_DIR"
/usr/bin/ditto "$APP_DIR" "$STAGE_DIR/Paneru.app"
/bin/ln -s /Applications "$STAGE_DIR/Applications"

for attempt in 1 2 3; do
  /bin/rm -f "$DMG_PATH"
  if /usr/bin/hdiutil create \
    -volname Paneru \
    -srcfolder "$STAGE_DIR" \
    -ov \
    -format UDZO \
    -imagekey zlib-level=9 \
    "$DMG_PATH"; then
    break
  fi

  if [[ "$attempt" -eq 3 ]]; then
    echo "Unable to create DMG after $attempt attempts." >&2
    exit 1
  fi

  echo "hdiutil create failed on attempt $attempt; retrying..." >&2
  /bin/sleep "$((attempt * 2))"
done

if [[ "$SIGN_IDENTITY" != "-" ]]; then
  /usr/bin/codesign --force --timestamp --sign "$SIGN_IDENTITY" "$DMG_PATH"
fi
/usr/bin/hdiutil verify "$DMG_PATH"
/bin/rm -rf "$STAGE_DIR"
printf '%s\n' "$DMG_PATH"
