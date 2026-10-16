# CXA-F420 — Multi-repo, multi-folder projects (spec)

**Status:** approved design (Steve, 2026-10-16) · **Owner:** F420 chain
**Dogfood target:** add the private `Harxes` repo as the second service of
project `cxa`, so the manual "fix harxes → tag → bump pin in CoXAgent" loop
becomes SA-coordinated work.

## 1. Problem

A project today IS one repo: one `work_dir`, one deploy gate, one PR queue.
Real products span several repos (hub + engine, backend + SDK) and several
packages inside one repo. Cross-repo changes are done by hand outside the
team, and monorepo tickets make agents sweep the whole workspace for a
one-package change.

## 2. Model: Project → Services

A **service** is the unit of code ownership and gating:

```jsonc
// registry.json / coxagent.json (per project)
"services": [
  {
    "name": "hub",                      // unique within the project
    "repo": "git@github.com:org/coxagent.git",
    "work_dir": "/path/to/clone",       // hub-managed clone
    "scopes": ["crates/", "web/"],      // optional monorepo folders
    "build_cmd": "cargo build --release",
    "test_cmd": "cargo test --workspace",
    "deploy": { /* existing deploy-gate config, per service */ },
    "status": "active"                   // onboarding | active | needs-setup | paused | archived
  }
]
```

Rules:

- **R1 — Backward compatibility is silent.** On first boot with the new
  schema, a legacy single-repo project is read as `services:[{name:"main",…}]`
  derived from its existing fields. No migration step, no prompt, no pause.
  A ticket without `service_tag` belongs to the first service.
- **R2 — `service_tag` becomes load-bearing.** The field already exists on
  tickets. BA/SA set it at mint time; selection/claiming cuts the worktree
  from that service's repo; the gate runs that service's build/test/deploy.
- **R3 — Per-repo isolation.** PR queue, `max_open_prs`, WIP limits, worktree
  pool, and live-log namespaces are **per service**, not per project. Two
  repos must never compete for one PR budget.
- **R4 — Monorepo scope, not new services.** Folders inside one repo are
  `scopes` on the service. The DEV brief names the scope; the gate runs only
  the affected package's tests (changed-paths detection), with the full-suite
  run reserved for the merge gate.
- **R5 — One attempt, one repo.** DEV never touches two repos in one attempt.
  A cross-repo ticket MUST be split by SA into an ordered chain along the
  dependency edge (see §5).

## 3. Onboarding (new project)

Principle: **a short interview where the machine has done the homework** —
the user confirms, never fills forms.

1. **Entry — one question: where is the code?** Paste one or more git URLs,
   pick local paths, or OAuth the forge and tick repos. Each repo becomes a
   row with its own lifecycle: `cloning → scanning → ready/failed`.
2. **Scan & propose.** Deterministic scanners first, LLM second:
   language/toolchain → proposed build/test cmds; Dockerfile presence →
   proposed deploy gate; workspace manifests (Cargo workspace,
   pnpm-workspace, go.work) → proposed `scopes`; manifest pins/imports →
   a draft **service dependency graph** (feeds §5). Results render as one
   editable **"Proposed services"** card per repo. AI proposes, the human
   approves, nobody types from scratch.
3. **Probe-first.** Before a service may become `active`: clean clone →
   build_cmd → test_cmd, time-bounded. Green → `active`. Red → `needs-setup`
   with the real error and a hint ("missing env X?", "tests need a DB?").
   Partial onboarding is normal: green services start working immediately;
   red ones block nothing but themselves. (Lesson: never trust an unprobed
   config — the Dockerfile `syntax` directive false-failed gates for a week.)
4. **Seed context.** Two free-text questions — *what is this project for?*
   and *what must never be touched?* — become the shared
   `project_context.md`. Each service gets an LLM-drafted service context
   from its README + tree, user-reviewed. If the forge has open issues,
   offer a tick-box import as the initial backlog.
5. **Timid defaults, earned trust.** New projects start with concurrency 1,
   a low `max_open_prs`, and `human.gate_verify` ON for the first N tickets.
   After X consecutive green landings the hub proposes loosening. Trust is
   earned through landings, not granted by a checkbox.

## 4. Adding a repo to a live project

Onboarding and expansion are the SAME flow: the "Add repo" button runs §3
steps 1–4 for one repo while the rest of the project keeps running.

- While `onboarding`/`needs-setup`, the new service is invisible to BA/SM —
  no tickets, no worktrees, no PR budget.
- On probe-green it flips `active`, and only then: BA intake learns the
  service (its description joins the intake prompt), a worktree pool + PR
  queue is provisioned, and the **cross-repo dependency pass** runs (does
  the new repo pin/import an existing service, or vice versa? record the
  edge).
- **No forced retag.** Untagged backlog stays with the default service.
  After an add, a *retag suggestion pass* proposes "these look like repo B"
  (same UX as the goals Retag dialog: suggestions + Apply, never silent).
- **Sprint hygiene.** A mid-sprint service never joins the current sprint's
  committed set; its tickets enter the backlog and commit next sprint, so
  velocity measurement stays clean.

## 5. Cross-repo changes (the dangerous part)

The dependency graph from §3.2/§4 gives SA the order. For "change API in A,
adapt consumer in B":

1. SA splits into an ordered chain: `[1/2] A: change + release tag` →
   (merge + tag green) → `[2/2] B: bump pin + adapt`.
2. SM holds `[2/2]` until `[1/2]` is terminal (existing parent/child hold +
   `sm_drive` resume machinery — reuse, don't rebuild).
3. The `[2/2]` brief carries the concrete pin/version produced by `[1/2]`.

This is the productized form of the manual harxes flow (fix → `vX.Y.Z` tag →
pin bump PR in CoXAgent).

## 6. Removing / pausing a service

Symmetry or zombie services: **pause** (keep tickets, stop assignment — like
`needs-setup`), **archive** (open tickets → `on_hold` with a reason,
worktrees cleaned, PR budget released). Both reversible except archive's
worktree cleanup.

## 7. UI

- Service chip on every ticket; board/backlog filter by service.
- Project page: one card per service (status, last probe, PR queue depth,
  last landing) — reuse the PanelShell terminal-state component (B196).
- "Add repo" lives on the project page and opens the §3 wizard scoped to one
  repo.

## 8. Rollout chain

| Ticket | Scope | Unlocks |
|---|---|---|
| F420a | Registry/config schema `services[]` + silent legacy migration (R1) + per-service worktree/PR/gate routing by `service_tag` (R2, R3) | Harxes as second service, manually configured |
| F420b | Onboard wizard: repo rows, scan/propose cards, probe-first, seed context (§3) + "Add repo" on live projects (§4) | Self-serve onboarding |
| F420c | Cross-repo dependency pass + SA chain rule + SM ordering (§5) | Automated harxes pin-bump loop |
| F420d | Monorepo scopes in briefs + changed-path gate narrowing (R4) | Faster attempts in big repos |
| F420e | Retag suggestion pass, pause/archive, service chips/panels (§6–7) | Day-2 operations |

Acceptance for the chain as a whole: project `cxa` runs with `hub` +
`harxes` services, and one real harxes bugfix lands end-to-end (fix in
Harxes, tag, pin bump in CoXAgent) with no human git commands.
