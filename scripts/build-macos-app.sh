#!/usr/bin/env bash
# Build CoXAgent.app + CoXAgent.dmg for macOS — a native WKWebView shell that
# boots the bundled `coxagent hub`. Ad-hoc signed (no Apple cert needed): the
# app runs locally; first launch may need right-click → Open (Gatekeeper).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUILD="$ROOT/desktop/build"
APP="$BUILD/CoXAgent.app"
ID="com.coxagent.desktop"

echo "==> Building release binary"
( cd "$ROOT" && cargo build --release --bin coxagent )

echo "==> Compiling the Swift shell"
rm -rf "$BUILD"; mkdir -p "$BUILD"
swiftc -O "$ROOT/desktop/CoXAgentApp.swift" -o "$BUILD/CoXAgent" \
  -framework Cocoa -framework WebKit

echo "==> Generating the app icon"
cat > "$BUILD/icongen.swift" <<'SWIFT'
import Cocoa
let size = 1024.0
let img = NSImage(size: NSSize(width: size, height: size))
img.lockFocus()
let ctx = NSGraphicsContext.current!.cgContext
let rect = CGRect(x: 96, y: 96, width: size-192, height: size-192)
ctx.addPath(CGPath(roundedRect: rect, cornerWidth: 190, cornerHeight: 190, transform: nil))
ctx.setFillColor(CGColor(red: 0.033, green: 0.569, blue: 0.698, alpha: 1)); ctx.fillPath()
let attrs: [NSAttributedString.Key: Any] = [.font: NSFont.systemFont(ofSize: 560, weight: .bold), .foregroundColor: NSColor.white]
let s = NSAttributedString(string: "C", attributes: attrs); let sz = s.size()
s.draw(at: NSPoint(x: (size-sz.width)/2, y: (size-sz.height)/2 - 20))
img.unlockFocus()
let bmp = NSBitmapImageRep(data: img.tiffRepresentation!)!
try! bmp.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
SWIFT
swiftc -O "$BUILD/icongen.swift" -o "$BUILD/icongen" -framework Cocoa
"$BUILD/icongen" "$BUILD/icon.png"

ICONSET="$BUILD/AppIcon.iconset"; mkdir -p "$ICONSET"
for s in 16 32 64 128 256 512; do
  sips -z $s $s "$BUILD/icon.png" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  d=$((s*2)); sips -z $d $d "$BUILD/icon.png" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$BUILD/AppIcon.icns"

echo "==> Assembling $APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BUILD/CoXAgent"               "$APP/Contents/MacOS/CoXAgent"
# Named cox-server so it does not collide case-insensitively with CoXAgent.
cp "$ROOT/target/release/coxagent" "$APP/Contents/MacOS/cox-server"
cp "$BUILD/AppIcon.icns"           "$APP/Contents/Resources/AppIcon.icns"
chmod +x "$APP/Contents/MacOS/CoXAgent" "$APP/Contents/MacOS/cox-server"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>CoXAgent</string>
  <key>CFBundleDisplayName</key><string>CoXAgent</string>
  <key>CFBundleExecutable</key><string>CoXAgent</string>
  <key>CFBundleIdentifier</key><string>$ID</string>
  <key>CFBundleVersion</key><string>2.9.9</string>
  <key>CFBundleShortVersionString</key><string>2.9.9</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSAppTransportSecurity</key><dict><key>NSAllowsLocalNetworking</key><true/></dict>
</dict></plist>
PLIST

echo "==> Ad-hoc signing (nested binary first, then the bundle)"
codesign --force --sign - "$APP/Contents/MacOS/cox-server"
codesign --force --sign - "$APP"

echo "==> Building DMG"
DMGDIR="$BUILD/dmg"; rm -rf "$DMGDIR"; mkdir -p "$DMGDIR"
cp -R "$APP" "$DMGDIR/"; ln -s /Applications "$DMGDIR/Applications"
rm -f "$BUILD/CoXAgent.dmg"
hdiutil create -volname "CoXAgent" -srcfolder "$DMGDIR" -ov -format UDZO "$BUILD/CoXAgent.dmg" >/dev/null

echo "==> Done:"
echo "    App: $APP"
echo "    DMG: $BUILD/CoXAgent.dmg"
