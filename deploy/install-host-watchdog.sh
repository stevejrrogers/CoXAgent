#!/bin/bash
# Install the CoXAgent host watchdog as a per-user LaunchAgent (every 5 min).
set -eu
SRC="$(cd "$(dirname "$0")" && pwd)/host-watchdog.sh"
DEST="$HOME/CoXAgent/host-watchdog.sh"
PLIST="$HOME/Library/LaunchAgents/com.coxagent.watchdog.plist"
mkdir -p "$HOME/CoXAgent" "$HOME/Library/LaunchAgents"
cp "$SRC" "$DEST" && chmod 700 "$DEST"
cat > "$PLIST" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.coxagent.watchdog</string>
  <key>ProgramArguments</key><array><string>/bin/bash</string><string>$DEST</string></array>
  <key>StartInterval</key><integer>300</integer>
  <key>RunAtLoad</key><true/>
</dict></plist>
PL
chmod 600 "$PLIST"
launchctl unload "$PLIST" 2>/dev/null || true
launchctl load "$PLIST"
echo "watchdog installed: runs every 5 minutes (log: ~/CoXAgent/watchdog.log)"
