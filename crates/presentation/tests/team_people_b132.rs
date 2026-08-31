//! CXA-B132 regression — the Team view's "People on this project" panel was
//! stuck on bare "loading…" forever for the hub owner.
//!
//! The hub owner is provisioned role `super` (bootstrap_admin), but the UI
//! gated every admin surface on `role==="admin"` exactly. Three links in the
//! chain broke for that role, and every link is pinned here on the bytes the
//! hub actually serves — the same no-harness discipline as
//! `signin_icon_font_b112.rs` and `preflight_f239_tdd.rs`: no server, no
//! port, no invented types. The runtime half of the proof (a real signed-in
//! super session reaching the panel's terminal state) lives in
//! `e2e/specs-auth/team-people-super.spec.ts`; this file pins the wiring so
//! `cargo test` catches the regression without a browser.
//!
//!   1. `applyRole` never set `body.admin` for `super`, and the CSS
//!      (`body:not(.admin) .admin-only`) hid the panel entirely.
//!   2. `renderActive`'s team branch never called `renderTeamPeople()` for
//!      `super`, so even un-hidden the card kept its initial placeholder.
//!   3. The ⌘K palette filtered admin views on the same exact match.
//!
//! The fix routes all three through one hub-admin predicate. Each test below
//! fails on the pre-fix tree and passes on the fixed one.

/// The shared-scope script owning the role model and `applyRole`.
const SHELL_JS: &str = include_str!("../src/web/js/shell.js");

/// The shared-scope script owning `renderActive`'s team branch.
const CORE_JS: &str = include_str!("../src/web/js/core.js");

/// The dashboard markup — where the panel and its `admin-only` visibility live.
const INDEX_HTML: &str = include_str!("../src/web/index.html");

/// The one line of `src` containing `needle`. Line-scoped on purpose: a bare
/// `contains` can pass on a comment or dead code elsewhere in the file, while
/// these gates must pin executable wiring.
fn line_containing<'a>(src: &'a str, needle: &str) -> Option<&'a str> {
    src.lines().find(|line| line.contains(needle))
}

/// Fixture sanity: the panel exists, is an `admin-only` surface, and ships
/// with the `loading…` placeholder whose stuck state is the bug's symptom.
/// Fails with a routing message if the markup moves, so the wiring tests
/// below can never silently pass against a panel that no longer exists.
#[test]
fn team_people_panel_is_an_admin_only_surface_on_the_team_view() {
    let card = line_containing(INDEX_HTML, "id=\"team-people\"")
        .expect("the #team-people card vanished from index.html — the Team \
                view's people panel moved; re-point the CXA-B132 gates");
    assert!(
        card.contains("admin-only"),
        "#team-people must stay an admin-only surface — its visibility is \
         what applyRole's body.admin class governs (CXA-B132): {card}"
    );
    assert!(
        card.contains("loading"),
        "#team-people lost its initial placeholder — the stuck-on-loading \
         symptom this gate exists for is no longer observable: {card}"
    );
}

/// Link 1 — the predicate itself. `super` and `admin` are both the hub-admin
/// tier, and open/local mode (no auth) sees admin surfaces too, exactly as
/// the pre-B132 gate allowed for admins. Any edit to these lines must be a
/// conscious re-decision of who sees admin surfaces, not drift.
#[test]
fn the_hub_admin_predicate_admits_super_admin_and_open_mode() {
    let role = line_containing(SHELL_JS, "function roleIsHubAdmin(r)")
        .expect("roleIsHubAdmin vanished from shell.js — the hub-admin tier \
                predicate that CXA-B132 introduced is gone");
    assert!(
        role.contains("\"super\"") && role.contains("\"admin\""),
        "roleIsHubAdmin must admit BOTH hub-admin roles — dropping \"super\" \
         re-strands the hub owner (CXA-B132): {role}"
    );
    let wrapper = line_containing(SHELL_JS, "function isHubAdmin()")
        .expect("isHubAdmin vanished from shell.js — the open-mode-aware \
                wrapper the call sites delegate to is gone");
    assert!(
        wrapper.contains("!ME||!ME.auth"),
        "isHubAdmin must keep the open/local-mode clause (no auth sees admin \
         surfaces, the B121-era behavior): {wrapper}"
    );
}

/// Link 2 — visibility. `applyRole` owns `body.admin`, and the CSS
/// `body:not(.admin) .admin-only{display:none}` made the whole panel
/// disappear for `super` even when every other link was fixed.
#[test]
fn apply_role_derives_admin_surface_visibility_from_the_predicate() {
    let gate = line_containing(SHELL_JS, "const isAdmin=isHubAdmin();")
        .expect("applyRole no longer derives isAdmin from isHubAdmin() — \
                admin-surface visibility drifted back to a role literal \
                (the CXA-B132 half that hid the panel from the hub owner)");
    assert!(
        gate.contains("isHubAdmin()"),
        "applyRole must consult the shared hub-admin predicate: {gate}"
    );
}

/// Link 3 — the renderer call. This is the ticket's named root cause: the
/// team branch called `renderTeamPeople()` under `ME.role==="admin"`
/// exactly, so the hub owner's panel never left "loading…".
#[test]
fn the_team_branch_populates_the_people_panel_via_the_predicate() {
    assert!(
        CORE_JS.contains("renderTeamPeople()"),
        "renderTeamPeople() is never called from core.js — the panel has no \
         renderer to gate at all"
    );
    let gate = line_containing(CORE_JS, "if(isHubAdmin())renderTeamPeople();")
        .expect("the team branch no longer gates renderTeamPeople() behind \
                isHubAdmin() — role \"super\" (the hub owner, CXA-B132) is \
                back to a permanently stuck panel");
    assert!(
        !gate.contains("ME.role"),
        "the gate must delegate to the shared predicate, not re-match roles \
         inline — that inline match is the original drift: {gate}"
    );
}

/// Link 4 — the ⌘K palette. It filtered admin views on `ME.role==="admin"`
/// exactly, so the hub owner could not reach Audit/People from the palette
/// even after the sidebar showed them. Authed admins keep access; open mode
/// deliberately does not gain palette entries (pre-existing behavior).
#[test]
fn the_command_palette_filters_admin_views_through_the_predicate() {
    let gate =
        line_containing(SHELL_JS, "const isAdmin=!!ME&&roleIsHubAdmin(ME.role);")
            .expect("cmdkBuild no longer filters admin views through \
                    roleIsHubAdmin — the palette drifted back to an exact \
                    role match (the CXA-B132 ⌘K half)");
    assert!(
        gate.contains("!!ME"),
        "the palette gate must stay authed-only (open mode never had palette \
         admin entries): {gate}"
    );
}
