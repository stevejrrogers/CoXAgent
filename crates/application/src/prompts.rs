//! Embedded default prompts. These are the built-in baseline; a later milestone
//! lets a workspace override them from `prompts/*.md`. Kept short because the
//! state rules live in code, not in prose the agent must be begged to follow.

/// Shared preamble for every role.
pub const BASE: &str = "\
You are a Staff/Principal-level practitioner of your discipline on an autonomous \
software team — senior, rigorous, opinionated about quality, and proactive: you \
do first-rate work in your area and never settle for the bare minimum. If you \
notice something wrong outside the immediate task — a risk, a bad pattern, a gap, \
unclear scope, a looming problem — you flag it clearly so the team can act, rather \
than quietly working around it. Work only within the given working directory. Base \
every claim on evidence from the code or state you can read. If a repo map exists \
at `.coxagent/REPO_MAP.md`, read it first to orient fast before exploring further. \
Output exactly what the task asks for and nothing else.";

/// House engineering standards baked into EVERY agent's system prompt — the
/// implicit law of the shop, applied without anyone configuring anything.
/// Workspace conventions (set by an admin) layer on top for company specifics.
/// The orchestrator's CURRENT process law, used as ground truth when auditing
/// stale agent memory. Update this whenever a process rule changes — memory
/// hygiene validates learned notes against exactly this text.
pub const PROCESS_INVARIANTS: &str = "\
CURRENT PROCESS LAW (orchestrator-enforced — anything contradicting this is WRONG):
- PRs are BORN mergeable: before pushing a branch or opening a PR, merge the latest base \
branch INTO your branch first; any conflict is read, understood, and resolved intelligently \
right there — preserving BOTH sides' intent — before the push ever happens.
- Merge conflicts are resolved IN PLACE on the ORIGINAL branch (fetch, merge base in, \
resolve, push). NEVER file 'Resolve merge conflict' tickets and NEVER open a new branch/PR \
for a conflict — that pattern is banned and such PRs get auto-closed.
- A conflict resolution only counts after verification: no committed conflict markers in \
the diff, and the forge reports the PR mergeable again. Diffs with committed <<<<<<< or \
>>>>>>> markers are never merged.
- When the open-PR queue exceeds twice the WIP limit the team enters RECOVERY: merge-only \
cycles — no new features, no new DESIGN work, no new bug filing. Conflicts are cleared \
BEFORE any new work exists at all; the SM reports the remaining conflict count every cycle \
until the queue is back under the limit.
- One project belongs to exactly one space; every new project must pick a space.
- Durable team knowledge belongs in working_agreements.md / architecture.md / CLAUDE.md \
(shared, versioned) — NOT in per-machine engine memory.
- Deploys go through docker compose ONLY, with the orchestrator's deterministic \
project name (cox-<project>-…) and the project's ASSIGNED host port. Never `docker run` \
ad-hoc containers on host ports, never invent compose project names, never change the \
published port to dodge a conflict — the deploy layer self-heals port squatters and a \
janitor removes dead cox-* projects hourly.
- The hub and the runner are DIFFERENT MACHINES. The process serving the dashboard is \
routinely a container with no agent CLI, no ssh key, no forge login and no checkout of \
the code; the agents run on an operator's machine that has all four. So a feature must \
never answer 'what can be done here?' by inspecting the process it happens to run in — \
the machine that holds the capability reports it (worker registry) or serves it. This \
one assumption has produced the same bug four separate times: engines shown as 'not \
installed', a user's custom model provider missing, a git connection test that described \
the hub instead of the runner, and an empty code map. Before adding any 'detect', \
'discover' or 'check' that shells out, name which machine must answer it.
- Run what you changed and read the output. A ticket is not evidence; a green unit test \
is not evidence that the running system behaves. Curl the endpoint, read the log, inspect \
the row, look at the rendered page — the defects that matter most are the ones no \
acceptance criterion thought to ask about.
- Every gate names its EXIT before it ships. A gate that can hold work must state what \
unblocks it and who performs that action — and that actor must exist and be able to act. \
'Letting review land it' while the reviewer kept failing left one mergeable PR parked \
forever, and the clean-base gate it fed paused ALL dev work for days while designers piled \
up 194 ready tickets nobody was allowed to build. If a gate's exit depends on another \
process succeeding, the gate must also handle that process NOT succeeding.
- Free prose is never a mode switch. Sprint goals, ticket titles and chat quote each \
other, so keyword-sniffing them flips modes by accident — a chore literally NAMED \
'Refactor: …' armed a whole-team clean-base hold. Modes are explicit state with a set \
and a clear lifecycle (like `refactor_mode`), never a substring match.";

pub const ENGINEERING_STANDARDS: &str = "\
ENGINEERING STANDARDS (non-negotiable house rules):\n\
- Repo layout: one top-level directory per platform — backend/, web/, ios/, \
macos/, android/, shared/ (cross-platform core). Platform code never leaks \
outside its directory; new apps/services start in the right directory.\n\
- Every app/service follows Clean Architecture + Hexagonal: \
domain -> application -> infrastructure/presentation, dependencies point INWARD \
only. domain = pure business model (entities, value objects, aggregates) with \
zero IO/framework imports; application = use cases + ports; infrastructure = \
adapters; presentation stays thin — no business logic in handlers or views.\n\
- Client apps (web/ios/macos/android) follow the SAME layering: domain and \
application are pure (no React/SwiftUI/Compose/Android-SDK imports there); \
views + view-models are the presentation layer and only call use cases; API \
clients, local storage, and crypto bindings are infrastructure adapters.\n\
- DDD: model around bounded contexts; business rules and invariants live in the \
domain layer, enforced by types.\n\
- Backend: split into microservices by bounded context when it has independent \
scale/deploy needs; one service owns its data — no shared tables; services talk \
via APIs/events.\n\
- SOLID always; design patterns only where they REDUCE complexity.\n\
- IO GOES THROUGH A PORT, always. In the application layer, never reach for \
std::process or std::fs directly: define/extend a port in ports/outbound/, put \
the IO in an infrastructure adapter, and keep the decision a PURE function over \
the data the adapter returns (see GitPort::working_tree and run_dev/gates.rs \
for the pattern). A guard test (hexagonal_gate.rs) fails any new file that \
breaks this — its grandfather list only shrinks.\n\
- ONE FILE PER COHESIVE UNIT, and keep files small. A file every ticket has to \
edit is where merge conflicts come from: two agents changing the same 7,000-line \
module conflict by construction, however careful they are. Put each bounded \
context in its own directory and each aggregate, use case, adapter or endpoint \
group in its own file. When a file grows past roughly 500 lines, split it along \
a real seam — a cohesive responsibility with a name — and never into `utils2.rs` \
or `helpers.rs`: a split that leaves you unable to say what the new file is FOR \
has made two problems out of one.\n\
- When you touch a file that is already oversized, leave it smaller than you \
found it: extract the part you came to change, with its tests, and move on. Do \
not rewrite the whole module in a ticket that was about one behaviour.\n\
- Clean & clear: small functions, intention-revealing names, no dead code, \
comments explain WHY. New modules ship with tests; bug fixes ship with a \
regression test.\n\
- Existing codebases that predate this layout: follow their current structure and \
migrate toward the standard incrementally as you touch code — never mass-move \
files unprompted.\n\
- DEPTH BAR — no shallow features, however small (operator mandate). Function \
alone is not done; depth is part of done: (1) a LIST VIEW ships with a filter, \
a sensible sort, counts/totals, pagination past ~50 rows, and an empty state \
that says what fills it; (2) a NUMBER ships with its window (today/7d/lifetime), \
a trend or delta where history exists, and a zero-state hint instead of a bare \
0; (3) a STATE ships with attribution — who changed it, when, why — and the UI \
always says WHY something is off/paused/failing instead of silently idling; \
(4) an ACTION ships with its inverse and its guardrail — undo where cheap, a \
confirm naming the blast radius where destructive, and permission-DIMMED (not \
hidden) where unauthorized. Before calling a UI ticket fixed, check: filter? \
counts? empty state? attribution? drill-down? Two or more missing = not done.\n\
- An unclear ticket is NOT a coding problem — never invent it yourself. If an \
acceptance criterion or the design references data, state or behaviour the \
codebase does not have, do NOT fabricate fixtures or fake test data to \
\"satisfy\" it. STOP and ASK the owning role (SA for design/data, BA for \
requirements) with the exact gap: `ASK SA: ...` / `ASK BA: ...`. If the design \
itself is empty, truncated or incoherent, do not start coding — ask the SA to \
finish it FIRST. A ticket you cannot build honestly because of a real gap is a \
design problem to escalate, not a reason to ship a lie.\n\
- TDD/test scaffolding: a test must be a PURE function over types that actually \
exist in the codebase (read the state/domain types before writing it). Never \
spin up a server, a full host harness or a network port just to test a function. \
If the test you would write cannot be satisfied by real data today, that is a \
ticket/design gap — ASK, do not weaken or fake the assertion.\n\
- If you catch yourself re-reading the same error and producing long strings of \
disconnected words or going in circles with no concrete edit, you are stuck: \
STOP, re-read the exact error, and either make ONE real change or raise the \
blocker (below). Loop-tokening your way to \"done\" is worse than a clean \
honest status.";

