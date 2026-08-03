#!/usr/bin/env bash
# Generate a fresh super-admin password into an (untracked) .env file.
#
# COX-B030: a real ADMIN_PASSWORD was committed in deploy/.env, so it is public
# to every clone and fork of this repo. Untracking the file stops the next leak
# but does nothing about the value already out there — a leaked credential is
# only neutralised by rotation. This script is that rotation, and the same one
# to use on any schedule or after anyone with the password leaves.
#
#   scripts/rotate-admin-password.sh                 # rewrites deploy/.env
#   scripts/rotate-admin-password.sh --env-file .env # rewrites another file
#   scripts/rotate-admin-password.sh --restart       # …and recreates the app
#
# Without --restart the running hub keeps the OLD password until someone
# recreates it: the hub reads COXAGENT_ADMIN_PASSWORD once, at boot.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ENV_FILE="$ROOT/deploy/.env"
RESTART=0

while [ $# -gt 0 ]; do
  case "$1" in
    --env-file) ENV_FILE="$2"; shift 2 ;;
    --restart)  RESTART=1; shift ;;
    -h|--help)  sed -n '2,14p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# Refuse to write a file git tracks — that would commit the new password on the
# next `git add -A`, recreating the exact bug this rotation exists to undo.
if git -C "$ROOT" ls-files --error-unmatch "$ENV_FILE" >/dev/null 2>&1; then
  echo "refusing to write $ENV_FILE: it is tracked in git." >&2
  echo "  git rm --cached '$ENV_FILE'   # then re-run" >&2
  exit 1
fi

if [ ! -f "$ENV_FILE" ]; then
  echo "no $ENV_FILE — copy deploy/.env.example to it and fill it in first." >&2
  exit 1
fi

# 32 characters from the OS CSPRNG, over an alphabet with no shell
# metacharacters: this value gets pasted into .env files, compose environments
# and CI secrets, and a quote or a `$` in it breaks them in confusing ways.
#
# `head` reads a fixed block and `tr` filters it — not the other way round. With
# `tr </dev/urandom | head -c 32`, `head` closes the pipe first, `tr` takes
# SIGPIPE, and under `pipefail` the script dies at 141 having rotated nothing.
RANDOM_ALNUM="$(head -c 1024 /dev/urandom | LC_ALL=C tr -dc 'A-Za-z0-9')"
NEW="${RANDOM_ALNUM:0:32}"
[ "${#NEW}" -eq 32 ] || { echo "could not generate a password" >&2; exit 1; }

# Rewrite in place, preserving mode, via a temp file in the same directory so
# the update is atomic and never leaves a half-written .env behind.
TMP="$(mktemp "$(dirname "$ENV_FILE")/.env.rotate.XXXXXX")"
trap 'rm -f "$TMP"' EXIT
chmod 600 "$TMP"
awk -v pw="$NEW" '
  /^[[:space:]]*ADMIN_PASSWORD=/ { print "ADMIN_PASSWORD=" pw; found = 1; next }
  { print }
  END { if (!found) exit 3 }
' "$ENV_FILE" > "$TMP" || {
  echo "no ADMIN_PASSWORD= line in $ENV_FILE — nothing rotated." >&2
  exit 1
}
mv "$TMP" "$ENV_FILE"
trap - EXIT
chmod 600 "$ENV_FILE"

echo "ADMIN_PASSWORD rotated in $ENV_FILE (mode 600)."
echo "New password: $NEW"
echo "Store it in your password manager now — the hub keeps only a hash."

if [ "$RESTART" -eq 1 ]; then
  COMPOSE_DIR="$(dirname "$ENV_FILE")"
  echo "==> Recreating the app service so it picks the new password up"
  ( cd "$COMPOSE_DIR" && docker compose up -d --force-recreate app )
  echo "Done. Old password no longer works."
else
  echo
  echo "The running hub still accepts the OLD password until it is recreated:"
  echo "  ( cd $(dirname "$ENV_FILE") && docker compose up -d --force-recreate app )"
fi
