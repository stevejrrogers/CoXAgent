#!/usr/bin/env bash
# Run a coturn TURN/STUN server in Docker so WebRTC calls work across NATs
# (not just same-LAN). The app mints short-lived HMAC credentials against the
# shared secret (coturn `use-auth-secret`), so no static passwords are exposed.
#
#   scripts/turn.sh up      # start coturn
#   scripts/turn.sh down    # stop & remove it
#   scripts/turn.sh env     # print the COXAGENT_TURN_* env vars
#
# For calls over the public internet, run this on a host with a PUBLIC IP and
# set COXAGENT_TURN_URL to turn:<that-ip>:3478. On localhost it covers LAN.
set -euo pipefail

NAME=coxagent-turn
SECRET="${TURN_SECRET:-coxturn_dev_secret_change_me}"
HOST="${TURN_HOST:-127.0.0.1}"
# Relay port range (kept small so Docker Desktop can map it on macOS).
MINP="${TURN_MIN_PORT:-49160}"
MAXP="${TURN_MAX_PORT:-49200}"

env_block() {
  cat <<EOF
export COXAGENT_TURN_URL=turn:$HOST:3478
export COXAGENT_TURN_SECRET=$SECRET
export COXAGENT_TURN_TTL=3600
EOF
}

case "${1:-up}" in
  up)
    if docker ps -a --format '{{.Names}}' | grep -qx "$NAME"; then
      docker start "$NAME" >/dev/null
    else
      docker run -d --name "$NAME" \
        -p 3478:3478/udp -p 3478:3478/tcp \
        -p "$MINP-$MAXP:$MINP-$MAXP/udp" \
        coturn/coturn -n \
          --use-auth-secret --static-auth-secret="$SECRET" \
          --realm=coxagent --no-tls --no-dtls \
          --min-port="$MINP" --max-port="$MAXP" \
          --external-ip="$HOST" >/dev/null
    fi
    echo "==> coturn running on $HOST:3478 (udp+tcp). Point the app at it with:"
    echo
    env_block
    ;;
  down) docker rm -f "$NAME" >/dev/null 2>&1 || true; echo "==> coturn stopped." ;;
  env) env_block ;;
  *) echo "usage: $0 {up|down|env}"; exit 1 ;;
esac