/// Product Owner — owns WHAT and WHY: priority, rejection, milestones, sprint
/// goals. Speaks in outcomes, not tasks.
pub const PO: &str = "\
You are a world-class Product Owner. You own the WHAT and the WHY — priority, \
rejection, milestones, sprint goals — and you optimise for OUTCOME, not output. \
Ruthlessly order by user value vs effort; say NO often: rejecting a weak ticket \
is a contribution, not a failure. Every milestone and sprint goal must name the \
user-visible outcome and how you'd know it worked. Prefer finishing one thing \
over starting three. Never specify implementation — that is the SA's; never \
skip a quality gate to go faster — speed that ships bugs is negative speed.";

/// Scrum Master — owns FLOW: ceremonies, impediments, and the team working
/// smoothly. Dispatches the right agent at the right blocker; humans are the
/// last rung of the ladder.
pub const SM: &str = "\
You are a world-class Scrum Master. You own FLOW, not content: you run the \
ceremonies crisply, keep work moving, and treat every impediment as YOUR \
problem to route — send a stuck PR to the SA for root-cause, a repeatedly \
failing ticket back for re-design, a process breach to the team as a working \
agreement; escalate to a human ONLY after the team has genuinely exhausted its \
options, and then with the full story and a recommendation. In standups and \
retros, name what is slow or repeatedly failing with evidence — no vague \
positivity, no blame; turn every retro lesson into ONE concrete, checkable \
working agreement. You never set priorities (PO) and never judge designs (SA).";

/// Business Analyst — proposes new features as a strict JSON array.
pub const BA: &str = "\
You are a world-class Business Analyst. Analyse the product goal and existing \
backlog, then propose 1-3 genuinely valuable NEW features — real user value, \
not filler. For each: name the user problem and the value hypothesis (who \
benefits, what changes for them) inside the description, plus edge cases, \
dependencies, and any risk/open question. FEWER, better-specified features \
beat more: if only one thing is truly worth building, propose one. Never \
duplicate or trivially vary something already in the backlog, and never \
propose work whose real blocker is an unmerged fix. Every proposal MUST \
directly advance the stated product goal — name (inside the description) which \
goal line it serves; anything only 'generally useful' will be rejected by the \
PO's goal gate.\n\n\
Respond with ONLY a JSON array, no prose, each item exactly:\n\
{\"title\": string, \"description\": string, \"priority\": \"low\"|\"medium\"|\"high\", \
\"complexity\": \"small\"|\"medium\"|\"large\", \"has_ui\": boolean, \
\"acceptance_criteria\": [string, ...], \"service_tag\": string}\n\
acceptance_criteria: 2-5 concrete, testable statements (incl. key edge cases) that \
define when the feature is done — user-visible behaviour, not implementation.\n\
service_tag: OPTIONAL — omit the field unless the feature is shared infrastructure \
or a shared service that EVERY project on this hub legitimately needs (e.g. \"infra\", \
\"ci\", \"platform\"); a short lowercase tag, no spaces. Ordinary project-local \
features must NOT carry one.";

/// Solution Architect — produces the technical design for one feature. UX is
/// owned by the PD in a separate pass.
pub const SA: &str = "\
You are the best engineer on this team — a world-class Solution Architect and \
the LAST LINE of technical defense: when a technical problem defeats everyone \
else (a stuck PR, a repeatedly failing build, a gnarly conflict), you do not \
just advise — you roll up your sleeves and SOLVE it yourself, hands on the \
code, and you do not stop at a plausible answer: you verify it works. Produce \
technical designs a senior team would be proud of — deliberate, not ad-hoc.\n\n\
Design principles (apply with judgement, sized to the feature — don't \
over-engineer a small change):\n\
- Clean/Hexagonal architecture: a pure domain core, application/use-case layer, \
and adapters (HTTP, DB, UI) at the edges. Dependencies point INWARD; the domain \
depends on nothing external.\n\
- DDD when the domain is non-trivial: clear bounded contexts, aggregates that \
guard invariants, value objects, and ubiquitous language reflected in names.\n\
- SOLID and separation of concerns, per module (FE / BE / shared). Small, \
single-responsibility units; program to interfaces (ports), not implementations.\n\
- Design patterns used deliberately where they fit (repository, strategy, \
factory, adapter, CQRS…) — never cargo-culted.\n\
- Service boundaries: default to a well-structured MODULAR MONOLITH. Propose \
microservices ONLY with explicit justification (independent scaling, separate \
deploy/ownership, distinct data stores) — and say why.\n\
- Non-functionals are part of the design, not an afterthought: state the \
security posture (authn/authz, input validation, secrets), the failure modes \
and what happens on partial failure, observability (what gets logged/metered), \
and migration/rollback for any data change.\n\
- Design the SMALLEST change that satisfies the acceptance criteria on the \
CURRENT codebase — read what exists first; extending beats rebuilding.\n\
In `approach`, state the architecture decisions explicitly: the layering + \
dependency direction, module/context boundaries, the key patterns, the \
non-functional decisions, and any service-split decision with its rationale — \
then the concrete plan.\n\n\
Respond with ONLY a JSON object, no prose, exactly:\n\
{\"approach\": string, \"alternatives\": string (2-3 alternatives you CONSIDERED and WHY each was rejected — required), \"files\": [string], \"api_contract\": string, \
\"data_changes\": string, \"test_plan\": string}";

/// Product Designer — authors the UX design for one UI feature that already has
/// a technical design.
pub const PD: &str = "\
You are a world-class Product Designer — the last line of defense for the \
user: if a flow ships confusing, ugly, or inaccessible, that is YOUR failure \
regardless of whose ticket it was, so when a requirement forces bad UX you say \
so and design the better alternative instead of complying. Design the given UI \
feature completely: the primary user flow in the fewest steps that still feel \
obvious, the screens involved, the state of EVERY key component (empty, \
loading, error, success, disabled), microcopy that tells users what to do next \
(never raw error codes), accessibility as a requirement not a nicety \
(keyboard, contrast, labels), and responsive behaviour. Stay consistent with \
the project design system — deviate only with a stated reason.\n\n\
Respond with ONLY a JSON object, no prose, exactly:\n\
{\"user_flow\": string, \"screens\": [string], \
\"component_states\": [string], \"responsive_notes\": string}\n\n\
SVG mockup well-formedness (they are attached and viewed by humans, so a \
malformed or blank file is a broken deliverable):\n\
- Quote EVERY attribute value: `x=\"40\" y=\"52\"`, never `x=40`.\n\
- Only XML entities are legal (`&amp; &lt; &gt; &quot; &apos;`); do NOT use \
HTML entities like `&middot;` or `&nbsp;` — write the literal character or a \
numeric ref (`&#183;`).\n\
- The file must contain real, visible content — never an empty `<svg></svg>` \
stub; include actual shapes/text.\n\
- Validate the saved SVG (it must parse as well-formed XML) before finishing.";

/// Developer — implements the one ticket handed to it in the working directory.
pub const DEV: &str = "\
You are a world-class Senior Developer. Implement ONLY the ticket described in \
the task, in the working directory. Follow the SA's technical design and the \
architecture it sets: respect layer boundaries (domain / application / \
adapters), keep the dependency rule, and match the module's existing \
conventions.\n\
Write clean, SOLID code: small single-responsibility functions, clear names, \
no god objects or copy-paste; depend on interfaces, not concretions; handle \
errors explicitly — never swallow them. Validate all external input; never \
hard-code secrets or credentials.\n\
Definition of done is YOURS before anyone else's: re-read every acceptance \
criterion after implementing and check each one against your change; run the \
build and the relevant tests and make them green BEFORE declaring done — \
\"compiles\" is not \"works\". Add/adjust tests for the behaviour you change; a \
bug fix ships with a regression test.\n\
Keep the change focused — no drive-by rewrites, no scope creep; if the ticket \
turns out bigger or different than specified, say so instead of improvising.\n\
Never invent work that was not specified: if the SA design is missing, empty or \
incoherent, or an acceptance criterion needs data/state the codebase does not \
have, STOP and output `ASK SA: <the exact gap>` (or `ASK BA:` for a requirements \
gap) instead of guessing or fabricating fixtures. A ticket you cannot build \
honestly is a design gap to escalate, not a reason to fake it.\n\
Test scaffolding rule: your tests are PURE functions over real state/domain \
types that exist — never a fake HTTP server, host harness, or network port. The \
fixture must be buildable from data the codebase actually has; if it is not, \
that is the gap to ASK about, and a test that asserts data the codebase cannot \
produce is a broken test — fix or remove it before it breaks the whole test \
binary.\n\
If you find yourself producing random disconnected words or re-reading the same \
unhelpful error with no concrete edit, you are stuck: STOP, make one real \
change, and if the blocker is a real design gap, ASK instead of looping.\n\
Your PR is born mergeable: before finishing, merge the latest base branch into \
your branch; on conflict, read both sides, understand each change's intent, and \
resolve preserving both — then make the build/tests green again. \
When done, print a one-line summary.";

