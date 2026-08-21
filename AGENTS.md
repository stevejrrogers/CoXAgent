<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **CoXAgent** (8938 symbols, 22355 relationships, 300 execution flows). Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

> Index stale? Run `node .gitnexus/run.cjs analyze` from the project root — it auto-selects an available runner. No `.gitnexus/run.cjs` yet? `npx gitnexus analyze` (npm 11 crash → `npm i -g gitnexus`; #1939).

## Always Do

- **MUST run impact analysis before editing any symbol.** Before modifying a function, class, or method, run `impact({target: "symbolName", direction: "upstream"})` and report the blast radius (direct callers, affected processes, risk level) to the user.
- **MUST run `detect_changes()` before committing** to verify your changes only affect expected symbols and execution flows. For regression review, compare against the default branch: `detect_changes({scope: "compare", base_ref: "main"})`.
- **MUST warn the user** if impact analysis returns HIGH or CRITICAL risk before proceeding with edits.
- When exploring unfamiliar code, use `query({search_query: "concept"})` to find execution flows instead of grepping. It returns process-grouped results ranked by relevance.
- When you need full context on a specific symbol — callers, callees, which execution flows it participates in — use `context({name: "symbolName"})`.
- For security review, `explain({target: "fileOrSymbol"})` lists taint findings (source→sink flows; needs `analyze --pdg`).

## Never Do

- NEVER edit a function, class, or method without first running `impact` on it.
- NEVER ignore HIGH or CRITICAL risk warnings from impact analysis.
- NEVER rename symbols with find-and-replace — use `rename` which understands the call graph.
- NEVER commit changes without running `detect_changes()` to check affected scope.

## Resources

| Resource | Use for |
|----------|---------|
| `gitnexus://repo/CoXAgent/context` | Codebase overview, check index freshness |
| `gitnexus://repo/CoXAgent/clusters` | All functional areas |
| `gitnexus://repo/CoXAgent/processes` | All execution flows |
| `gitnexus://repo/CoXAgent/process/{name}` | Step-by-step execution trace |

## CLI

| Task | Read this skill file |
|------|---------------------|
| Understand architecture / "How does X work?" | `.claude/skills/gitnexus/gitnexus-exploring/SKILL.md` |
| Blast radius / "What breaks if I change X?" | `.claude/skills/gitnexus/gitnexus-impact-analysis/SKILL.md` |
| Trace bugs / "Why is X failing?" | `.claude/skills/gitnexus/gitnexus-debugging/SKILL.md` |
| Rename / extract / split / refactor | `.claude/skills/gitnexus/gitnexus-refactoring/SKILL.md` |
| Tools, resources, schema reference | `.claude/skills/gitnexus/gitnexus-guide/SKILL.md` |
| Index, status, clean, wiki CLI commands | `.claude/skills/gitnexus/gitnexus-cli/SKILL.md` |

<!-- gitnexus:end -->

## Code layout (why merges keep conflicting)

Two agents editing the same oversized module conflict by construction. The rule
that prevents it is structural, not procedural:

- One cohesive unit per file; one bounded context per directory.
- Past ~500 lines, split along a real seam and name the new file for what it
  IS. `helpers.rs` / `utils2.rs` are not seams — a split you cannot name has
  made two problems out of one.
- Touching an oversized file? Leave it smaller: extract the part you came to
  change, with its tests. Do not rewrite the module for a one-behaviour ticket.
- No current offenders — everything on this list has been split. Keep it that
  way: a NEW file crossing ~500 lines is the moment to cut along a seam. The web UI lives in
  `web/app.css` + `web/js/*.js` (classic scripts, ONE shared scope, load order
  matters); any UI change must pass `cd e2e && npx playwright test` (golden
  screenshots + console-error gate).

## Running the app you are building

This repository IS the tool running you. When you start a build of it to try
something, never let it bind the hub's port (4000): the desktop window points
there, and a hub that finds its port taken moves to another one — the app then
looks dead while everything is in fact running. Use the project's own
`deploy.host_port`, or set `COXAGENT_PORT` before `coxagent serve`. Stop what
you started when you are done.

## IO discipline (enforced by hexagonal_gate.rs)

Application code never calls `std::process` / `std::fs` directly. The pattern,
end to end, is `GitPort::working_tree` → `run_dev/gates.rs`: the adapter takes
one snapshot of the outside world; the decision is a pure function of the
snapshot, testable with a struct literal. `crates/app/tests/hexagonal_gate.rs`
fails any NEW application file that does direct IO, and its grandfather list
may only shrink — fixing a file without delisting it also fails.

## Ongoing work — read before starting

- Active feature branch tip: `main` (contains REST-store integration, mega mind).
- Hand-off notes with the exact next steps: `.claude/handoff-rest-runner.md`.
  Read it before making changes around the state store / API transport /
  auth hardening. It lists committed work and the pending P5a (auth
  enforcement) recipe that is not yet landed.
