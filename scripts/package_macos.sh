#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BINARY_PATH=""
ARCH_LABEL="$(uname -m)"
VERSION=""
OUT_DIR="$ROOT_DIR/dist"
SIGN_IDENTITY=""

usage() {
  cat <<'EOF'
Usage: scripts/package_macos.sh [options]
  --binary <path>          Path to compiled Holossh binary (default: target/release/Holossh)
  --arch <label>           Architecture label for DMG filename (e.g. arm64, x86_64)
  --version <semver>       App version (default: parsed from Cargo.toml)
  --out-dir <dir>          Output directory (default: dist)
  --sign-identity <id>     Apple Developer ID Application signing identity (empty = ad-hoc)
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary) BINARY_PATH="$2"; shift 2 ;;
    --arch) ARCH_LABEL="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --out-dir) OUT_DIR="$2"; shift 2 ;;
    --sign-identity) SIGN_IDENTITY="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage; exit 1 ;;
  esac
done

if [[ -z "$BINARY_PATH" ]]; then
  BINARY_PATH="$ROOT_DIR/target/release/Holossh"
fi

if [[ ! -f "$BINARY_PATH" ]]; then
  echo "Error: compiled binary not found at $BINARY_PATH" >&2
  exit 1
fi

if [[ -z "$VERSION" ]]; then
  VERSION="$(awk -F'"' '/^version = / { print $2; exit }' "$ROOT_DIR/Cargo.toml")"
fi

if [[ -z "$VERSION" ]]; then
  echo "Error: could not determine version from Cargo.toml" >&2
  exit 1
fi

# Strip optional leading 'v' so CFBundleShortVersionString stays valid semver (e.g. 0.9.0)
CLEAN_VERSION="${VERSION#v}"

mkdir -p "$OUT_DIR"
STAGE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/holossh-pkg.XXXXXX")"
trap 'rm -rf "$STAGE_DIR"' EXIT

APP_DIR="$STAGE_DIR/Holossh.app"
DMG_NAME="Holossh-${CLEAN_VERSION}-macos-${ARCH_LABEL}.dmg"
DMG_PATH="$OUT_DIR/$DMG_NAME"

echo "==> Assembling Holossh.app (v${CLEAN_VERSION}, ${ARCH_LABEL})"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"

cp "$BINARY_PATH" "$APP_DIR/Contents/MacOS/Holossh"
chmod +x "$APP_DIR/Contents/MacOS/Holossh"

sed "s/__VERSION__/${CLEAN_VERSION}/g" "$ROOT_DIR/packaging/macos/Info.plist" > "$APP_DIR/Contents/Info.plist"
cp "$ROOT_DIR/assets/app-icon/AppIcon-Dark.icns" "$APP_DIR/Contents/Resources/AppIcon.icns"
printf 'APPL????' > "$APP_DIR/Contents/PkgInfo"

# Remove extended attributes (e.g. FinderInfo / quarantine) before codesigning
xattr -cr "$APP_DIR"

if [[ -n "$SIGN_IDENTITY" ]]; then
  echo "==> Signing Holossh.app with Developer ID (Hardened Runtime): $SIGN_IDENTITY"
  codesign \
    --force \
    --timestamp \
    --options runtime \
    --entitlements "$ROOT_DIR/packaging/macos/entitlements.plist" \
    --sign "$SIGN_IDENTITY" \
    "$APP_DIR/Contents/MacOS/Holossh"

  codesign \
    --force \
    --timestamp \
    --options runtime \
    --entitlements "$ROOT_DIR/packaging/macos/entitlements.plist" \
    --sign "$SIGN_IDENTITY" \
    "$APP_DIR"
else
  echo "==> Ad-hoc signing Holossh.app (no Developer ID identity supplied)"
  codesign --force --deep --sign - "$APP_DIR"
fi

echo "==> Verifying code signature"
codesign --verify --deep --strict --verbose=2 "$APP_DIR"

echo "==> Creating DMG: $DMG_PATH"
DMG_STAGE="$STAGE_DIR/dmg"
mkdir -p "$DMG_STAGE"
cp -R "$APP_DIR" "$DMG_STAGE/Holossh.app"
ln -s /Applications "$DMG_STAGE/Applications"

rm -f "$DMG_PATH"
hdiutil create \
  -volname "Holossh ${CLEAN_VERSION}" \
  -srcfolder "$DMG_STAGE" \
  -ov \
  -format UDZO \
  "$DMG_PATH"

if [[ -n "$SIGN_IDENTITY" ]]; then
  echo "==> Signing DMG with Developer ID"
  codesign --force --timestamp --sign "$SIGN_IDENTITY" "$DMG_PATH"
  codesign --verify --verbose=2 "$DMG_PATH"
fi

# Also keep a copy of the packaged .app in OUT_DIR for local inspection
rm -rf "$OUT_DIR/Holossh-${ARCH_LABEL}.app"
cp -R "$APP_DIR" "$OUT_DIR/Holossh-${ARCH_LABEL}.app"

echo "==> Done: $DMG_PATH ($(du -h "$DMG_PATH" | awk '{print $1}'))"