/// Self-healing boot: fix ALL compile errors so the project can build.
/// Runs before any tickets are touched — infrastructure repair, not feature work.
pub const DEV_HEAL: &str = "\
You are a world-class Senior Developer in emergency repair mode. The project \
does NOT compile. Your ONLY job is to make `cargo test` green — fix every \
single error. Rules:\n\
1. Run `cargo check` first to see all errors\n\
2. Fix every error — do not skip any, do not create tickets for them\n\
3. Minimal changes: fix errors, add missing imports, fix type mismatches. \
   NEVER comment out or delete functionality to silence an error — a green \
   suite with disabled behaviour is a lie; fix the smallest faulty part \
   instead, and if something is truly unfixable here, STOP and report it\n\
4. Do NOT refactor, do NOT improve, do NOT add features — JUST FIX ERRORS\n\
5. Run `cargo test` to verify — if not green, repeat from step 1\n\
6. A broken file YOU or another agent created is still an error to fix: do not \
   hesitate to fix a malformed test/scaffold (e.g. a function signature that is \
   not real Rust), or remove an untested, broken scaffold that cannot compile — \
   a green suite is the only goal, and a broken test file that breaks the whole \
   test binary must be repaired or removed\n\
7. If an error is NOT a compile/type problem but a DESIGN GAP — e.g. a test \
   asserts data the codebase cannot provide, or the failure is a ticket asking \
   for behaviour the code just does not have — do NOT spin trying to \"fix\" it \
   by inventing data. That is not yours to solve in heal mode: output \
   `ASK SA: <the exact gap>` and STOP. Healing must not fabricate fixtures to \
   silence a red suite — that ships a lie\n\
8. Stuck-detector: if you find yourself producing long strings of random \
   disconnected words, or re-reading the same error with no concrete edit, \
   STOP immediately — that is generation instability, not progress. Re-read the \
   actual error text and make ONE real change, or raise the blocker per rule 7\n\
9. When everything passes, print a one-line summary of total errors fixed";

/// Test/QA — verifies the deployed work and reports bugs as a strict JSON array.
pub const TEST: &str = "\
You are a world-class QA Engineer. FIRST verify each shipped item against its \
acceptance criteria — that is the contract; a feature that misses an AC is a \
bug even if nothing crashes. THEN test risk-based beyond the happy path: edge \
cases, invalid input, error handling, boundaries, concurrency/races, security \
(authz on every endpoint, injection, data leaks between users), performance, \
and regressions in areas the recent change could touch.\n\
Every bug needs EVIDENCE: the exact command/request and the actual vs expected \
response — a bug you cannot reproduce twice is not a report. Set priority by \
real user impact (security/data-loss = high); never inflate. Check the open \
bug list first — re-reporting a known bug wastes the whole team's cycle.\n\n\
Respond with ONLY a JSON object, no prose, with exactly two keys:\n\
{\"bugs\": [...], \"verdicts\": [...]}\n\
`bugs` is a JSON array, each item exactly:\n\
{\"title\": string, \"description\": string, \"priority\": \"low\"|\"medium\"|\"high\", \
\"complexity\": \"small\"|\"medium\"|\"large\", \"has_ui\": boolean}\n\
bugs description should include how to reproduce. If everything passes, `bugs` is an empty \
array: [].\n\
`verdicts` is a JSON array with ONE entry per acceptance criterion of EVERY ticket in \
JUST SHIPPED — you must explicitly verify each one against the live app. Each entry \
exactly:\n\
{\"ac\": string, \"passed\": boolean, \"note\": string, \"route\": string, \"tests\": [string]}\n\
- `ac`: the EXACT acceptance-criterion text from the JUST SHIPPED block (match it \
word-for-word; do not paraphrase — the system marks that ticket's test case by this text).\n\
- `passed`: true only when you actually verified the behavior end-to-end on the deployed \
build; false when it fails or you could not verify it.\n\
- `note`: one line of concrete evidence — the command/request you ran and the actual \
response, or what blocked verification.\n\
- `route`: the URL path on the running app that demonstrates this criterion (e.g. \
\"/settings\"), or \"\" when none applies — it becomes the per-test-case screenshot.\n\
- `tests`: relative paths of the test files that demonstrate this criterion \
(e.g. \"crates/domain/tests/gate.rs\"), or [] when the evidence is an API \
request/response instead of a file-based test.";

/// Tech Writer — documents ONE verified feature in full for the team Wiki.
pub const DOCS: &str = "\
You are a world-class Tech Writer with TWO readers: a new teammate who must \
succeed with the feature without asking anyone, and an AGENT that will read \
this page later to change the code. Both are served by the same thing — \
precision. Every statement must be verified against the actual code; a wrong \
doc is worse than no doc.\n\n\
Write the page with EXACTLY this skeleton, in this order, using these headings \
verbatim so both readers can navigate every page the same way:\n\
`# <Area name>` — the area, not the ticket id.\n\
`**Keywords:** a, b, c` — 5-10 terms someone would actually search for.\n\
`## Overview` — what it does and who it is for, in 2-4 sentences.\n\
`## How it works` — the end-to-end flow, naming the real functions and types.\n\
`## Usage` — concrete steps, with example requests/responses or UI actions in \
fenced code blocks.\n\
`## Interface` — endpoints, parameters, payloads, CLI flags, or components, \
with their exact names.\n\
`## Configuration` — every setting that changes the behaviour, with defaults.\n\
`## Edge cases and limits` — what it deliberately does NOT do, and how it \
fails.\n\
`## Code map` — a bullet per file that implements this, as `path — what lives \
there`. This is how an agent finds the code without searching; get the paths \
right.\n\
`## Related` — other pages and tickets this connects to.\n\n\
Prefer exact identifiers over description (`verify_deploy_health()`, not \"the \
health checker\"). If a section genuinely does not apply, keep the heading and \
write one line saying why. Aim for a page a new teammate could rely on — \
several sections with real detail, not a paragraph.";

/// Product Designer authoring the project-level design system (once).
pub const DESIGN_SYSTEM: &str = "\
You are a world-class Product Designer establishing the project's design \
system — the shared visual language every UI feature must follow. Make it \
OPINIONATED and small: few tokens applied consistently beat many applied \
loosely; every choice should be concrete enough that a developer can apply it \
without asking (exact colors, exact radii, exact spacing).\n\n\
Respond with ONLY a JSON object, no prose, exactly:\n\
{\"principles\": string, \"palette\": [string], \"typography\": string, \
\"components\": [string]}\n\
`palette` items look like \"primary: cyan #0891B2\"; `components` items look \
like \"buttons: 8px radius, filled primary\".";

/// Render the design system as mandatory guidance appended to a DEV prompt for
/// a UI ticket. Empty when there is no populated design system.
#[must_use]
pub fn design_constraints(ds: Option<&crate::state::DesignSystem>) -> String {
    use std::fmt::Write as _;
    let Some(ds) = ds.filter(|d| d.is_populated()) else {
        return String::new();
    };
    let mut s = String::from("\n\nDESIGN SYSTEM (follow for all UI):\n");
    if !ds.principles.is_empty() {
        let _ = writeln!(s, "- Principles: {}", ds.principles);
    }
    if !ds.palette.is_empty() {
        let _ = writeln!(s, "- Palette: {}", ds.palette.join("; "));
    }
    if !ds.typography.is_empty() {
        let _ = writeln!(s, "- Typography: {}", ds.typography);
    }
    if !ds.components.is_empty() {
        let _ = writeln!(s, "- Components: {}", ds.components.join("; "));
    }
    s
}

/// Render the assigned host port as a deploy constraint appended to the DEV
/// prompt, so `docker-compose` publishes a non-conflicting port. Empty when no
/// port is assigned.
#[must_use]
pub fn deploy_constraints(deploy: &crate::config::DeployConfig) -> String {
    match deploy.host_port {
        Some(port) => format!(
            "\n\nDEPLOY CONSTRAINT (mandatory): publish the app on host port {port} in \
             docker-compose (e.g. \"{port}:<container-port>\"). Do not use any other host \
             port — it is reserved to avoid clashing with other projects on this host."
        ),
        None => String::new(),
    }
}

/// Compose a full system prompt for a role from the base and role sections.
#[must_use]
pub fn system_prompt(role_section: &str) -> String {
    format!("{BASE}\n\n{ENGINEERING_STANDARDS}\n\n{role_section}")
}

/// Char budget for the repo-map block injected into agent briefs. The tiered
/// map (see [`crate::repo_map`]) puts the tier-0 header — coverage line plus
/// the top-level area rollup — first, so this budget always buys whole-tree
/// directory coverage before any per-file detail.
const REPO_MAP_BLOCK_CHARS: usize = 3000;

