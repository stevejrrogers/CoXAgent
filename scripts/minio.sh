#!/usr/bin/env bash
# Run a local MinIO (S3-compatible) object store for CoXAgent file storage, and
# print the env vars to point the app at it. Files from chat + tickets are then
# stored in MinIO instead of local disk.
#
#   scripts/minio.sh up      # start MinIO (console at http://localhost:9001)
#   scripts/minio.sh down     # stop & remove it
#   scripts/minio.sh env      # print the COXAGENT_S3_* env vars
set -euo pipefail

NAME=coxagent-minio
USER_="${MINIO_USER:-coxagent}"
PASS="${MINIO_PASS:-coxagent123}"
PORT="${MINIO_PORT:-9000}"
CONSOLE="${MINIO_CONSOLE:-9001}"
BUCKET="${MINIO_BUCKET:-coxagent}"

env_block() {
  cat <<EOF
export COXAGENT_S3_ENDPOINT=http://127.0.0.1:$PORT
export COXAGENT_S3_BUCKET=$BUCKET
export COXAGENT_S3_REGION=us-east-1
export COXAGENT_S3_ACCESS_KEY=$USER_
export COXAGENT_S3_SECRET_KEY=$PASS
EOF
}

case "${1:-up}" in
  up)
    if docker ps -a --format '{{.Names}}' | grep -qx "$NAME"; then
      docker start "$NAME" >/dev/null
    else
      docker run -d --name "$NAME" \
        -p "$PORT:9000" -p "$CONSOLE:9001" \
        -e "MINIO_ROOT_USER=$USER_" -e "MINIO_ROOT_PASSWORD=$PASS" \
        -v coxagent-minio:/data \
        minio/minio server /data --console-address ":9001" >/dev/null
    fi
    echo "==> MinIO running: API http://localhost:$PORT · console http://localhost:$CONSOLE ($USER_/$PASS)"
    echo "    The app auto-creates the '$BUCKET' bucket on startup. Point it at MinIO with:"
    echo
    env_block
    ;;
  down) docker rm -f "$NAME" >/dev/null 2>&1 || true; echo "==> MinIO stopped." ;;
  env) env_block ;;
  *) echo "usage: $0 {up|down|env}"; exit 1 ;;
esac
