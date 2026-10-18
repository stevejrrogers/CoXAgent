#!/bin/sh
# Sourced port/identity guard for the e2e fixture servers (CXA-F315).
# This file only DEFINES functions — it runs inside run-server.sh /
# run-server-auth.sh and must never execute anything on source.
#
# The suite ports (4517/4518) are ours by contract: an interrupted Playwright
# run leaves its fixture server bound to them and the next run must evict it
# and boot clean. But eviction is ATTRIBUTION-CHECKED, never blind (CXA-B083's
# lesson, mirrored from crates/app/tests/deploy_smoke.rs decide_holder):
# classify the holder BEFORE evicting — only a process whose command proves it
# is this repo's own fixture binary (a coxagent build) is killed; any other
# holder fails the boot in seconds, naming pid and command, port untouched.
#
# Identity is proven separately because Playwright's webServer.url accepts ANY
# 2xx on /api/health: only the real hub serves /api/openapi.json with the
# title "CoXAgent Hub API" (crates/presentation/src/server/openapi.rs), and
# the auth suite additionally proves its throwaway account store by logging
# in with the bootstrap admin it just provisioned.

# Evict + await BOTH suite ports — the entry point for anything about to
# launch Playwright. This must happen BEFORE `playwright test`, not only
# inside run-server.sh: Playwright's webServer.url availability probe runs
# BEFORE the webServer command, so a healthy stale fixture on the port aborts
# the whole run with "already used" before the boot script is ever invoked
# (verified live). Attribution keeps this safe: only our own fixture servers
# die; foreign holders fail fast and the user's hub on 4000 is never touched.
clear_fixture_ports() {
  evict_stale_fixture 4517 || return 1
  await_port_free 4517 || return 1
  evict_stale_fixture 4518 || return 1
  await_port_free 4518 || return 1
}

# Kill only OUR stale fixture server holding the port; fail fast on a foreign
# one. TERM first, SIGKILL escalation only for a holder that ignores TERM, and
# every kill is preceded by the same attribution check. Only the LISTENER is
# classified: a mere client connection to the stale server (a stray browser
# tab on the fixture page) is not a squatter and must never trip the foreign
# fail-fast — it dies on its own once the listener does.
evict_stale_fixture() {
  _ef_port="$1"
  _ef_pids="$(lsof -tiTCP:"$_ef_port" -sTCP:LISTEN 2>/dev/null || true)"
  if [ -z "$_ef_pids" ]; then
    return 0
  fi
  for _ef_pid in $_ef_pids; do
    # -ww: never let ps truncate the command line we attribute with.
    _ef_cmd="$(ps -ww -p "$_ef_pid" -o command= 2>/dev/null || true)"
    # The holder vanished between the lsof snapshot and this ps: it can no
    # longer hold the port, so there is nothing to evict.
    if [ -z "$_ef_cmd" ]; then
      continue
    fi
    case "$_ef_cmd" in
      # The only thing run-server*.sh ever boots is target/debug/coxagent, so
      # that path IS the ownership proof (a sibling worktree's stale fixture
      # matches too — exactly the squatter we want gone). Anything that merely
      # MENTIONS coxagent (a cargo build, an editor) stays untouched.
      *target/debug/coxagent*)
        kill -TERM "$_ef_pid" 2>/dev/null || true
        ;;
      *)
        echo "e2e fixture port $_ef_port is held by a FOREIGN process:" >&2
        echo "  pid $_ef_pid: $_ef_cmd" >&2
        echo "refusing to kill it — stop that process or free the port, then retry" >&2
        exit 1
        ;;
    esac
  done
  # A killed process can take seconds to die: never boot into a doomed bind.
  if ! await_port_free "$_ef_port"; then
    # SIGKILL escalation for a holder that ignored TERM — still attributed.
    for _ef_pid in $(lsof -tiTCP:"$_ef_port" -sTCP:LISTEN 2>/dev/null || true); do
      _ef_cmd="$(ps -ww -p "$_ef_pid" -o command= 2>/dev/null || true)"
      if [ -z "$_ef_cmd" ]; then
        continue
      fi
      case "$_ef_cmd" in
        *target/debug/coxagent*) kill -KILL "$_ef_pid" 2>/dev/null || true ;;
      esac
    done
    await_port_free "$_ef_port" || return 1
  fi
  return 0
}

# Bounded wait for the port to actually release. The fixed `sleep 1` this
# replaces leaked the very "port already used" failure it guarded against
# whenever the stale server needed more than a second to exit.
await_port_free() {
  _ap_port="$1"
  _ap_tries=0
  # LISTEN-only, deliberately: a client with a lingering CLOSE_WAIT socket
  # (a browser tab left on the fixture page) must never read as "port still
  # bound" — only a listener can block the next bind.
  while lsof -tiTCP:"$_ap_port" -sTCP:LISTEN >/dev/null 2>&1; do
    _ap_tries=$((_ap_tries + 1))
    if [ "$_ap_tries" -ge 50 ]; then
      echo "port $_ap_port still bound after 10s — refusing to boot into a doomed bind" >&2
      return 1
    fi
    sleep 0.2
  done
  return 0
}

