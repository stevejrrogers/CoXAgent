#!/bin/bash
# Install the CXA backend-stack autohealer (autoheal-cxa.sh) as a per-user
# KeepAlive LaunchAgent so it survives reboot/logout. It was previously started
# as a bare `nohup … &`, which dies on reboot — leaving the Postgres/Redis
# stack with no automated remedy exactly when a machine restart is most likely
# to have knocked it over.
#
# Like install-host-watchdog.sh, this COPIES the script (and the compose file it
# drives) to a stable ~/CoXAgent location and points the LaunchAgent there,
# rather than running it in place from the repo. The repo working tree is the
# hub's own review/hygiene checkout, which is moved to arbitrary refs without
# notice — running the live self-healer from it risks a stale or half-checked-
# out autohealer taking over at the worst moment. A copy under ~/CoXAgent is
# immune; re-run this installer to pick up a fixed script.
#
# The autohealer is a CONTINUOUS loop (unlike the 5-minute host-watchdog), so
# the plist uses KeepAlive (relaunch on exit) rather than StartInterval. It
# resolves its compose file / log relative to its own directory, which is why
# docker-compose.cxa.yml is copied alongside it; .env is optional (the script
# falls back to coordination.json when it is absent).
set -eu
SRCDIR="$(cd "$(dirname "$0")" && pwd)"
DESTDIR="$HOME/CoXAgent"
DEST="$DESTDIR/autoheal-cxa.sh"
PLIST="$HOME/Library/LaunchAgents/com.coxagent.autoheal.plist"
LOG="$DESTDIR/autoheal.log"
mkdir -p "$DESTDIR" "$HOME/Library/LaunchAgents"

# Copy the autohealer and the compose file it drives to the stable location.
cp "$SRCDIR/autoheal-cxa.sh" "$DEST" && chmod 700 "$DEST"
cp "$SRCDIR/docker-compose.cxa.yml" "$DESTDIR/docker-compose.cxa.yml"
# .env holds secrets and is gitignored, so it is not in a clean checkout; carry
# it over only if the operator keeps one next to the repo compose file.
if [ -f "$SRCDIR/.env" ]; then
  cp "$SRCDIR/.env" "$DESTDIR/.env" && chmod 600 "$DESTDIR/.env"
fi

cat > "$PLIST" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.coxagent.autoheal</string>
  <key>ProgramArguments</key><array><string>/bin/bash</string><string>$DEST</string></array>
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
echo "autoheal installed as a KeepAlive LaunchAgent (com.coxagent.autoheal)"
echo "  runs: $DEST (stable copy; log: $LOG)"
