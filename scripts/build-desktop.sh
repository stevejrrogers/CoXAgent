#!/usr/bin/env bash
# Build the CoXAgent desktop app for all three OSes and drop the artifacts in
# ./dist. GUI apps can't be cross-compiled between operating systems, so:
#   - macOS  : built natively here (Swift .app + .dmg).
#   - Linux  : built here in a Docker container (WebKitGTK).
#   - Windows: built on GitHub Actions (windows runner) and downloaded — a
#              Windows GUI can't be produced on macOS/Linux. Needs `gh`.
#
# Env: SKIP_WINDOWS=1 to skip the CI round-trip. REPO overrides the gh repo.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="$ROOT/dist"
REPO="${REPO:-stevejrrogers/CoXAgent}"
mkdir -p "$DIST"

echo "==> [1/3] macOS (.dmg) — native"
bash "$ROOT/scripts/build-macos-app.sh"
cp "$ROOT/desktop/build/CoXAgent.dmg" "$DIST/CoXAgent-macos.dmg"

echo "==> [2/3] Linux (.tar.gz) — Docker (WebKitGTK)"
if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  docker run --rm -v "$ROOT":/src -v "$DIST":/out -w /src rust:bookworm bash -c '
    set -e
    apt-get update -qq >/dev/null
    apt-get install -y -qq libwebkit2gtk-4.1-dev libgtk-3-dev pkg-config >/dev/null
    # Build into container-local target dirs so the host target/ is untouched.
    CARGO_TARGET_DIR=/tmp/hub  cargo build --release --locked -p coxagent-app
    CARGO_TARGET_DIR=/tmp/desk cargo build --release --manifest-path desktop/coxagent-desktop/Cargo.toml
    mkdir -p /tmp/pkg/CoXAgent
    cp /tmp/hub/release/coxagent            /tmp/pkg/CoXAgent/cox-server
    cp /tmp/desk/release/coxagent-desktop   /tmp/pkg/CoXAgent/CoXAgent
    tar czf /out/CoXAgent-linux-x64.tar.gz -C /tmp/pkg CoXAgent
  '
  echo "    -> $DIST/CoXAgent-linux-x64.tar.gz"
else
  echo "    !! Docker not available — skipping Linux build."
fi

echo "==> [3/3] Windows (.zip) — GitHub Actions"
if [ "${SKIP_WINDOWS:-0}" = "1" ]; then
  echo "    (skipped: SKIP_WINDOWS=1)"
elif command -v gh >/dev/null 2>&1; then
  echo "    Triggering the desktop workflow on $REPO (Windows can't build on this OS)…"
  gh workflow run desktop.yml --repo "$REPO"
  sleep 6
  RUN=$(gh run list --repo "$REPO" --workflow desktop.yml --limit 1 --json databaseId -q '.[0].databaseId')
  echo "    Waiting for run $RUN…"
  gh run watch "$RUN" --repo "$REPO" --exit-status >/dev/null 2>&1 || true
  gh run download "$RUN" --repo "$REPO" -n CoXAgent-windows-x64 --dir "$DIST" 2>/dev/null \
    && echo "    -> $DIST/CoXAgent-windows-x64.zip" \
    || echo "    !! Could not download Windows artifact (check the run on GitHub)."
else
  echo "    !! gh CLI not found — push a tag 'v*' to build Windows on CI, or install gh."
fi

echo "==> Done. Artifacts in $DIST:"
ls -lh "$DIST" 2>/dev/null | awk 'NR>1{print "    "$9"  "$5}'
