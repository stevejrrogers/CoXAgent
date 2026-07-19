#!/usr/bin/env bash
# Export a complete, portable CoXAgent snapshot into one tarball:
#   Postgres (source of truth) + Mongo (wiki docs) + MinIO (uploads)
#   + ~/CoXAgent configs (per-project coxagent.json, project_context, hub.url)
# Codebases are NOT included — they live on the git forge; the import script
# re-clones them. Redis is ephemeral (TTL leases) and never exported.
#
# Usage:  scripts/migrate-export.sh [output-dir]     (default: ./cox-export)
# Result: cox-export-YYYYmmdd-HHMM.tar.gz — carry this + migrate-import.sh
#         to the new machine.
set -euo pipefail

OUT="${1:-./cox-export}"
STAMP="$(date +%Y%m%d-%H%M)"
WORK="$OUT/cox-export-$STAMP"
mkdir -p "$WORK"

need() { docker ps --format '{{.Names}}' | grep -qx "$1" || { echo "!! container $1 not running — start cox-infra first"; exit 1; }; }
need cox-pg

echo "==> Postgres dump (schema+data, clean-restore ready)"
docker exec cox-pg pg_dump -U coxagent --clean --if-exists coxagent > "$WORK/coxagent.sql"

if docker ps --format '{{.Names}}' | grep -qx cox-mongo; then
  echo "==> Mongo dump (wiki docs)"
  docker exec cox-mongo mongodump --archive --quiet > "$WORK/mongo.archive"
else
  echo "==> Mongo not running — skipped"
fi

if docker volume inspect coxagent-minio >/dev/null 2>&1; then
  echo "==> MinIO volume (file uploads)"
  docker run --rm -v coxagent-minio:/data -v "$(cd "$WORK" && pwd)":/out alpine \
    tar czf /out/minio-data.tar.gz -C /data .
else
  echo "==> MinIO volume not found — skipped"
fi

echo "==> ~/CoXAgent configs (excluding codebases/logs — they re-clone from git)"
if [ -d "$HOME/CoXAgent" ]; then
  tar czf "$WORK/coxagent-home.tar.gz" -C "$HOME" \
    --exclude='CoXAgent/*/codebase' --exclude='CoXAgent/*/logs' \
    --exclude='CoXAgent/**/.state.lock' CoXAgent
fi

cat > "$WORK/MANIFEST.txt" <<EOF
CoXAgent migration snapshot — $STAMP (host: $(hostname))
coxagent.sql        Postgres dump: all project state, tickets, users, auth, KV (workspace doc)
mongo.archive       Wiki/docs store (if present)
minio-data.tar.gz   Chat/ticket file uploads (if present)
coxagent-home.tar.gz  ~/CoXAgent configs: per-project coxagent.json, state/project_context.md, hub.url
Restore with: scripts/migrate-import.sh <this tarball>
NOT included (redo on the new machine): git clones of project codebases,
'claude' CLI login, 'gh auth login' — credentials never travel in the export.
EOF

TARBALL="$OUT/cox-export-$STAMP.tar.gz"
tar czf "$TARBALL" -C "$OUT" "cox-export-$STAMP"
rm -rf "$WORK"
echo "==> DONE: $TARBALL"
du -h "$TARBALL"
