#!/usr/bin/env bash
# CoXAgent docker janitor — reclaims the junk agent deploys accumulate without
# touching anything currently running.
#   ./scripts/docker-clean.sh          # safe clean (default)
#   ./scripts/docker-clean.sh --deep   # + builder cache & all dangling volumes
set -euo pipefail

echo "== Stopped containers from CoXAgent project deploys =="
# Compose projects named like preview/pr deploys or project codebases.
docker ps -a --filter status=exited --format '{{.ID}} {{.Names}} {{.Label "com.docker.compose.project"}}' \
  | awk 'NF>=2 {print $1}' | xargs -r docker rm >/dev/null && echo "removed" || true

echo "== Dangling images (untagged build layers) =="
docker image prune -f

echo "== Networks left behind by removed compose projects =="
docker network prune -f

if [[ "${1:-}" == "--deep" ]]; then
  echo "== Builder cache =="
  docker builder prune -af
  echo "== Dangling volumes (NOT named project volumes) =="
  docker volume prune -f
fi

echo "== Disk usage after clean =="
docker system df