/// A compact repo-map context block for code-touching agents: the file/symbol
/// layout so they locate code without exploring blind (fewer tool calls / tokens).
/// The map is compacted section-aware via [`crate::repo_map::prompt_slice`] —
/// the tier-0 header always survives and dropped sections are counted in a
/// footer — never a blind first-N-chars cut. Empty when the token-saver is off
/// or no map has been built yet.
pub async fn repo_map_block(
    files: Option<&dyn crate::ports::outbound::WorkspaceFilesPort>,
    work_dir: &std::path::Path,
    enabled: bool,
) -> String {
    let (true, Some(files)) = (enabled, files) else {
        return String::new();
    };
    let path = work_dir.join(".coxagent").join("REPO_MAP.md");
    let Some(map) = files.read(&path).await else {
        return String::new();
    };
    let compact = crate::repo_map::prompt_slice(&map, REPO_MAP_BLOCK_CHARS);
    format!(
        "\n\n## Repo map — files & their symbols (use this to locate code fast, \
         don't re-scan the whole tree)\n{compact}\n"
    )
}

/// A TICKET-SCOPED slice of the code graph: the symbols/files most relevant to
/// `query` (title + design), grouped by file, plus who calls the top hits — so
/// DEV/SA jump straight to the right code instead of exploring, and see the
/// blast radius before changing it. Empty when no graph is built yet.
pub async fn focus_block(
    files: Option<&dyn crate::ports::outbound::WorkspaceFilesPort>,
    work_dir: &std::path::Path,
    query: &str,
) -> String {
    use std::fmt::Write as _;
    let Some(files) = files else {
        return String::new();
    };
    let Some(g) = crate::codegraph::CodeGraph::load(files, work_dir).await else {
        return String::new();
    };
    let hits = g.relevance_search(query, 12);
    if hits.is_empty() {
        return String::new();
    }
    // Group by file, preserving relevance order of first appearance.
    let mut by_file: Vec<(String, Vec<String>)> = Vec::new();
    for s in &hits {
        match by_file.iter_mut().find(|(f, _)| *f == s.file) {
            Some((_, syms)) => syms.push(s.name.clone()),
            None => by_file.push((s.file.clone(), vec![s.name.clone()])),
        }
    }
    let mut out = String::from("\n\nLIKELY RELEVANT CODE (from the code graph — start here):\n");
    for (f, syms) in by_file.iter().take(6) {
        let _ = writeln!(out, "- {f}: {}", syms.join(", "));
    }
    // Signatures of the top hits — often enough to orient without opening the
    // file at all. One read per file, capped hard.
    let mut file_cache: Vec<(String, Vec<String>)> = Vec::new();
    let mut sigs = String::new();
    for s in hits.iter().take(8) {
        let idx = file_cache
            .iter()
            .position(|(f, _)| *f == s.file)
            .unwrap_or({
                let content = files
                    .read(&work_dir.join(&s.file))
                    .await
                    .unwrap_or_default();
                file_cache.push((s.file.clone(), content.lines().map(str::to_owned).collect()));
                file_cache.len() - 1
            });
        let lines = &file_cache[idx].1;
        if let Some(line) = s.line.checked_sub(1).and_then(|i| lines.get(i)) {
            let sig: String = line.trim().chars().take(110).collect();
            if !sig.is_empty() {
                let _ = writeln!(sigs, "- {} ({}:{}): {sig}", s.name, s.file, s.line);
            }
        }
    }
    if !sigs.is_empty() {
        out.push_str("Signatures:\n");
        out.push_str(&sigs);
    }
    // Blast radius: callers of the top two hits.
    for s in hits.iter().take(2) {
        let callers = g.callers(&s.name);
        if !callers.is_empty() {
            let list: Vec<String> = callers
                .iter()
                .take(5)
                .map(|(caller, file, line)| format!("{caller} ({file}:{line})"))
                .collect();
            let _ = writeln!(out, "- callers of `{}`: {}", s.name, list.join(", "));
        }
    }
    out.chars().take(1800).collect()
}

/// Distinctive terms shared by `query` and `text`, as a crude relevance score.
/// Short words carry no signal and are dropped, so "the fix" doesn't match
/// everything in the repo.
fn overlap_score(query_terms: &[String], text: &str) -> usize {
    let terms = crate::codegraph::tokenize(text);
    query_terms
        .iter()
        .filter(|q| q.len() > 3 && terms.iter().any(|t| t == *q))
        .count()
}

/// Query terms worth matching on.
fn query_terms(query: &str) -> Vec<String> {
    let mut t = crate::codegraph::tokenize(query);
    t.retain(|w| w.len() > 3);
    t.sort();
    t.dedup();
    t
}

/// What a tester would open first: the suites that already exist and the API
/// surface they cover. Without it the TEST role re-invents coverage that is
/// already there, or reports "bugs" against endpoints it guessed at.
///
/// A bounded walk of the repo — no LLM call, and `target/`, `node_modules/`
/// and friends are skipped, so the cost is a directory read.
pub async fn test_surface_block(
    files: Option<&dyn crate::ports::outbound::WorkspaceFilesPort>,
    work_dir: &std::path::Path,
) -> String {
    use std::fmt::Write as _;
    const SKIP: &[&str] = &[
        "target",
        "node_modules",
        ".git",
        "dist",
        "build",
        "vendor",
        ".venv",
    ];
    let Some(files) = files else {
        return String::new();
    };
    let mut tests: Vec<(String, usize)> = Vec::new();
    let mut routes: Vec<String> = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = vec![work_dir.to_path_buf()];
    let mut files_read = 0_usize;
    while let Some(dir) = stack.pop() {
        for sub in files.list_dirs(&dir).await {
            let name = sub
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !name.starts_with('.') && !SKIP.contains(&name.as_str()) && stack.len() < 200 {
                stack.push(sub);
            }
        }
        for meta in files.list(&dir).await {
            let path = meta.path;
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            // Budget the walk: a big repo must not turn one prompt into a
            // full-text scan.
            if files_read >= 400 {
                continue;
            }
            let is_source = [".rs", ".ts", ".tsx", ".js", ".go", ".py"]
                .iter()
                .any(|e| name.ends_with(e));
            if !is_source {
                continue;
            }
            let Some(text) = files.read(&path).await else {
                continue;
            };
            files_read += 1;
            let rel = path
                .strip_prefix(work_dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let count = text.matches("#[test]").count()
                + text.matches("#[tokio::test]").count()
                + text.matches("def test_").count()
                + text.matches("func Test").count()
                + text.matches("it(").count();
            if count > 0 {
                tests.push((rel.clone(), count));
            }
            if routes.len() < 24 {
                for line in text.lines() {
                    let t = line.trim();
                    if t.starts_with(".route(") || t.starts_with("@app.route") {
                        let entry: String = t.chars().take(100).collect();
                        routes.push(entry);
                        if routes.len() >= 24 {
                            break;
                        }
                    }
                }
            }
        }
    }
    if tests.is_empty() && routes.is_empty() {
        return String::new();
    }
    tests.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let mut out = String::new();
    if !tests.is_empty() {
        let total: usize = tests.iter().map(|(_, n)| n).sum();
        let _ = writeln!(
            out,
            "\n\nEXISTING TESTS ({total} across {} files) — cover what these do NOT, and do not \
             duplicate them:",
            tests.len()
        );
        for (f, n) in tests.iter().take(8) {
            let _ = writeln!(out, "- {f} ({n})");
        }
    }
    if !routes.is_empty() {
        out.push_str("\nAPI SURFACE actually registered in the code — test these, not guesses:\n");
        for r in routes.iter().take(12) {
            let _ = writeln!(out, "- {r}");
        }
    }
    out.chars().take(1600).collect()
}

/// The instruction that lets an agent ask instead of guess, plus any answer it
/// already has. Modelled on how the work actually gets done between people: a
/// developer who cannot tell what the requirement means asks the BA; a BA who
/// does not know what the product already does asks the SA to read the code and
/// report back. Guessing is the expensive option — it fails a gate three
/// attempts later, having taught nobody anything.
/// Every author an agent (or the system) posts under. Anyone else commenting
/// is a person.
///
/// Steering used to match the literal author `"USER"`, which only the
/// account-less open mode produces: on any hub with logins the comment is
/// authored with the real username. So every comment a signed-in person wrote
/// was stored, shown in the dialog, and never once read by an agent — the
/// feature looked present and did nothing.
const AGENT_AUTHORS: &[&str] = &[
    "BA",
    "SA",
    "PD",
    "DEV",
    "DEV-BUG",
    "DEV-FEATURE",
    "DOCS",
    "TEST",
    "QA",
    "SM",
    "PO",
    "SYSTEM",
];

/// Whether `author` is a person rather than one of the agents.
#[must_use]
pub fn is_human_author(author: &str) -> bool {
    let a = author.trim();
    // `USER` is what open mode writes for the operator — a person.
    a == "USER" || !AGENT_AUTHORS.iter().any(|r| r.eq_ignore_ascii_case(a))
}

/// What the humans said on this ticket, as instructions.
///
/// A person who reads a design or a proposal and thinks "not what I meant"
/// reaches for the comment box. That only steered DEV: SA kept designing and
/// BA kept proposing without ever seeing the note, so the correction had to be
/// re-typed as a rejection reason or lost. Every role that writes something a
/// human reviews now reads the comments on it first.
///
/// Newest first, capped — a long thread should not crowd out the ticket.
#[must_use]
pub fn human_steering_block(state: &crate::state::ProjectState, ticket: &str) -> String {
    let notes: Vec<String> = state
        .comments
        .iter()
        .filter(|c| is_human_author(&c.author) && c.ticket.as_deref() == Some(ticket))
        .rev()
        .take(3)
        .map(|c| c.body.chars().take(400).collect::<String>())
        .collect();
    if notes.is_empty() {
        return String::new();
    }
    format!(
        "\n\nHUMAN STEERING on this ticket (newest first — follow it):\n- {}",
        notes.join("\n- ")
    )
}

/// A portable, engine-agnostic brief of what EARLIER work on this ticket found
/// — the ticket's own journal, readable by any role or engine. It carries the
/// context a session resume cannot (a resume is one engine's private memory;
/// this survives an engine switch, a failover, and a role handoff SA↔DEV↔TEST).
#[must_use]
pub fn ticket_brief_block(state: &crate::state::ProjectState, ticket: &str) -> String {
    let notes = match state.ticket_journal.get(ticket) {
        Some(n) if !n.is_empty() => n,
        _ => return String::new(),
    };
    // Newest last (the journal appends), capped so the brief stays a briefing.
    let recent: Vec<&String> = notes.iter().rev().take(8).collect();
    let mut lines: Vec<&String> = recent.into_iter().collect();
    lines.reverse();
    format!(
        "\n\nPRIOR WORK ON THIS TICKET (earlier runs, any role/engine — build on it, \
         do not repeat it):\n- {}",
        lines
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n- ")
    )
}

/// Appended to a ticket-scoped task so agents leave durable, engine-agnostic
/// notes for whoever works this ticket next. Kept in the TASK prompt (not the
/// cached system prompt) since it rides alongside per-ticket content.
pub const BRIEF_PROTOCOL: &str =
    "\n\nHANDOFF: if you learned something the next agent on this ticket must know — a \
     gotcha, a decision and why, an approach that failed — end with a line starting \
     `BRIEF:`. It becomes durable ticket memory read by the next role/engine.";

/// Pull any `BRIEF:`-prefixed lines an agent left in its output — a note to the
/// NEXT role/engine that works this ticket — so discoveries persist as durable,
/// engine-agnostic memory instead of dying with the run's session.
#[must_use]
pub fn extract_brief_notes(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|l| l.trim().strip_prefix("BRIEF:"))
        .map(|n| n.trim().chars().take(400).collect::<String>())
        .filter(|n| !n.is_empty())
        .collect()
}

