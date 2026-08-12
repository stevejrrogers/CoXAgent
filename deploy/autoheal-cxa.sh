#!/bin/bash
# Auto-heal watchdog for the CXA backend (Postgres 5433 + Redis 6379).
# If the containers die (Docker restart, crash, stop), bring the stack back up.
# Run detached:  nohup ./autoheal-cxa.sh &
LOGFILE=/Users/luton/CoXAgent/cxa/logs/autoheal.log
COMPOSE_DIR=/Users/luton/Projects/CoXAgent/deploy
mkdir -p "$(dirname "$LOGFILE")"
while true; do
  # Health check: can the hub's DB on 5433+6379 be reached (containers up)?
  UP=1
  docker exec cxa-backend-db-1 pg_isready -U coxagent -d coxagent >/dev/null 2>&1 || UP=0
  docker exec cxa-backend-redis-1 redis-cli -a "$(grep REDIS_PASSWORD "$COMPOSE_DIR/.env" | head -1 | cut -d= -f2)" ping >/dev/null 2>&1 || UP=0
  if [ "$UP" -eq 0 ]; then
    echo "$(date '+%F %T') HEAL: cxa-backend DB/Redis down, restarting stack" >> "$LOGFILE"
    cd "$COMPOSE_DIR" && set -a && source .env && set +a && \
      docker compose -f docker-compose.cxa.yml up -d >> "$LOGFILE" 2>&1
    sleep 20
    docker exec cxa-backend-db-1 pg_isready -U coxagent -d coxagent >/dev/null 2>&1 \
      && echo "$(date '+%F %T') HEAL: DB healthy again" >> "$LOGFILE" \
      || echo "$(date '+%F %T') HEAL: DB still down after restart" >> "$LOGFILE"
  fi
  sleep 60
done
