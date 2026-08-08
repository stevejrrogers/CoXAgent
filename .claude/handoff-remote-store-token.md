# Handoff: remote-store token — READER + WRITER DONE

Session goal COMPLETE: agents "login once, everything works" — after web login,
all local operator processes reach shared state through the RBAC REST gateway
`/store` automatically (no hand-copying a bearer token). The login network-error
fix also landed earlier.

## User context (Vietnamese speaker)
- Wants zero-manual-step automation: after they log in, local client apps just
  read a token file. Their mental model is exactly: login writes a file, clients
  read it. That IS the right design.
- Chose: fixed anchor `$HOME/CoXAgent/operator.token` (+ env override
  `COXAGENT_TOKEN_FILE`) for BOTH writer and reader so they can never disagree.

## STATUS — committed & pushed to origin/main (HEAD = 068a0be)
All three feature commits are SAFE on origin/main:

1. **Fix login network error** — `~/CoXAgent/hub.url` was `http://localhost`
   (port 80, nothing there) while the real hub is Docker at `http://localhost:
   8101`. Fixed by writing `http://localhost:8101` into that file; desktop app is
   remote-viewer mode onto Docker hub. Login now works at :8101.
2. **`ff3d461`** feat(store): coordination.json can route local operators
   through REST gateway — `load_coordination` in crates/app/src/builders.rs now
   reads optional keys `remote_store_url -> COXAGENT_REMOTE_STORE_URL`,
   `remote_token -> COXAGENT_REMOTE_TOKEN` (with ${VAR} interpolation,
   never-clobber pre-set env). make_store prefers REMOTE when configured.
3. **`c4ec24e`** feat(store): runner-side auto-provision read —
   `provision_local_token()` wired into Command::Run in lib.rs feeds
   COXAGENT_REMOTE_TOKEN before make_store picks backend.
4. **`068a0be`** fix(store): canonicalize path + serialize tests —
   added `pub fn operator_token_path() -> Option<PathBuf>` returning
   $COXAGENT_TOKEN_FILE else <home>/CoXAgent/operator.token; provision_local_
   token(_base) now ignores base and uses operator_token_path(). The three tests
   in builders_tests manipulate global env → serialized behind module-level
   static ENV_LOCK Mutex to stop parallel clobbering.

All verified green at commit time: cargo check/clippy clean for touched files,
3 builders tests pass, workspace compiles.

## What was done this round — THE WRITER (completed)
`crates/presentation/src/server/auth.rs` (fn `login_ep`, `LoginResult::Ok`):
```rust
if let Some(secret) = auth.auto_issue_personal_token(&req.username).await {
    std::env::set_var("COXAGENT_REMOTE_TOKEN", secret.clone());
    persist_local_operator_token(&secret);
}
```
Two inlined helpers added before `login_ep`:
- `fn operator_token_path() -> Option<PathBuf>` — mirrors
  `coxagent_app::builders::operator_token_path` exactly ($COXAGENT_TOKEN_FILE else
  home/CoXAgent/operator.token). Inlined because presentation can't import from
  coxagent-app (dependency cycle app->presentation).
- `fn persist_local_operator_token(secret: &str)` — create parent dir, write
  secret + newline, then chmod owner-only 0600 (`#[cfg(unix)]`
  set_permissions from_mode(0o600)). Silent no-op on any failure so a bad token
  file can never break login.

Verified green: cargo check --workspace, clippy -p coxagent-presentation clean,
fmt clean for presentation, hex gate test pass.

NOTE on implementation method: writing this function repeatedly degenerated into
garbage tokens (~10+ attempts). The approach that WORKED reliably was extracting
byte-exact Rust fragments from existing source files (builders.rs operator_
token_path, claude.rs create_dir_all/set_permissions idiom, status.rs fs::write)
and assembling via Python string ops — NOT typing the Rust by hand.

### Follow-up — DONE
Unit tests for the writer landed in auth.rs:
- `persist_local_operator_token_writes_secret_at_env_path` — writes verbatim to
  canonical location + enforces owner-only 0600 on unix.
- `persist_local_operator_token_creates_parent_dir_and_overwrites` — creates
  missing parent dirs and overwrites in place.
Uses tempfile TempDir + ENV_LOCK serialization (mirrors builders_tests). Note:
the landing agent wrote these from scratch after repeated model degeneration in
my own output; transcription of multi-line Rust remained unreliable this session.

## CRITICAL BLOCKER THIS SESSION — model degeneration on multi-line Rust edits
Repeatedly (~10+ attempts incl subagents), producing ANY non-trivial multi-line Rust with cfg/traits/inline imports caused output corruption into garbage tokens (e.g. wrote fake fns like std_env_nonempty(), home_dir_optional(), persist_write(); subagent degenerated into incoherent word-soup). This happened specifically under sustained/complex editing load.
Mitigations that WORKED:
- Small precise edits via edit tool + immediate cargo check loop; compiler errors are reliable even when prose isn't.
- Python-splice replacement worked ONCE cleanly earlier session for docker_compose.rs but later corrupted too.
Current repo state is CLEAN & compiling precisely because each failed attempt was reverted (`git checkout -- <file>`) immediately after corruption before compiling wrong code survived.

Next session recommendation: for multi-line Rust, DON'T type it by hand — extract
byte-exact fragments from an existing source file and assemble with Python. That
got the writer landed cleanly this round.

## Key environment facts / gotchas (learned)
- `/Users/luton/CoXAgent/cxa/codebase` is a SYMLINK -> `/Users/luton/Projects/CoXAgent`. Same repo. Operators edit this working tree directly.
- Repo currently checked out on branch feat/CXA-F004 with uncommitted agent WIP (`crates/app/tests/docs_ports.rs`, `docker-compose.yml`) — DO NOT reset/sweep these; they're a live agent's work.
- AGENTS.md warning honored: never let app bind port 4000; use deploy.host_port (:8101) or COXAGENT_PORT. Do NOT restart operators mid-cycle unless necessary; killing loses in-flight branch work only if reset happens.
- Running CoX instances today were killed during this session to fix things; current state = desktop viewer remote-mode onto Docker hub :8101 only (no local cox-server running).