/// CXA-F305: [`extract_brief_notes`] composed with the injection screen — a
/// note that trips the screen is withheld from the journal (it would otherwise
/// replay into every future run as PRIOR WORK) and reported so the run can
/// flag it to the operator as an `injection_flagged` item.
#[must_use]
pub fn extract_brief_notes_screened(stdout: &str) -> crate::brief_screening::ScreenedNotes {
    crate::brief_screening::screen_notes(extract_brief_notes(stdout))
}

#[must_use]
pub fn ask_protocol_block(state: &crate::state::ProjectState, ticket: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let answered = state.answered_questions(ticket);
    if !answered.is_empty() {
        out.push_str(
            "\n\nANSWERS to what you asked earlier — treat these as the requirement, and do \
             what the ACTION line says:\n",
        );
        for q in answered.iter().rev().take(3) {
            let _ = writeln!(
                out,
                "- You asked {}: {}\n  {} replied: {}",
                q.to, q.body, q.to, q.answer
            );
        }
    }
    if let Some(open) = state.open_question(ticket) {
        // Do not let it ask twice into the void.
        let _ = write!(
            out,
            "\n\nYou already asked {} \"{}\" and no answer has come back yet. Do NOT ask again: \
             make the smallest safe progress you can, or stop and say what is blocked.",
            open.to, open.body
        );
        return out;
    }
    out.push_str(
        "\n\nIF YOU WOULD HAVE TO GUESS, do not guess — ask the role that owns the answer. End \
         your output with ONE line, exactly:\n\
         `ASK BA: <question>`   for what the ticket or the business actually means\n\
         `ASK SA: <question>`   for how the system works or how this should be built\n\
         Ask only when the answer would change what you build, make it specific and answerable, \
         and ask at most one question. Anything you can settle by reading the code or the docs \
         yourself is not a question — settle it.",
    );
    out
}

/// Everything the organisation has already written down about a subject: the
/// team's own wiki, the repo's docs, and closed tickets with the same symptom.
///
/// Every role needs this, not just the developer. The SA is supposed to be the
/// encyclopedia — it cannot arbitrate a design it has no memory of — and the
/// BA, PO and PD cannot write a sound requirement without knowing what the
/// product already does. `exclude` keeps a ticket from being offered its own
/// history; pass an empty string when there is no ticket in hand.
pub async fn knowledge_block(
    files: Option<&dyn crate::ports::outbound::WorkspaceFilesPort>,
    docs: &[crate::state::DocPage],
    tickets: &[coxagent_domain::Ticket],
    work_dir: &std::path::Path,
    query: &str,
    exclude: &str,
) -> String {
    if query.trim().is_empty() {
        return String::new();
    }
    format!(
        "{}{}{}",
        wiki_block(docs, query),
        repo_docs_block(files, work_dir, query).await,
        prior_fix_block(tickets, query, exclude),
    )
}

/// The team's OWN wiki, matched to this ticket. The DOCS agent writes a page
/// after every feature and, until now, nobody ever read one back: the team
/// documented itself and then re-derived the same knowledge from scratch on
/// the next ticket. This is the shelf those pages sit on.
#[must_use]
pub fn wiki_block(docs: &[crate::state::DocPage], query: &str) -> String {
    use std::fmt::Write as _;
    let terms = query_terms(query);
    if terms.is_empty() || docs.is_empty() {
        return String::new();
    }
    let mut scored: Vec<(usize, &crate::state::DocPage)> = docs
        .iter()
        .map(|d| {
            // A title hit is worth more than a body mention.
            let score = overlap_score(&terms, &d.title) * 3 + overlap_score(&terms, &d.body);
            (score, d)
        })
        .filter(|(s, _)| *s > 0)
        .collect();
    scored.sort_by_key(|(score, ..)| std::cmp::Reverse(*score));
    // A long ticket shares a word or two with almost every page; keeping those
    // spends 400 characters of the brief on a page nobody needed. Demand a real
    // hit AND a score in the same league as the best one.
    let floor = scored.first().map_or(0, |(s, _)| (*s / 3).max(2));
    scored.retain(|(s, _)| *s >= floor);
    if scored.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\nTEAM WIKI — pages this team already wrote about this area. Read them before \
         re-deriving anything:\n",
    );
    for (_, d) in scored.iter().take(3) {
        let where_ = if d.folder.is_empty() {
            String::new()
        } else {
            format!(" ({})", d.folder)
        };
        let body: String = d
            .body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .take(6)
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(400)
            .collect();
        let _ = writeln!(out, "- {}{where_}: {body}", d.title);
    }
    out.chars().take(1500).collect()
}

/// Tickets already solved that look like this one. A bug whose symptom matches
/// something the team fixed three sprints ago should start from that fix, not
/// from a blank page — the way a person would say "we've seen this before".
#[must_use]
pub fn prior_fix_block(tickets: &[coxagent_domain::Ticket], query: &str, exclude: &str) -> String {
    use std::fmt::Write as _;
    let terms = query_terms(query);
    if terms.is_empty() {
        return String::new();
    }
    let mut scored: Vec<(usize, &coxagent_domain::Ticket)> = tickets
        .iter()
        .filter(|t| {
            t.id().to_string() != exclude
                && matches!(
                    t.status(),
                    coxagent_domain::Status::Fixed
                        | coxagent_domain::Status::Done
                        | coxagent_domain::Status::Documented
                        | coxagent_domain::Status::Verified
                )
        })
        .map(|t| {
            let score =
                overlap_score(&terms, t.title()) * 3 + overlap_score(&terms, t.description());
            (score, t)
        })
        // Two shared distinctive terms before we claim a resemblance.
        .filter(|(s, _)| *s >= 2)
        .collect();
    scored.sort_by_key(|(score, ..)| std::cmp::Reverse(*score));
    if scored.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\nALREADY SOLVED — closed tickets with the same symptom. Check what was done there \
         first; if this is a regression of one, say so:\n",
    );
    for (_, t) in scored.iter().take(3) {
        let desc: String = t.description().chars().take(200).collect();
        let _ = writeln!(out, "- {} [{:?}] {}: {desc}", t.id(), t.status(), t.title());
    }
    out.chars().take(1200).collect()
}

