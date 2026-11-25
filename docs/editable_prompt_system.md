FOLDER: -

# Editable Prompt System with Per-Project Override

**Keywords:** prompt, system prompt, role prompt, per-project override, prompts/*.md, editable prompts, BASE, ENGINEERING_STANDARDS, compose_role_system(), prompts_resolve, workspace override

## Overview

The Editable Prompt System builds every agent's system prompt from one shared source of truth in `crates/application/src/prompts.rs`: shared preamble `BASE`, process law `PROCESS_INVARIANTS`, house rules `ENGINEERING_STANDARDS`, plus one section per role. As of CXA-F445 (split out of CXA-F414) the **per-project override is real**: a workspace can replace any single role's body by dropping a Markdown file at `<workspace>/prompts/<role>.md`. Everything goes through one pure resolver, `coxagent_application::prompts_resolve`.

## How it works

1. **Embedded defaults** live as public Rust constants at the top of `crates/application/src/prompts.rs`: `BASE` (line 6), `PROCESS_INVARIANTS` (23), `ENGINEERING_STANDARDS` (60), then role sections (`BA` 130, `SA` 151, `PD` 188, `DEV` 204, `TEST` 243, `DOCS` 261, repair mode `DEV_HEAL` 228).
2. **Pure resolution** lives in `crates/application/src/prompts_resolve.rs` (exported as `prompts_resolve` in `lib.rs`). The contract:
   - `KNOWN_ROLE_KEYS: &[&str] = &["po","sm","ba","sa","pd","dev","dev-heal","test","docs"]`; `is_valid_role_key(key)` gates everything.
   - `load_role_override(files, work_dir, prompts_subdir, role_key) -> Option<String>` reads `<work_dir>/<prompts_subdir>/<role_key>.md` **through the outbound port** `WorkspaceFilesPort` (`crates/application/src/ports/outbound/workspace.rs`) — no direct `std::fs` in application code, keeping the hexagonal gate green. A blank/whitespace-only file counts as ABSENT (`Some` never carries an empty body), so the embedded default wins instead of an empty section leaking into every run.
   - `resolve_role_body(embedded, loaded_override) -> Cow<str>` is the pure precedence decision: non-blank override wins per role, otherwise the embedded constant.
   - `compose_role_system(files, work_dir, prompts_subdir, role_key) -> String` composes `format!("{BASE}\n\n{ENGINEERING_STANDARDS}\n\n{body}")`. Those two invariants ALWAYS survive — an override replaces only its own role's section, never the base rules or standards.
3. **IO happens only in the infrastructure adapter** `FsWorkspaceFiles` (`crates/infrastructure/src/workspace_files.rs`); tests inject an in-memory double implementing `WorkspaceFilesPort`.
4. **Task-prompt blocks** are unchanged: deterministic context appended by the helpers in prompts.rs (`repo_map_block`, `focus_block`, `knowledge_block`, `deploy_constraints(&DeployConfig)`, `design_constraints(Option<&DesignSystem>)`, `human_steering_block`, `ticket_brief_block` / `extract_brief_notes` / const `BRIEF_PROTOCOL`, `ask_protocol_block`, team-memory variants) — each returning an empty string when nothing applies. Overrides affect SYSTEM prompts only, never these task blocks.
5. The four correctness contracts g1–g4 are regression-tested in `crates/application/tests/prompts_resolve.rs` (CXA-F022): missing file ⇒ embedded wins; stray/non-role key ⇒ rejected, never injected; blank file ⇒ treated as absent; `BASE` + `ENGINEERING_STANDARDS` survive role replacement.

Note on coverage: per-role override resolution ships and is gate-tested at the resolver level; call sites still mostly pass embedded constants into `system_prompt(role_section)` directly (~75 across application/presentation, e.g. `run_dev/mod.rs:683` picks DEV vs DEV_HEAL, `run_ba.rs:131` BA then PO, review uses SA, QA TEST, docs DOCS). Wiring individual phase briefings through `compose_role_system(...)` is the remaining incremental migration. Also note `embedded_for()` currently maps bodies for ba/sa/pd/dev(+dev-heal)/test/docs; `po` and `sm` accept overrides over an empty embedded default.

## Usage

To give your workspace a custom analyst persona:

```bash
mkdir -p <workspace>/prompts
cat > <workspace>/prompts/ba.md <<'EOF'
You are our analyst; answer strictly as JSON.
EOF
```

The next BA ceremony loads that body through `compose_role_system(Some(&files), work_dir, "prompts", "ba")`; the file is optional per role — roles without a `.md` keep their embedded text.

Filename MUST be lowercase kebab-case matching a `KNOWN_ROLE_KEYS` entry exactly: `po.md`, `sm.md`, `ba.md`, `sa.md`, `pd.md`, `dev.md`, `dev-heal.md`, `test.md`, `docs.md`. No endpoint or CLI flag exposes editing today; you edit files under version control like any other project content.

To observe which prompt a phase used: run a cycle phase and read that engine's invocation log/transcript — each request carries separate `system_prompt` and `task_prompt` fields on [`AgentRequest`](crates/application/src/ports/outbound/engine.rs).

## Interface

Rust surface (crate `coxagent_application::prompts_resolve`):

- `pub const KNOWN_ROLE_KEYS: &[&str]` — the nine valid role keys above.
- `pub fn is_valid_role_key(key: &str) -> bool`
- `pub async fn load_role_override(files: Option<&dyn WorkspaceFilesPort>, work_dir: &Path, prompts_subdir: &str, role_key: &str) -> Option<String>`
- `pub fn resolve_role_body<'a>(embedded_body: &'a str, loaded_override: Option<&'a str>) -> Cow<'a, str>` — pure, trivially unit-testable precedence rule.
- `pub async fn compose_role_system(files: Option<&dyn WorkspaceFilesPort>, work_dir: &Path, prompts_subdir: &str, role_key: &str) -> String`

Still in `prompts.rs`: `pub fn system_prompt(role_section: &str) -> String` (direct, un-overridden composition used by existing call sites) and every task-prompt block builder listed above. Embedded constants remain public: `BASE`, `PROCESS_INVARIANTS`, `ENGINEERING_STANDARDS`, plus `BA`, `SA`, `PO`, `SM`, `PD`, `DEV`, `DEV_HEAL`, `TEST`, `DOCS`.

Outbound port: trait `WorkspaceFilesPort` (`read`, `write`, `list`, `stat`, …) in `crates/application/src/ports/outbound/workspace.rs`; adapter `FsWorkspaceFiles` in `crates/infrastructure/src/workspace_files.rs`. Engine adapters consuming the composed field: OpenCode/Hermes forward both verbatim; Claude appends MCP-hint text when configured; Copilot concatenates system + task.

No HTTP endpoints or CLI flags are specific to this feature.

## Configuration

| Setting | Location | Default | Effect |
|---|---|---|---|
| `<work_dir>/<prompts_subdir>/<role_key>.md` files | workspace filesystem | none present | Non-blank body replaces that ONE role's embedded section; blank/missing ⇒ embedded default |
| `prompts_subdir` argument | caller of `compose_role_system` | `"prompts"` | Relative subdirectory searched inside the project working dir |
| `workflow.language` | coxagent.json, enum `"en"` \| `"vi"` | `"en"` | `Language::reply_directive()` returns a Vietnamese directive appended to ceremony prompts when `vi` |
| `workflow.token_saver` | coxagent.json bool | `true` | Compresses large diff/log embeds and requests terse output in composed prompts |

There is no on/off switch for overrides: a file present = active, absent = inert.

## Edge cases and limits

- **Blank file = absent** (`g3`): whitespace-only `ba.md` does NOT blank the BA prompt; the embedded body wins.
- **Unknown/stale filenames are ignored, not injected** (`g2`): `prompts/README.md` or a typo'd key loads nothing — deliberately preferred over silently absorbing unexpected text into a system prompt.
- **Invariants cannot be stripped** (`g4`): `BASE` opens every composed prompt and `ENGINEERING_STANDARDS` follows it even when only the role body is replaced. Do not put per-workspace process-law edits in role files; they belong upstream in the repo's prompt sources.
- **Read-only per run, resolved per composition**: nothing caches; an edited `.md` takes effect on the next agent run without restart, but there is no hot-reload notification.
- **Only system prompts** are overridable — task-prompt blocks (repo map, briefs, ask protocol…) stay code-generated.
- Missing/unavailable `WorkspaceFilesPort` (`None`) degrades gracefully to full embedded behaviour, so a tool-less test harness cannot break prompt generation.
- Deliberately does NOT do: nested/per-agent-variant overrides, per-role multiple files, templating/includes between files, or validation beyond blankness of override content.

## Code map

- `crates/application/src/prompts.rs` — embedded defaults (`BASE`, `PROCESS_INVARIANTS`, `ENGINEERING_STANDARDS`, per-role sections), `system_prompt(...)`, every task-prompt block builder.
- `crates/application/src/prompts_resolve.rs` — THE override resolver: `KNOWN_ROLE_KEYS`, `is_valid_role_key`, `file_for`, `embedded_for`, `load_role_override`, `resolve_role_body`, `compose_role_system`.
- `crates/application/tests/prompts_resolve.rs` — regression tests g1–g4 gating the four correctness contracts (with an in-memory `WorkspaceFilesPort` double).
- `crates/application/src/ports/outbound/workspace.rs` — `WorkspaceFilesPort` + `FileMeta`; the port all prompt-file IO flows through.
- `crates/infrastructure/src/workspace_files.rs` — `FsWorkspaceFiles`, the real-filesystem adapter.
- `crates/application/src/lib.rs` — `pub mod prompts_resolve;` re-export (line 46).
- `crates/application/src/config.rs` / `config_parse.rs` — the prompt-affecting settings `workflow.language` / `workflow.token_saver`.
- `crates/application/src/use_cases/run_dev/briefing.rs` — DEV briefing `build_request()`: where phase compositions get wired onto `AgentRequest`.
- `crates/infrastructure/src/engine/{claude,opencode,hermes,copilot}.rs` — adapters carrying system/task prompts end-to-end.

## Related

- [docs/CXA-F003.md](CXA-F003.md) — doc-convention reference; the DOCS role prompt in prompts.rs defines this exact skeleton.
- CXA-F414 (parent ticket, split); CXA-F445 (this: extracted per-project override into `prompts_resolve.rs`); CXA-F022 (regression contracts g1–g4); EngineMapping::per_role / resolve() in config.rs — unrelated same-named engine-model override, do not confuse.