# Pick a free loopback port for THIS boot's metrics admin listener (CXA-B151):
# the hub binds that listener on 127.0.0.1:9010 by default, so a fixture
# booted beside a live hub silently loses its metrics endpoint (the hub
# continues without it by design — metrics_admin.rs's documented fail-open).
# Asking the kernel (bind port 0, read the assignment, release) is
# collision-free against the hub, the sibling suite, and anything else on the
# host — a hardcoded pin would just move the squatter flake to a new port.
# The socket is released before the fixture server binds, leaving a
# millisecond-scale rebind race we accept for a fixture boot. node is
# already a suite dependency (playwright), so no new tool is introduced.
# Callers must assign its result with a PLAIN assignment (`V="$(pick…)") so a
# failure aborts under set -e — `export V="$(pick…)"` masks it on some /bin/sh.
pick_free_loopback_port() {
  if ! command -v node >/dev/null 2>&1; then
    echo "pick_free_loopback_port: node is required to pick a free metrics port" >&2
    return 1
  fi
  # The port goes out as a STRING: node >= 24 honours FORCE_COLOR (which
  # Playwright's webServer sets on this script) and would otherwise emit the
  # number ANSI-wrapped, failing the numeric guard below on a healthy port.
  _pf_port="$(node -e 'const s=require("node:net").createServer();s.listen(0,"127.0.0.1",()=>{console.log(String(s.address().port));s.close()})')"
  case "$_pf_port" in
    ''|*[!0-9]*)
      echo "pick_free_loopback_port: node returned no usable port ($_pf_port)" >&2
      return 1
      ;;
  esac
  printf '%s\n' "$_pf_port"
}

# Prove the thing that just answered /api/health is THIS repo's hub before any
# spec talks to it. Bounded: on failure the boot dies here with the identity
# diagnostic instead of the suite failing deep inside its specs. The budget
# must cover the FULL server boot (this machine's observed boot is ~7s, so a
# loaded machine gets headroom), yet a server that DIES mid-boot must fail
# immediately — not burn the budget and then be misdiagnosed as an impostor.
await_identity() {
  _ai_port="$1"
  _ai_server_pid="$2"
  _ai_tries=0
  until _identity_matches "$_ai_port"; do
    _ai_tries=$((_ai_tries + 1))
    if ! kill -0 "$_ai_server_pid" 2>/dev/null; then
      echo "identity check failed: the fixture server (pid $_ai_server_pid) exited before it" >&2
      echo "could prove its identity on $_ai_port — see its stderr above for the boot failure" >&2
      return 1
    fi
    if [ "$_ai_tries" -ge 180 ]; then
      echo "identity check failed: the server on $_ai_port answers /api/health but never served" >&2
      echo "/api/openapi.json titled \"CoXAgent Hub API\" within 45s — an impostor or a broken" >&2
      echo "build is on the fixture port; refusing to run the suite against it" >&2
      return 1
    fi
    sleep 0.25
  done
  return 0
}

_identity_matches() {
  curl -fsS --max-time 2 "http://127.0.0.1:$1/api/openapi.json" 2>/dev/null | grep -q 'CoXAgent Hub API'
}

# Auth suite: identity proven twice — the API identity above AND a real login
# with the bootstrap admin this boot just provisioned. A 200 proves the
# throwaway account store (auth.json written beside this run's state dir), not
# leftover shared auth state a dev shell may have pointed the suite at.
await_auth_identity() {
  _aa_port="$1"
  _aa_server_pid="$2"
  _aa_user="$3"
  _aa_password="$4"
  await_identity "$_aa_port" "$_aa_server_pid" || return 1
  _aa_tries=0
  until _login_answers_200 "$_aa_port" "$_aa_user" "$_aa_password"; do
    _aa_tries=$((_aa_tries + 1))
    if ! kill -0 "$_aa_server_pid" 2>/dev/null; then
      echo "identity check failed: the fixture server (pid $_aa_server_pid) exited before login" >&2
      echo "could be proven on $_aa_port — see its stderr above for the boot failure" >&2
      return 1
    fi
    if [ "$_aa_tries" -ge 20 ]; then
      echo "identity check failed: the hub on $_aa_port is ours but login as $_aa_user never" >&2
      echo "succeeded within 5s — the fixture account store is not the throwaway one this" >&2
      echo "suite must run against; refusing to start the specs" >&2
      return 1
    fi
    sleep 0.25
  done
  return 0
}

# Suite credentials are bootstrap alphanumerics, so inline JSON is safe here.
_login_answers_200() {
  _lg_code="$(curl -sS --max-time 2 -o /dev/null -w '%{http_code}' \
    -X POST -H 'Content-Type: application/json' \
    -d "{\"username\":\"$2\",\"password\":\"$3\"}" \
    "http://127.0.0.1:$1/api/auth/login" 2>/dev/null || true)"
  [ "$_lg_code" = "200" ]
}