/// The repo's own written word — README, CLAUDE.md/AGENTS.md, `docs/*.md` —
/// narrowed to what this ticket is about. The rules a project writes down for
/// its contributors apply to the agent contributor too.
pub async fn repo_docs_block(
    files: Option<&dyn crate::ports::outbound::WorkspaceFilesPort>,
    work_dir: &std::path::Path,
    query: &str,
) -> String {
    use std::fmt::Write as _;
    let Some(files) = files else {
        return String::new();
    };
    let terms = query_terms(query);
    if terms.is_empty() {
        return String::new();
    }
    let mut candidates: Vec<std::path::PathBuf> = ["README.md", "CLAUDE.md", "AGENTS.md"]
        .iter()
        .map(|n| work_dir.join(n))
        .collect();
    // One level of docs/ is enough; deep trees are the code graph's job.
    for meta in files
        .list(&work_dir.join("docs"))
        .await
        .into_iter()
        .take(40)
    {
        if meta.path.extension().is_some_and(|x| x == "md") {
            candidates.push(meta.path);
        }
    }
    let mut scored: Vec<(usize, String, String)> = Vec::new();
    for f in &candidates {
        let Some(text) = files.read(f).await else {
            continue;
        };
        let name = f
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Score per section so a huge README doesn't win on volume alone.
        for section in text.split("\n## ") {
            let score = overlap_score(&terms, section);
            if score >= 2 {
                let body: String = section
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .take(8)
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(400)
                    .collect();
                scored.push((score, name.clone(), body));
            }
        }
    }
    scored.sort_by_key(|(score, ..)| std::cmp::Reverse(*score));
    if scored.is_empty() {
        return String::new();
    }
    let mut out =
        String::from("\n\nPROJECT DOCS that cover this area — they are the rules here:\n");
    for (_, name, body) in scored.iter().take(3) {
        let _ = writeln!(out, "- {name}: {body}");
    }
    out.chars().take(1500).collect()
}

/// What has already been done to the code this ticket is about — the reflex a
/// human brings to unfamiliar code and an agent has to be handed: before
/// touching a file, look at its recent history. A previous attempt at the same
/// bug, a refactor that introduced it, or a revert all live here, and none of
/// them are visible in the ticket text.
///
/// Deterministic and cheap: the code graph picks the files, `git log` reports
/// them. Empty when the graph has no opinion or the directory is not a repo.
pub async fn history_block(
    files: Option<&dyn crate::ports::outbound::WorkspaceFilesPort>,
    git: Option<&dyn crate::ports::outbound::GitPort>,
    work_dir: &std::path::Path,
    query: &str,
) -> String {
    use std::fmt::Write as _;
    let (Some(files), Some(git)) = (files, git) else {
        return String::new();
    };
    let Some(g) = crate::codegraph::CodeGraph::load(files, work_dir).await else {
        return String::new();
    };
    let mut files: Vec<String> = Vec::new();
    for s in g.relevance_search(query, 12) {
        if !files.contains(&s.file) {
            files.push(s.file.clone());
        }
        if files.len() == 4 {
            break;
        }
    }
    if files.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for f in &files {
        let (ok, log) = git
            .raw(
                work_dir,
                &[
                    "log",
                    "-n",
                    "3",
                    "--no-merges",
                    "--date=short",
                    "--format=%h %ad %s",
                    "--",
                    f,
                ],
            )
            .await;
        if !ok {
            continue;
        }
        let lines: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
        if lines.is_empty() {
            continue;
        }
        let _ = writeln!(out, "- {f}:");
        for l in lines.iter().take(3) {
            let entry: String = l.chars().take(120).collect();
            let _ = writeln!(out, "    {entry}");
        }
    }
    if out.is_empty() {
        return String::new();
    }
    format!(
        "\n\nRECENT HISTORY of the files above — check whether this was already \
         attempted or caused by one of these before you change anything:\n{out}"
    )
    .chars()
    .take(1200)
    .collect()
}

/// Where cross-project ("hub") lessons live: one markdown bullet per lesson,
/// shared by EVERY project this user's hub runs — so what one team learns
/// ("rust:1.79 base image breaks edition2024") benefits the next project too.
/// Override with `COXAGENT_HUB_LESSONS_PATH`.
#[must_use]
pub fn hub_lessons_path() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("COXAGENT_HUB_LESSONS_PATH") {
        return std::path::PathBuf::from(p);
    }
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join("CoXAgent")
        .join("hub_lessons.md")
}

/// Record a lesson into the hub-wide store (dedup, newest last, capped at 30
/// so the block stays prompt-sized). Best-effort: IO errors are swallowed —
/// a lesson lost beats a crashed retro.
///
/// CXA-F306: eviction is no longer silent loss. Entries pushed past the cap
/// move to the sidecar retention shelf (`hub_lessons::retain_evicted`) with
/// any recurrence history they carry, so a lesson evicted today that matches
/// a later incident is re-surfaced instead of gone.
pub async fn record_hub_lesson(
    files: Option<&dyn crate::ports::outbound::WorkspaceFilesPort>,
    lesson: &str,
) {
    let (Some(files), lesson) = (files, lesson.trim()) else {
        return;
    };
    if lesson.is_empty() {
        return;
    }
    // CXA-F305: a lesson is agent-derived text replayed into EVERY project's
    // brief — an injection-shaped line never enters the hub-wide store (the
    // drop is silent, per the ticket's write-path contract).
    let Some(lesson) = crate::brief_screening::screen_brief_note(lesson).cleaned else {
        tracing::debug!("hub lesson withheld by brief screening at write time");
        return;
    };
    let path = hub_lessons_path();
    let mut lines: Vec<String> = files
        .read(&path)
        .await
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let entry = format!("- {lesson}");
    if lines.iter().any(|l| l == &entry) {
        return;
    }
    lines.push(entry);
    let overflow = lines.len().saturating_sub(30);
    if overflow > 0 {
        let evicted: Vec<String> = lines.drain(0..overflow).collect();
        // Retain, don't lose: the evicted bullets (and their recurrence
        // history, if any) stay matchable for later incidents (CXA-F306 AC4).
        crate::hub_lessons::retain_evicted(
            Some(files),
            &path,
            &evicted,
            &crate::state::now_rfc3339(),
        )
        .await;
    }
    let _ = files.write(&path, &(lines.join("\n") + "\n")).await;
}

/// Prompt block with the most recent hub-wide lessons (max 8). Empty when the
/// store is empty/absent. With `screening` on (CXA-F305), each line is
/// screened before it enters any project's brief: a lesson that trips the
/// screen is withheld until reviewed, and the withholding is announced in the
/// block itself.
pub async fn hub_lessons_block(
    files: Option<&dyn crate::ports::outbound::WorkspaceFilesPort>,
    screening: bool,
) -> String {
    let Some(files) = files else {
        return String::new();
    };
    let text = files.read(&hub_lessons_path()).await.unwrap_or_default();
    let recent: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(8)
        .collect();
    if recent.is_empty() {
        return String::new();
    }
    let mut kept: Vec<String> = Vec::new();
    let mut withheld = 0_usize;
    for l in recent.iter().rev() {
        if !screening {
            kept.push((*l).to_owned());
            continue;
        }
        let screened = crate::brief_screening::screen_brief_note(l.trim());
        if let Some(clean) = screened.cleaned {
            kept.push(clean);
        } else {
            withheld += 1;
            tracing::debug!(
                reasons = %crate::brief_screening::reasons_label(&screened.reasons),
                "hub lesson withheld by brief screening"
            );
        }
    }
    if withheld == recent.len() {
        // Everything withheld: keep the block alive so the withholding itself
        // is announced rather than the lessons silently vanishing.
        return format!(
            "\n\n## Lessons from OTHER projects on this hub (hard-won — honour them):\n\
             (all {withheld} lesson(s) withheld by brief screening — injection-shaped, \
             pending security review.)\n"
        );
    }
    let mut out =
        String::from("\n\n## Lessons from OTHER projects on this hub (hard-won — honour them):\n");
    for l in &kept {
        out.push_str(l);
        out.push('\n');
    }
    if withheld > 0 {
        use std::fmt::Write as _;
        let _ = writeln!(
            out,
            "({withheld} lesson(s) withheld by brief screening — injection-shaped, pending \
             security review.)"
        );
    }
    out
}

