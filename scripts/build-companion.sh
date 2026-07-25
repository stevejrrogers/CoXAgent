#!/usr/bin/env bash
# Build the macOS menu-bar companion (SwiftUI) into a signed .app bundle.
# Menu-bar-only (LSUIElement): no Dock icon, lives in the status bar.
# Needs the full Xcode toolchain for SwiftUI; auto-selects it when
# xcode-select points at CommandLineTools.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PKG="$ROOT/desktop/CoXAgentCompanion"
OUT="$ROOT/desktop/build"
APP="$OUT/CoXAgent Companion.app"

if [[ "$(xcode-select -p)" == *CommandLineTools* && -d /Applications/Xcode.app ]]; then
  export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
fi

echo "==> Test"
swift test --package-path "$PKG"

echo "==> Build (release)"
swift build --package-path "$PKG" -c release

echo "==> Bundle"
BIN="$(swift build --package-path "$PKG" -c release --show-bin-path)/CompanionApp"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/CoXAgentCompanion"
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>CoXAgent Companion</string>
  <key>CFBundleIdentifier</key><string>dev.coxagent.companion</string>
  <key>CFBundleExecutable</key><string>CoXAgentCompanion</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>1.0.0</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
codesign -s - --force "$APP"
echo "==> Done: $APP"
