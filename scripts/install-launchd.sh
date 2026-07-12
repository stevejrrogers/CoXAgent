#!/usr/bin/env bash
# Install a launchd agent so `coxagent serve` runs 24/7 (auto-start on login,
# restart on crash). Usage: scripts/install-launchd.sh <workspace-dir>
# Remove: launchctl bootout gui/$(id -u)/com.coxagent.<name>
set -euo pipefail

WORKSPACE="${1:?usage: install-launchd.sh <workspace-dir>}"
WORKSPACE="$(cd "$WORKSPACE" && pwd -P)"
NAME="$(basename "$WORKSPACE")"
LABEL="com.coxagent.$NAME"
BIN="$(command -v coxagent || echo "$HOME/.cargo/bin/coxagent")"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"

mkdir -p "$HOME/Library/LaunchAgents" "$WORKSPACE/logs"
cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array>
    <string>$BIN</string>
    <string>--state-dir</string><string>$WORKSPACE/state</string>
    <string>serve</string>
    <string>--work-dir</string><string>$WORKSPACE/codebase</string>
  </array>
  <key>WorkingDirectory</key><string>$WORKSPACE</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>$WORKSPACE/logs/coxagent.out.log</string>
  <key>StandardErrorPath</key><string>$WORKSPACE/logs/coxagent.err.log</string>
</dict></plist>
EOF

launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$PLIST"
echo "Installed $LABEL -> $BIN serve (dashboard on http://localhost:4000)"
echo "Remove: launchctl bootout gui/$(id -u)/$LABEL && rm $PLIST"