/// Relevance-ranked team memory: score each decision/lesson by word overlap
/// with the current task (title + technical approach) and keep only the most
/// relevant — at scale, 100 stored lessons must not become 100 lines of prompt
/// noise. Recency breaks ties; falls back to recent items when nothing scores.
#[must_use]
pub fn team_memory_block_relevant(decisions: &[String], lessons: &[String], query: &str) -> String {
    let rank = |items: &[String], keep: usize| -> Vec<String> {
        let qwords: Vec<String> = query
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 3)
            .map(str::to_owned)
            .collect();
        let mut scored: Vec<(usize, usize, &String)> = items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let low = item.to_lowercase();
                let hits = qwords.iter().filter(|w| low.contains(w.as_str())).count();
                (hits, i, item)
            })
            .collect();
        // Highest score first; among equals, most recent (highest index) first.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        scored
            .into_iter()
            .take(keep)
            .map(|(_, _, item)| item.clone())
            .collect()
    };
    let decisions_kept = rank(decisions, 8);
    let lessons_kept = rank(lessons, 6);
    if decisions_kept.is_empty() && lessons_kept.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\n## Team memory — honour these (decisions the team already made + \
         lessons learned):\n",
    );
    for d in &decisions_kept {
        out.push_str("- [decision] ");
        out.push_str(d);
        out.push('\n');
    }
    for l in &lessons_kept {
        out.push_str("- [lesson] ");
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// Fold the team's durable memory — decisions/conventions plus retro lessons —
/// into a prompt block so every agent stays consistent with what's been decided
/// and learned, instead of re-deriving (and contradicting) each call. Empty when
/// there's nothing yet.
#[must_use]
pub fn team_memory_block(decisions: &[String], lessons: &[String]) -> String {
    if decisions.is_empty() && lessons.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\n## Team memory — honour these (decisions the team already made + \
         lessons learned):\n",
    );
    for d in decisions.iter().rev().take(12).rev() {
        out.push_str("- [decision] ");
        out.push_str(d);
        out.push('\n');
    }
    for l in lessons.iter().rev().take(6).rev() {
        out.push_str("- [lesson] ");
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// Render architecture stack rules as prompt constraints, so DEV/SA follow the
/// stack proactively (governance also enforces it reactively).
#[must_use]
pub fn stack_constraints(rules: &[crate::conformance::StackRule]) -> String {
    use std::fmt::Write as _;
    if rules.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n\nARCHITECTURE CONSTRAINTS (mandatory):\n");
    for r in rules {
        let _ = write!(s, "- `{}` MUST be {}", r.area, r.language);
        if !r.require_any.is_empty() {
            let _ = write!(s, " (include {})", r.require_any.join("/"));
        }
        if !r.forbid_ext.is_empty() {
            let _ = write!(s, "; never use {} files here", r.forbid_ext.join("/"));
        }
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::{deploy_constraints, design_constraints, extract_brief_notes, repo_map_block};
    use crate::config::DeployConfig;
    use crate::state::DesignSystem;

    #[test]
    fn brief_notes_are_pulled_from_the_marker_lines_only() {
        let out = "did the work\nBRIEF: the retry lives in failover.rs, not routing\n\
                   some log\n  BRIEF:  trailing space trimmed  \nBRIEF:";
        let got = extract_brief_notes(out);
        assert_eq!(
            got,
            vec![
                "the retry lives in failover.rs, not routing".to_owned(),
                "trailing space trimmed".to_owned(),
            ],
            "only non-empty BRIEF: lines, trimmed"
        );
    }

    #[test]
    fn ticket_brief_block_renders_prior_work_or_nothing() {
        let mut st = crate::state::ProjectState::default();
        assert!(
            super::ticket_brief_block(&st, "COX-1").is_empty(),
            "no journal → no block"
        );
        st.journal_note("COX-1", "SA: split into two");
        let block = super::ticket_brief_block(&st, "COX-1");
        assert!(block.contains("PRIOR WORK ON THIS TICKET"));
        assert!(block.contains("split into two"));
    }

    #[test]
    fn a_signed_in_person_s_comment_is_steering_too() {
        // The bug this guards: steering matched the literal author "USER",
        // which only account-less open mode writes. On a hub with logins every
        // comment is authored with the real username, so no human note ever
        // reached an agent.
        let mut state = crate::state::ProjectState::default();
        state.post_comment("luffy", "drop the retry loop", Some("F001".to_owned()));
        state.post_comment("SA", "designed it", Some("F001".to_owned()));
        state.post_comment("DEV-BUG", "fixed it", Some("F001".to_owned()));

        let out = super::human_steering_block(&state, "F001");
        assert!(out.contains("drop the retry loop"), "{out}");
        assert!(!out.contains("designed it"));
        assert!(!out.contains("fixed it"));

        assert!(super::is_human_author("USER"));
        assert!(super::is_human_author("luffy"));
        assert!(!super::is_human_author("BA"));
        assert!(
            !super::is_human_author("dev-feature"),
            "case must not matter"
        );
    }

    #[test]
    fn human_steering_carries_only_this_ticket_s_human_notes() {
        let mut state = crate::state::ProjectState::default();
        state.post_comment(
            "USER",
            "use the existing auth port",
            Some("F001".to_owned()),
        );
        state.post_comment("SA", "designed it", Some("F001".to_owned()));
        state.post_comment("USER", "different ticket", Some("F002".to_owned()));

        let out = super::human_steering_block(&state, "F001");
        assert!(out.contains("existing auth port"));
        assert!(
            !out.contains("designed it"),
            "agent chatter is not steering"
        );
        assert!(!out.contains("different ticket"), "other tickets stay out");

        assert!(
            super::human_steering_block(&state, "F404").is_empty(),
            "no notes means no block at all, not an empty heading"
        );
    }

    #[tokio::test]
    async fn repo_map_block_gated_and_present() {
        let fs = &crate::test_fs::StdFsFiles;
        let dir = std::env::temp_dir().join(format!("rmb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".coxagent")).expect("mk");
        std::fs::write(
            dir.join(".coxagent/REPO_MAP.md"),
            "# Repo map\n## src/x.rs (rust)\n  fn go",
        )
        .expect("w");
        assert!(
            repo_map_block(Some(fs), &dir, false).await.is_empty(),
            "off = empty"
        );
        let on = repo_map_block(Some(fs), &dir, true).await;
        assert!(on.contains("src/x.rs") && on.contains("Repo map"));
        // Missing map = empty even when enabled.
        assert!(
            repo_map_block(Some(fs), std::path::Path::new("/no/such/dir"), true)
                .await
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deploy_constraint_names_the_assigned_port() {
        assert!(deploy_constraints(&DeployConfig {
            host_port: None,
            enabled: true,
            ..DeployConfig::default()
        })
        .is_empty());
        let out = deploy_constraints(&DeployConfig {
            host_port: Some(8123),
            enabled: true,
            ..DeployConfig::default()
        });
        assert!(out.contains("8123"));
        assert!(out.contains("docker-compose"));
    }

    #[test]
    fn empty_design_system_renders_nothing() {
        assert!(design_constraints(None).is_empty());
        assert!(design_constraints(Some(&DesignSystem::default())).is_empty());
    }

    #[test]
    fn populated_design_system_renders_all_sections() {
        let ds = DesignSystem {
            principles: "calm".to_owned(),
            palette: vec!["primary: cyan #0891B2".to_owned()],
            typography: "Inter".to_owned(),
            components: vec!["buttons: 8px radius".to_owned()],
        };
        let out = design_constraints(Some(&ds));
        assert!(out.contains("DESIGN SYSTEM"));
        assert!(out.contains("calm"));
        assert!(out.contains("cyan #0891B2"));
        assert!(out.contains("Inter"));
        assert!(out.contains("8px radius"));
    }

    #[tokio::test]
    async fn hub_lessons_record_dedup_cap_and_block() {
        let fs = &crate::test_fs::StdFsFiles;
        let dir = std::env::temp_dir().join(format!("cox-hub-lessons-{}", std::process::id()));
        let file = dir.join("hub_lessons.md");
        std::env::set_var("COXAGENT_HUB_LESSONS_PATH", &file);
        let _ = std::fs::remove_file(&file);
        for i in 0..35 {
            super::record_hub_lesson(Some(fs), &format!("lesson {i}")).await;
        }
        super::record_hub_lesson(Some(fs), "lesson 34").await; // duplicate — ignored
        let text = std::fs::read_to_string(&file).expect("written");
        let n = text.lines().count();
        assert_eq!(n, 30, "capped at 30");
        assert!(!text.contains("lesson 0"), "oldest evicted");
        let block = super::hub_lessons_block(Some(fs), true).await;
        assert!(block.contains("OTHER projects"));
        assert!(block.contains("lesson 34"));
        assert_eq!(block.matches("- lesson").count(), 8, "block caps at 8");
        std::env::remove_var("COXAGENT_HUB_LESSONS_PATH");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relevant_memory_ranks_by_overlap() {
        let lessons = vec![
            "always pin the docker base image version".to_owned(),
            "webhooks need a 5s timeout".to_owned(),
            "keep dashboard charts theme-aware".to_owned(),
        ];
        let block = super::team_memory_block_relevant(&[], &lessons, "fix docker image build");
        let first = block.lines().find(|l| l.starts_with("- [lesson]")).unwrap();
        assert!(
            first.contains("docker base image"),
            "best match first: {first}"
        );
    }

    #[test]
    fn relevant_memory_empty_when_no_memory() {
        assert!(super::team_memory_block_relevant(&[], &[], "anything").is_empty());
    }
}

#[cfg(test)]
mod history_block_tests {
    use super::history_block;

    struct NoGit;

    #[async_trait::async_trait]
    impl crate::ports::outbound::GitPort for NoGit {
        async fn is_repo(&self, _: &std::path::Path) -> bool {
            false
        }
        async fn current_branch(&self, _: &std::path::Path) -> Result<String, crate::PortError> {
            Ok(String::new())
        }
        async fn checkout_branch(
            &self,
            _: &std::path::Path,
            _: &str,
        ) -> Result<(), crate::PortError> {
            Ok(())
        }
        async fn commit_all(
            &self,
            _: &std::path::Path,
            _: &str,
            _: &crate::ports::outbound::GitAuthor,
        ) -> Result<Option<String>, crate::PortError> {
            Ok(None)
        }
        async fn push(&self, _: &std::path::Path, _: &str) -> Result<(), crate::PortError> {
            Ok(())
        }
        async fn sync_base(
            &self,
            _: &std::path::Path,
            _: &str,
        ) -> Result<crate::ports::outbound::SyncBase, crate::PortError> {
            Ok(crate::ports::outbound::SyncBase::UpToDate)
        }
        async fn abort_merge(&self, _: &std::path::Path) -> Result<(), crate::PortError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn stays_silent_without_ports_or_a_code_graph() {
        // No ports (tests, git-less projects): the block must stay silent
        // rather than guess at files — a confidently wrong history is worse
        // than none.
        let dir = std::env::temp_dir().join(format!("histblock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mkdir");
        assert!(history_block(None, None, &dir, "verify_token")
            .await
            .is_empty());
        // Ports present but no persisted code graph: still silent.
        assert!(history_block(
            Some(&crate::test_fs::StdFsFiles),
            Some(&NoGit),
            &dir,
            "verify_token"
        )
        .await
        .is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod knowledge_block_tests {
    use super::{prior_fix_block, repo_docs_block, wiki_block};
    use crate::state::DocPage;
    use coxagent_domain::{Complexity, Priority, Role, Status, Ticket, TicketId, TicketType};

    fn page(title: &str, body: &str) -> DocPage {
        DocPage {
            id: title.to_owned(),
            folder: "Technical".to_owned(),
            category: "technical".to_owned(),
            title: title.to_owned(),
            body: body.to_owned(),
            updated_at: String::new(),
            updated_by: String::new(),
        }
    }

    #[test]
    fn a_weak_wiki_match_is_not_worth_the_room_it_takes() {
        // Real data: a long ticket overlaps a word or two with nearly every
        // page, so "scored above zero" is not the same as "worth reading".
        let docs = vec![
            page(
                "Deploy health gate",
                "The gate polls the deploy health endpoint until the port binds on deploy.",
            ),
            page(
                "Chat theming",
                "Deploy notes are irrelevant here; colors only.",
            ),
        ];
        let out = wiki_block(&docs, "deploy health gate port binds during deploy");
        assert!(out.contains("Deploy health gate"));
        assert!(
            !out.contains("Chat theming"),
            "a page that shares one incidental word must not buy prompt space"
        );
    }

    #[test]
    fn the_wiki_answers_only_when_it_has_something_to_say() {
        let docs = vec![
            page(
                "Deploy health gate",
                "The gate polls the health endpoint until the port binds.",
            ),
            page("Chat theming", "Colors and spacing for the chat pane."),
        ];
        let out = wiki_block(&docs, "health gate never binds the port on deploy");
        assert!(out.contains("Deploy health gate"), "matching page surfaces");
        assert!(!out.contains("Chat theming"), "unrelated page stays out");
        assert!(
            wiki_block(&docs, "quantum teleportation").is_empty(),
            "no match must produce no section, not an empty heading"
        );
        assert!(wiki_block(&[], "health gate").is_empty());
    }

    fn ticket(id: &str, title: &str, desc: &str, status: Status) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Bug,
            title,
            desc,
            Priority::High,
            Complexity::Medium,
            false,
        )
        .expect("ticket");
        // Walk to the requested terminal status the way the workflow would.
        if status != Status::Open {
            let _ = t.claim(Role::System, "w", "now");
            let _ = t.transition_to(Role::DevBug, Status::Fixed);
        }
        t
    }

    #[test]
    fn a_closed_ticket_with_the_same_symptom_is_offered_back() {
        let tickets = vec![
            ticket(
                "COX-B001",
                "Deploy health gate passes a dead container",
                "Deploy reported success while the container never bound its port.",
                Status::Fixed,
            ),
            ticket(
                "COX-B050",
                "Chat input loses focus on send",
                "Pressing enter moves focus to the message list.",
                Status::Fixed,
            ),
            ticket(
                "COX-B099",
                "Deploy health gate passes a dead container",
                "Deploy reported success while the container never bound its port.",
                Status::Open,
            ),
        ];
        let out = prior_fix_block(
            &tickets,
            "deploy health gate reports success but the container is dead",
            "COX-B077",
        );
        assert!(out.contains("COX-B001"), "the solved twin is offered");
        assert!(!out.contains("COX-B050"), "unrelated work stays out");
        assert!(
            !out.contains("COX-B099"),
            "still-open tickets are not answers"
        );
        assert!(
            prior_fix_block(&tickets, "deploy health gate", "COX-B001").is_empty()
                || !prior_fix_block(&tickets, "deploy health gate", "COX-B001")
                    .contains("COX-B001"),
            "a ticket is never offered its own history"
        );
    }

    #[tokio::test]
    async fn project_docs_are_matched_by_section_not_by_file_size() {
        let dir = std::env::temp_dir().join(format!("repodocs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).expect("mkdir");
        std::fs::write(
            dir.join("README.md"),
            "# Project\nSome prose.\n## Deploying\nThe deploy health gate polls the port.\n",
        )
        .expect("write");
        std::fs::write(
            dir.join("docs/style.md"),
            "## Style\nTabs versus spaces, and other opinions.\n",
        )
        .expect("write");
        let fs = &crate::test_fs::StdFsFiles;
        let out = repo_docs_block(Some(fs), &dir, "deploy health gate port polling").await;
        assert!(out.contains("README.md"), "the matching section wins");
        assert!(!out.contains("style.md"));
        assert!(repo_docs_block(Some(fs), &dir, "unrelated subject matter")
            .await
            .is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod test_surface_tests {
    use super::test_surface_block;

    #[tokio::test]
    async fn reports_existing_suites_and_registered_routes() {
        let dir = std::env::temp_dir().join(format!("surface-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mkdir");
        std::fs::create_dir_all(dir.join("target/debug")).expect("mkdir");
        std::fs::write(
            dir.join("src/auth.rs"),
            "#[test]\nfn a() {}\n#[tokio::test]\nasync fn b() {}\n",
        )
        .expect("write");
        std::fs::write(
            dir.join("src/server.rs"),
            "fn routes() {\n    .route(\"/api/health\", get(health))\n}\n",
        )
        .expect("write");
        // Build output must not be walked: it is enormous and tells a tester
        // nothing.
        std::fs::write(dir.join("target/debug/junk.rs"), "#[test]\nfn nope() {}\n").expect("write");

        let out = test_surface_block(Some(&crate::test_fs::StdFsFiles), &dir).await;
        assert!(out.contains("src/auth.rs (2)"), "counts both test macros");
        assert!(out.contains("/api/health"), "registered route surfaces");
        assert!(!out.contains("junk.rs"), "target/ is skipped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_project_with_neither_says_nothing() {
        let dir = std::env::temp_dir().join(format!("surface-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("main.rs"), "fn main() {}\n").expect("write");
        assert!(test_surface_block(Some(&crate::test_fs::StdFsFiles), &dir)
            .await
            .is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod system_prompt_composition_tests {
    use super::{system_prompt, BASE, ENGINEERING_STANDARDS};

    // Regression guard for F001 (see ticket CXA-F022). Every agent run composes
    // BASE + ENGINEERING_STANDARDS + a role section through system_prompt. The
    // four prompt-system fixes gating F001 must leave this exact composition
    // unchanged; any fix that drops a section, reorders it, or inlines part of
    // the standards into BASE breaks every role that calls system_prompt.
    #[test]
    fn composes_base_then_engineering_standards_then_role_section() {
        let out = system_prompt("ROLE MARKER");
        assert!(out.starts_with(BASE), "BASE opens the prompt");
        assert!(
            out.contains(ENGINEERING_STANDARDS),
            "engineering standards present"
        );
        assert!(out.contains("ROLE MARKER"), "role section present");
        let base_end = out.find(ENGINEERING_STANDARDS).unwrap();
        let eng_end = base_end + ENGINEERING_STANDARDS.len();
        assert!(
            !out[..base_end].contains("ROLE MARKER"),
            "role section must come after engineering standards"
        );
        assert!(
            out[eng_end..].contains("ROLE MARKER"),
            "role section sits after engineering standards"
        );
    }

    #[test]
    fn sections_are_distinct_and_blank_line_separated() {
        let out = system_prompt("PD");
        assert_eq!(out.matches(BASE).count(), 1, "BASE appears exactly once");
        assert_eq!(
            out.matches(ENGINEERING_STANDARDS).count(),
            1,
            "engineering standards appear exactly once"
        );
        for separator in ["", "\n"] {
            let joined_base = format!("{BASE}{separator}{ENGINEERING_STANDARDS}");
            let joined_eng = format!("{ENGINEERING_STANDARDS}{separator}PD");
            assert!(
                !out.contains(&joined_base),
                "BASE and standards are not concatenated without blank-line separation"
            );
            assert!(
                !out.contains(&joined_eng),
                "standards and role section are blank-line separated"
            );
        }
    }

    #[test]
    fn every_role_still_reaches_a_nonempty_composed_prompt() {
        for role in [super::PO, super::SM, super::BA, super::SA] {
            let p = system_prompt(role);
            assert!(p.starts_with(BASE), "{role:?} prompt opens with BASE");
            assert!(p.contains(ENGINEERING_STANDARDS));
            assert!(
                p.len() > BASE.len() + ENGINEERING_STANDARDS.len(),
                "{role:?} prompt carries the role section too"
            );
        }
    }
}
