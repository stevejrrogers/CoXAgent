#!/usr/bin/env bash
# One-shot restore of a CoXAgent snapshot on a NEW machine: brings up the
# cox-infra backing services (Postgres/Redis/Mongo/MinIO) via docker compose,
# restores all data from a migrate-export tarball, and restores ~/CoXAgent
# configs. After this, install an agent CLI + gh, open the app, and the team
# resumes exactly where it stopped (state lives in Postgres).
#
# Usage:  scripts/migrate-import.sh cox-export-YYYYmmdd-HHMM.tar.gz
set -euo pipefail

TARBALL="${1:?usage: migrate-import.sh <cox-export-*.tar.gz>}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
command -v docker >/dev/null || { echo "!! install Docker first"; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
echo "==> Unpacking $TARBALL"
tar xzf "$TARBALL" -C "$WORK"
SNAP="$(find "$WORK" -maxdepth 1 -type d -name 'cox-export-*' | head -1)"
[ -n "$SNAP" ] || { echo "!! not a migrate-export tarball"; exit 1; }

echo "==> Creating volumes + starting cox-infra"
for v in cox_pgdata coxagent-minio coxagent_mongo; do
  docker volume inspect "$v" >/dev/null 2>&1 || docker volume create "$v" >/dev/null
done

# Pre-load MinIO data BEFORE its first start (volume must exist and be filled).
if [ -f "$SNAP/minio-data.tar.gz" ]; then
  echo "==> Restoring MinIO uploads into the volume"
  docker run --rm -v coxagent-minio:/data -v "$SNAP":/in alpine \
    sh -c "tar xzf /in/minio-data.tar.gz -C /data"
fi

docker compose -f "$ROOT/deploy/local-infra/docker-compose.yml" up -d

echo "==> Waiting for Postgres"
for _ in $(seq 1 30); do
  docker exec cox-pg pg_isready -U coxagent >/dev/null 2>&1 && break
  sleep 2
done
docker exec cox-pg pg_isready -U coxagent >/dev/null || { echo "!! Postgres never became ready"; exit 1; }

echo "==> Restoring Postgres (all project state, users, tickets, KV)"
docker exec -i cox-pg psql -U coxagent -d coxagent -q < "$SNAP/coxagent.sql"

if [ -f "$SNAP/mongo.archive" ]; then
  echo "==> Restoring Mongo (wiki docs)"
  docker exec -i cox-mongo mongorestore --archive --quiet --drop < "$SNAP/mongo.archive"
fi

if [ -f "$SNAP/coxagent-home.tar.gz" ]; then
  if [ -d "$HOME/CoXAgent" ]; then
    echo "==> ~/CoXAgent already exists — restoring alongside as ~/CoXAgent.imported (merge manually)"
    mkdir -p "$HOME/CoXAgent.imported"
    tar xzf "$SNAP/coxagent-home.tar.gz" -C "$HOME/CoXAgent.imported" --strip-components=1
  else
    echo "==> Restoring ~/CoXAgent configs"
    tar xzf "$SNAP/coxagent-home.tar.gz" -C "$HOME"
  fi
fi

cat <<'EOF'
==> DONE. Remaining manual steps (credentials never travel in an export):
  1. claude          # sign in the agent CLI (and/or opencode)
  2. gh auth login   # so agents can push/PR
  3. Re-clone project codebases into ~/CoXAgent/<project>/codebase
     (git clone <repo> ~/CoXAgent/<project>/codebase)
  4. Launch the CoXAgent app (or `coxagent hub …`) — state resumes from
     Postgres exactly where the old machine stopped.
EOF
