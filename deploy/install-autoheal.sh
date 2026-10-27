#!/bin/bash
# Install the CXA backend-stack autohealer (autoheal-cxa.sh) as a per-user
# KeepAlive LaunchAgent so it survives reboot/logout. It was previously started
# as a bare `nohup … &`, which dies on reboot — leaving the Postgres/Redis
# stack with no automated remedy exactly when a machine restart is most likely
# to have knocked it over.
#
# Unlike the host-watchdog (self-contained, copied to ~/CoXAgent and run every
# 5 min), the autohealer is a CONTINUOUS loop and reads its compose file via
# its own DEPLOY_DIR (deploy/docker-compose.cxa.yml), so the plist points at
# the IN-REPO script path and uses KeepAlive (relaunch on exit) rather than
# StartInterval. Running from the repo means a `git pull` of a fixed
# autoheal-cxa.sh is picked up on the next relaunch — no copy to keep in sync.
set -eu
SRC="$(cd "$(dirname "$0")" && pwd)/autoheal-cxa.sh"
PLIST="$HOME/Library/LaunchAgents/com.coxagent.autoheal.plist"
LOG="$HOME/CoXAgent/autoheal.log"
mkdir -p "$HOME/CoXAgent" "$HOME/Library/LaunchAgents"
chmod +x "$SRC"
cat > "$PLIST" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.coxagent.autoheal</string>
  <key>ProgramArguments</key><array><string>/bin/bash</string><string>$SRC</string></array>
  <key>KeepAlive</key><true/>
  <key>RunAtLoad</key><true/>
  <key>StandardOutPath</key><string>$LOG</string>
  <key>StandardErrorPath</key><string>$LOG</string>
</dict></plist>
PL
chmod 600 "$PLIST"
# Make launchd the single owner: stop any bare nohup instance first, then
# (re)load the agent.
pkill -f 'autoheal-cxa.sh' 2>/dev/null || true
launchctl unload "$PLIST" 2>/dev/null || true
launchctl load "$PLIST"
echo "autoheal installed as a KeepAlive LaunchAgent (com.coxagent.autoheal; log: $LOG)"
