FOLDER: -

# Editable Prompt System with Per-Project Override

**Keywords:** prompt, system prompt, role prompt, per-project override, prompts/*.md, editable prompts, BASE, ENGINEERING_STANDARDS, system_prompt(), workspace override

## Overview

The Editable Prompt System is the layer that builds every agent's system prompt from one shared source of truth in `crates/application/src/prompts.rs`. Today it ships embedded default role prompts — shared preamble `BASE`, process law `PROCESS_INVARIANTS`, house rules `ENGINEERING_STANDARDS`, plus one section per role (BA / SA / PO / SM / PD / DEV / TEST / DOCS) — composed by the free function `system_prompt(role_section)`.

The **per-project override**, letting a workspace replace those defaults with its own files under `<workspace>/prompts/*.md`, is declared as the next milestone in the module doc but is **not yet implemented**: every call site still passes an embedded constant directly. This page documents what exists today and marks exactly where the override plugs in.

## How it works

All default role text lives as Rust constants at the top of `crates/application/src/prompts.rs`: preamble `BASE` (line 6), process law `PROCESS_INVARIANTS` (line 23), standards `ENGINEERING_STANDARDS` (line 60), then one section per role (`BA` at 130, `SA` at 151, `PD` at 188, `DEV` at 204, `TEST` at 243, `DOCS` at 261) plus repair modes such as `DEV_HEAL` (line 228).

The single composer is:

```rust
pub fn system_prompt(role_section: &str) -> String {
    format!("{BASE}\n\n{ENGINEERING_STANDARDS}\n\n{role_section}")
}
```

So every run's system prompt = preamble + standards + one caller-selected role section. Roughly seventy-five call sites across the application and presentation crates pass an embedded constant into `system_prompt(...)` or directly into an engine request — e.g. `run_dev/mod.rs:683` uses DEV for feature work and DEV_HEAL when repairing a build; `run_ba.rs:131` uses BA then PO; forged-review paths use SA; QA uses TEST; docs phases use DOCS. None reads a file or consults project config for prompt text today.

On top of those static sections deterministic context **blocks** are appended to *task* prompts by helpers also living in prompts.rs: repo map (`repo_map_block`), code-graph focus (`focus_block`), team knowledge (`knowledge_block`, folding wiki + repo docs + prior fixes), deploy constraints (`deploy_constraints(&DeployConfig)` returns a host-port rule only when one is assigned), design-system guidance (`design_constraints`) only when populated, human steering on this ticket (`human_steering_block`), ticket journal brief and handoff notes (`ticket_brief_block`, `extract_brief_notes`, const BRIEF_PROTOCOL), ask protocol (`ask_protocol_block`) and team memory — each returning empty string when nothing applies. The DEV briefing path wires most of these together in [`build_request()`](crates/application/src/use_cases/run_dev/briefing.rs).

## Usage

There is no operator-facing usage yet because editing is not implemented; every agent run uses the embedded defaults automatically. To observe which prompt a phase used:

1. Run a cycle phase (dev / test / docs / review).
2. Read that engine's invocation log or transcript. Each request carries `system_prompt` and `task_prompt` as separate fields on `AgentRequest`; adapters forward them verbatim (OpenCode, Hermes) or concatenate them (Copilot) — see the Interface section.

Start the hub with defaults:

```
coxagent serve          # binds COXAGENT_PORT or deploy.host_port
# first BA cycle runs with:
#   system_prompt = BASE + ENGINEERING_STANDARDS + BA     (all embedded)
```

What an eventual per-project override would add, so future milestones pick it up instead of the embedded BA section:

```bash
mkdir -p <workspace>/prompts
cat > <workspace>/prompts/BA.md <<'EOF'
You are our analyst; answer strictly as JSON.
EOF
```

## Interface

Today this surface is pure Rust over embedded strings; there are no endpoints or CLI flags specific to this feature.

- Embedded constants: `BASE`, `PROCESS_INVARIANTS`, `ENGINEERING_STANDARDS`, plus one section per role (`BA`, `SA`, `PO`, `SM`, `PD`, `DEV`, `DEV_HEAL`, `TEST`, `DOCS`), all public in prompts.rs.
- Composer function: [`system_prompt(role_section: &str) -> String`](crates/application/src/prompts.rs).
- Task-prompt block builders (each returns an empty string when nothing applies): [`deploy_constraints(&DeployConfig)`], [`design_constraints(Option<&DesignSystem>)`], [`repo_map_block(...)`], [`focus_block(...)`], [`knowledge_block(...)`], [`human_steering_block(state, ticket)`], [`ticket_brief_block(state, ticket)`], [`ask_protocol_block(state, ticket)`], and the team-memory variants — all defined in prompts.rs.
- Engine port carrying it end-to-end: the struct field on [`AgentRequest`](crates/application/src/ports/outbound/engine.rs) — one invocation of an agent carries both a composed system prompt and a task prompt into a working directory.

Adapters that consume the field: OpenCode/Hermes forward both verbatim; Claude appends MCP-hint text when configured; Copilot concatenates system + task into one block.

## Configuration

No configuration key exists yet for custom prompt files or for enabling overrides — those settings ship together with implementation. The settings below alter generated prompt content today:

| Setting | Type / location | Default | Effect on generated content |
|---------|-----------------|---------|------------------------------|
| `workflow.language` | enum `Language` (`"en"` \| `"vi"`) in coxagent.json | `en` | `Language::reply_directive()` returns a Vietnamese directive appended to ceremony prompts when set to `vi`; empty for English |
| `workflow.token_saver` | bool in coxagent.json (serde default true) | `true` (`on`) | compresses large diff/log embeds and requests terse output, trimming tokens in composed prompts |

The role sections themselves are not configurable at runtime.

## Edge cases and limits

Because the override milestone is not landed, this capability deliberately does NOT happen yet; each statement below is the target contract an implementer should satisfy:

- **No disk load occurs at runtime.** No file under `<workspace>/prompts/*.md` is read today; nothing depends on a custom prompt file existing, so missing files cannot break a run.
- **Unknown roles.** When overrides are added, any file whose base name does not match a known embedded role (BA, SA, PO, SM, PD, DEV, DEV_HEAL, TEST, DOCS) should be rejected or ignored rather than injected as prompt text; prefer failing loudly over silently absorbing an unexpected file.
- **Malformed content.** A hand-written `.md` may be empty or contain only whitespace — the resolution rule must define whether that counts as "present" (override with blank) or "absent" (fall back to embedded). Undefined behavior here would silently weaken every agent of that role.
- **Precedence stays explicit.** Override wins per role; the shared `BASE` / `ENGINEERING_STANDARDS` sections keep their process-law invariants even when a role body is replaced — otherwise an override could strip mandatory house rules.
- The full override surface fails today by construction (compiler constant paths), so there is no runtime error path to observe until implementation lands.

## Code map

- `crates/application/src/prompts.rs` — embedded default prompts (`BASE`, `PROCESS_INVARIANTS`, `ENGINEERING_STANDARDS`, per-role sections), the `system_prompt(role_section)` composer, and every task-prompt block builder (`deploy_constraints`, `design_constraints`, `repo_map_block`, `focus_block`, `knowledge_block`, `human_steering_block`, `ticket_brief_block`, `ask_protocol_block`, team memory). This is where an override loader for `<workspace>/prompts/*.md` would live.
- `crates/application/src/config.rs` — per-project config structs; holds the two prompt-affecting settings (`workflow.language` → `Language::reply_directive()`, `workflow.token_saver`) and is where an override-on/path key would be added.
- `crates/application/src/config_parse.rs` — strict JSON parse of coxagent.json; validates config that could carry future override settings.
- `crates/application/src/ports/outbound/engine.rs` — defines `AgentRequest` (system + task prompt fields, role, work_dir, timeout) that every engine consumes.
- `crates/application/src/use_cases/run_dev/briefing.rs` — DEV-run briefing: `build_request()` calls each block builder then assigns the composed system prompt into an `AgentRequest`.
- Engine adapters consuming the field (in `crates/infrastructure/src/engine/`): `claude.rs`, `opencode.rs`, `hermes.rs`, `copilot.rs`, plus harness/metering wrappers.

## Related

- [docs/CXA-F003.md](CXA-F003.md) — same doc convention; shows how a verified feature page is written.
- The embedded DOCS role prompt itself defines this exact page skeleton and lives in prompts.rs (search for the Tech Writer section).
- Engine-model per-role override (`EngineMapping::per_role` / `resolve()`), an unrelated but similarly-named "override" feature in config.rs — do not confuse it with prompt-text overrides.
