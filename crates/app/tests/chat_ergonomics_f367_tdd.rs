//! CXA-F367 — Chat message ergonomics: edit, delete, reactions and pinned
//! messages. RED half of the TDD pair.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "Own messages support in-place edit (shows 'edited') and delete
//!    (tombstone); server rejects edits of others' messages except admin
//!    delete"
//! 2. "Reactions toggle per user from a 5-emoji set and render as count pills
//!    live for all viewers"
//! 3. "Channel owner/admin can pin up to 5 messages; the pin bar lists them
//!    and click scrolls to the message; unpin works"
//! 4. "All rules enforced server-side (not just hidden buttons); golden
//!    screenshots updated; console gate clean"
//!
//! HOW THESE CRITERIA ARE ENCODED: pure fixtures over the real state/domain
//! types the codebase has today (`SystemChat` + `ChatMsg`/`Reaction`/`Channel`
//! driven through the REAL aggregate API — `post`, `react`, `create_channel`,
//! serde round-trips; no engine, no harness, no network port) plus source-scan
//! guards over the files the hub actually builds and serves
//! (`server/chat.rs`, `web/js/chat.js`, `web/js/shell.js`, `web/index.html`,
//! `e2e/specs/chat.spec.ts`) — the same no-harness discipline as
//! `deploy_forensics_f289_tdd.rs` and `global_search_f275_tdd.rs`. A test
//! that called a `SystemChat::delete_message`/`::pin` directly could not
//! compile today (no such symbols exist — verified before writing this
//! file), so the red half pins the missing RULES where they must be declared,
//! and the green half pins executable semantics over the types that DO exist.
//! Every failing assertion below fails only because CXA-F367's behaviour is
//! missing; if an assertion's mechanism moves during implementation, move the
//! guard with it (the `preflight_f239_tdd.rs` convention).
//!
//! Red today, and why (evidence, not judgement):
//!   * AC1 — `syschat_delete_ep` (server/chat.rs:672) refuses EVERY non-author
//!     (`msg.user != user` → 403 "not yours"): an admin CANNOT tombstone
//!     another user's message, which is exactly the "except admin delete"
//!     carve-out the AC grants. Edit (server/chat.rs:641) is already
//!     author-only and stamps `edited` — green-guarded so it cannot weaken.
//!   * AC2 — `SystemChat::react` (system_chat.rs:268) toggles per user
//!     (green-guarded below) but accepts ANY emoji, and `syschat_react_ep`
//!     (server/chat.rs:506) merely truncates to 8 chars. No 5-emoji set is
//!     declared anywhere (the picker `EMOJI_CATS` is the full keyboard, not a
//!     reaction set) — the AC's set is undeclared and unenforced.
//!   * AC3 — `syschat_pin_ep` (server/chat.rs:726) toggles a pin for ANY
//!     caller: no owner/admin permission check, no cap (a 6th pin is accepted
//!     as happily as the 1st). The pin bar itself (shell.js `loadPins`,
//!     `pins-bar` in index.html, click → `scrollIntoView`, unpin toast) is
//!     wired — green-guarded below.
//!   * AC4 — every rule above lives (or must live) server-side; and
//!     `e2e/specs/chat.spec.ts` (27 lines) exercises none of the four
//!     ergonomics under its console gate + golden, so "golden screenshots
//!     updated; console gate clean" is unverifiable for this behaviour today.
//!
//! Green guards (fixture validity — house rule: every fixture must be
//! buildable from data the codebase actually has):
//!   * the reaction toggle law the AC builds on is real and executable today
//!     (`SystemChat::react` over `SystemChat::default()`);
//!   * a legacy `ChatMsg` document without the ergonomics keys loads with the
//!     documented defaults, and an edited/deleted message round-trips — the
//!     additive-field wire contract the tombstone and "(edited)" render ride;
//!   * pins are ids that resolve to live messages of their own channel (the
//!     data law `syschat_pins_ep` and the pin bar depend on);
//!   * a private channel records its `owner` — the permission primitive the
//!     pin rule builds on.
//!
//! PINNED CONTRACTS (flag to SA before renaming — the names below are name
//! PATTERNS, not exact spellings, so an honest implementation cannot miss):
//!   * the 5-emoji reaction set: a `pub const` whose name contains EMOJI or
//!     REACTION, declared beside the aggregate that owns `react`
//!     (`system_chat.rs`), holding EXACTLY 5 emoji literals — the AC fixes
//!     the count, not the glyphs, so the test pins the count and never the
//!     specific emojis;
//!   * the 5-pin cap: a `pub const` named `MAX_PIN*` beside the chat caps
//!     (the `MAX_CHAT`/`MAX_COMMENTS` house convention), enforced in the pin
//!     handler;
//!   * pin permission and the admin-delete carve-out in their handler windows
//!     in `server/chat.rs` (the `user_can_manage`/`is_admin`/`can_pin`
//!     vocabulary this file already uses for the same authority).
//!
//! AC → test map:
//! - AC1: [`the_delete_rule_lets_an_admin_tombstone_another_users_message`]
//!   (RED), [`the_edit_rule_stays_author_only_with_no_admin_override`]
//!   (green, protective), [`the_ui_renders_the_edited_marker_and_the_delete_tombstone`]
//!   (green), [`a_legacy_message_without_the_ergonomics_keys_loads_with_the_defaults`]
//!   (green wire contract)
//! - AC2: [`reactions_toggle_per_user_and_emptied_reactions_are_pruned`]
//!   (green law), [`the_reaction_set_is_a_declared_five_emoji_const`] (RED),
//!   [`react_enforces_the_declared_set_server_side`] (RED),
//!   [`the_ui_renders_reactions_as_count_pills_and_applies_live_updates`]
//!   (green)
//! - AC3: [`the_pin_rule_is_owner_or_admin_enforced_server_side`] (RED),
//!   [`the_five_pin_cap_is_a_named_const_beside_the_chat_caps`] (RED),
//!   [`the_pin_handler_enforces_the_five_pin_cap`] (RED),
//!   [`pins_reference_live_messages_of_their_channel`] (green),
//!   [`the_channel_records_its_owner_the_pin_rule_builds_on`] (green),
//!   [`the_pin_bar_lists_pins_click_scrolls_and_unpin_toggles`] (green)
//! - AC4: the RED rule guards above (all server-side) +
//!   [`the_chat_e2e_spec_exercises_the_ergonomics_under_the_console_gate`]
//!   (RED — the spec must cover the four ergonomics, arm the shared console
//!   gate and refresh the golden, the `e2e_acceptance_gate.rs` anti-shrink
//!   pattern).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use coxagent_application::system_chat::{ChatContext, SystemChat, UserRef};
use coxagent_application::{ChatMsg, Reaction, GENERAL_CHANNEL};

// --- repo-state scan helpers (the deploy_forensics_f289_tdd.rs pattern) ------

const SERVER_CHAT: &str = "crates/presentation/src/server/chat.rs";
const SYSTEM_CHAT: &str = "crates/application/src/system_chat.rs";
const STATE_CHAT: &str = "crates/application/src/state/chat.rs";
const STATE_MOD: &str = "crates/application/src/state/mod.rs";
const CHAT_JS: &str = "crates/presentation/src/web/js/chat.js";
const SHELL_JS: &str = "crates/presentation/src/web/js/shell.js";
const INDEX_HTML: &str = "crates/presentation/src/web/index.html";
const E2E_SPEC: &str = "e2e/specs/chat.spec.ts";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The source window of one top-level item: from its `header` to the next item
/// introduced by `terminator` (or end of file). Everything the item declares
/// lives inside this window. An absent header yields an empty window — the
/// caller's own assertion, not an index panic, must report the miss.
fn window_of<'a>(src: &'a str, header: &str, terminator: &str) -> &'a str {
    let Some(at) = src.find(header) else {
        return "";
    };
    let rest = &src[at..];
    let end = rest[header.len()..]
        .find(terminator)
        .map_or(rest.len(), |rel| header.len() + rel);
    &rest[..end]
}

fn edit_window(src: &str) -> &str {
    window_of(
        src,
        "async fn syschat_edit_ep",
        "pub(super) async fn syschat_delete_ep",
    )
}

fn delete_window(src: &str) -> &str {
    window_of(
        src,
        "async fn syschat_delete_ep",
        "pub(super) async fn syschat_search_ep",
    )
}

fn react_ep_window(src: &str) -> &str {
    window_of(
        src,
        "async fn syschat_react_ep",
        "pub(super) async fn syschat_webhook_create_ep",
    )
}

fn pin_window(src: &str) -> &str {
    window_of(
        src,
        "async fn syschat_pin_ep",
        "pub(super) async fn syschat_pins_ep",
    )
}

fn react_method_window(src: &str) -> &str {
    window_of(src, "pub fn react", "pub fn create_webhook")
}

/// Whether the admin/owner authority vocabulary appears in a handler window —
/// the same tokens `server/chat.rs` already uses for identical authority
/// (`user_can_manage` for hub admins, `can_*` for channel-level rules).
fn has_authority_token(window: &str) -> bool {
    ["can_pin", "user_can_manage", "is_admin", "owner"]
        .iter()
        .any(|t| window.contains(t))
}

/// A `pub const` whose NAME contains EMOJI or REACTION in `src`, returned as
/// (name, initializer window up to the first `;`). None when undeclared — the
/// caller's assertion, not a panic, must report the miss.
fn reaction_set_const(src: &str) -> Option<(String, String)> {
    for line in src.lines() {
        let t = line.trim_start();
        if !(t.starts_with("pub const") || t.starts_with("pub(crate) const")) {
            continue;
        }
        let Some(after) = t.split("const").nth(1) else {
            continue;
        };
        let Some(name) = after.split([':', ' ', '\t']).find(|s| !s.is_empty()) else {
            continue;
        };
        let upper = name.to_uppercase();
        if upper.contains("EMOJI") || upper.contains("REACTION") {
            let init = window_of(src, &format!("const {name}"), ";");
            return Some((name.to_owned(), init.to_owned()));
        }
    }
    None
}

/// Quoted string/char literals containing at least one non-ASCII char — the
/// emoji entries of the reaction set. Counted, never named: the AC fixes the
/// count (5), not the glyphs.
fn emoji_literals(src: &str) -> usize {
    let mut count = 0;
    let mut in_lit = false;
    let mut lit_has_emoji = false;
    for c in src.chars() {
        match c {
            '\'' | '"' => {
                if in_lit && lit_has_emoji {
                    count += 1;
                }
                in_lit = !in_lit;
                lit_has_emoji = false;
            }
            _ if in_lit && !c.is_ascii() => lit_has_emoji = true,
            _ => {}
        }
    }
    count
}

/// A `MAX_PIN*` cap const in `src`, with its declared value.
fn pin_cap_const(src: &str) -> Option<usize> {
    src.lines().find_map(|line| {
        let t = line.trim_start();
        if !(t.contains("const") && t.contains("MAX_PIN")) {
            return None;
        }
        let val = t.split('=').nth(1)?;
        val.trim()
            .trim_end_matches(';')
            .trim()
            .parse::<usize>()
            .ok()
    })
}

// --- fixtures: built ONLY through the aggregate's legal API ------------------

fn ctx_with_admin() -> ChatContext {
    ChatContext {
        users: vec![
            UserRef {
                username: "alice".into(),
                projects: vec![],
                admin: false,
            },
            UserRef {
                username: "root".into(),
                projects: vec![],
                admin: true,
            },
        ],
        projects: vec![],
    }
}

// --- GREEN: executable semantics over the types that DO exist ----------------

#[test]
fn reactions_toggle_per_user_and_emptied_reactions_are_pruned() {
    let mut sc = SystemChat::default();
    let id = sc.post("alice", "hello room", GENERAL_CHANNEL, Vec::new());

    // First reaction from one user: one pill, one user.
    let m = sc.react(&id, "alice", "👍").expect("msg exists");
    assert_eq!(
        m.reactions,
        vec![Reaction {
            emoji: "👍".into(),
            users: vec!["alice".into()],
        }]
    );

    // A second user joins the SAME pill (count pills, not per-user rows).
    let m = sc.react(&id, "bob", "👍").expect("msg exists");
    assert_eq!(m.reactions.len(), 1);
    assert_eq!(m.reactions[0].users.len(), 2);

    // Toggling again removes ONLY that user's reaction (per-user toggle).
    let m = sc.react(&id, "alice", "👍").expect("msg exists");
    assert_eq!(m.reactions[0].users, vec!["bob".to_owned()]);

    // The last user leaving prunes the pill entirely.
    let m = sc.react(&id, "bob", "👍").expect("msg exists");
    assert!(m.reactions.is_empty(), "an emptied reaction must be pruned");

    // A reaction on an unknown message is not found — never a phantom pill.
    assert!(sc.react("missing", "alice", "👍").is_none());
}

#[test]
fn a_legacy_message_without_the_ergonomics_keys_loads_with_the_defaults() {
    // A pre-ergonomics persisted document: no edited/deleted/reactions keys.
    let legacy =
        r#"{"id":"m1","at":"2026-01-01T00:00:00Z","user":"alice","body":"hi","channel":"general"}"#;
    let m: ChatMsg = serde_json::from_str(legacy).expect("legacy document loads");
    assert!(!m.deleted, "a legacy message is not a tombstone");
    assert!(m.edited.is_none(), "a legacy message was never edited");
    assert!(m.reactions.is_empty());

    // The ergonomics keys round-trip: this is the wire contract the
    // "(edited)" marker and the tombstone render ride on.
    let mut m: ChatMsg = serde_json::from_str(legacy).unwrap();
    m.edited = Some("2026-09-05T00:00:00Z".into());
    m.deleted = true;
    m.reactions.push(Reaction {
        emoji: "🎉".into(),
        users: vec!["bob".into()],
    });
    let back: ChatMsg = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
    assert_eq!(back, m);
}

#[test]
fn pins_reference_live_messages_of_their_channel() {
    let mut sc = SystemChat::default();
    let a = sc.post("alice", "first", GENERAL_CHANNEL, Vec::new());
    let b = sc.post("bob", "second", GENERAL_CHANNEL, Vec::new());

    // The pin bar can only list what resolves: every pinned id must be a live
    // message of the SAME channel — the data law `syschat_pins_ep` filters on
    // and the pin bar's chips depend on.
    sc.pins
        .insert(GENERAL_CHANNEL.to_owned(), vec![a.clone(), b.clone()]);
    let in_channel = sc.messages_in(GENERAL_CHANNEL);
    for pid in &sc.pins[GENERAL_CHANNEL] {
        assert!(
            in_channel
                .iter()
                .any(|m| &m.id == pid && m.channel == GENERAL_CHANNEL),
            "pin {pid} must resolve to a live message of #{GENERAL_CHANNEL}"
        );
    }
    assert_eq!(sc.pins.get("other").map(Vec::len), None);
}

#[test]
fn the_channel_records_its_owner_the_pin_rule_builds_on() {
    let ctx = ctx_with_admin();
    let mut sc = SystemChat::default();
    let ch = sc.create_channel("Pins", "alice", &ctx).expect("create");
    assert_eq!(ch.owner, "alice", "the owner is recorded on the channel");
    assert!(ch.can_view("alice"));
    assert!(!ch.can_view("mallory"), "a non-member is outside the room");
}

// --- GREEN: view-side contracts that exist and must not shrink ---------------

#[test]
fn the_ui_renders_the_edited_marker_and_the_delete_tombstone() {
    // The message renderer (renderOneMsg) lives in shell.js, not chat.js.
    let js = read(SHELL_JS);
    assert!(
        js.contains("(edited)"),
        "shell.js must mark edited messages with the '(edited)' marker (AC1)"
    );
    assert!(
        js.contains("This message was deleted"),
        "shell.js must render deleted messages as a tombstone, not drop them (AC1)"
    );
}

#[test]
fn the_ui_renders_reactions_as_count_pills_and_applies_live_updates() {
    // Pills are rendered by the message renderer (shell.js); the live update
    // rides the server's `reaction` broadcast into chat.js's applyReaction.
    let shell = read(SHELL_JS);
    assert!(
        shell.contains("tcre") && shell.contains("users.length"),
        "shell.js must render each reaction as a pill showing its user count (AC2)"
    );
    let chat = read(CHAT_JS);
    assert!(
        chat.contains("applyReaction") && chat.contains("\"reaction\""),
        "chat.js must apply the server's reaction broadcast so pills update live for all viewers (AC2)"
    );
    assert!(
        chat.contains("/api/chat/react"),
        "chat.js toggleReact must POST the toggle to the server so every viewer sees it (AC2)"
    );
}

#[test]
fn the_pin_bar_lists_pins_click_scrolls_and_unpin_toggles() {
    let shell = read(SHELL_JS);
    let html = read(INDEX_HTML);
    assert!(
        html.contains("pins-bar"),
        "index.html must carry the pin bar surface (AC3)"
    );
    assert!(
        shell.contains("/api/chat/pins?channel="),
        "shell.js loadPins must fetch the channel's pins from the server (AC3)"
    );
    assert!(
        shell.contains("pin-chip") && shell.contains("scrollIntoView"),
        "clicking a pin chip must scroll to the message (AC3)"
    );
    assert!(
        shell.contains("Unpinned"),
        "unpin must be a real user-visible toggle, not a one-way pin (AC3)"
    );
}

// --- AC1: the delete rule is missing its admin carve-out (RED) ---------------

#[test]
fn the_delete_rule_lets_an_admin_tombstone_another_users_message() {
    let src = read(SERVER_CHAT);
    let win = delete_window(&src);
    assert!(
        !win.is_empty(),
        "syschat_delete_ep must still exist in {SERVER_CHAT}"
    );
    // AC1 grants exactly one exception to author-only delete: admin delete.
    assert!(
        win.contains("user_can_manage") || win.contains("is_admin"),
        "syschat_delete_ep refuses every non-author today — the admin carve-out \
         ('except admin delete') is missing; enforce it server-side where the \
         tombstone is written"
    );
    // The tombstone contract itself is already right and must stay: soft
    // delete marks the message and empties its body.
    assert!(win.contains("deleted"), "delete must be a soft tombstone");
    assert!(
        win.contains("String::new") || win.contains("body.clear()"),
        "the tombstone clears the body"
    );
}

#[test]
fn the_edit_rule_stays_author_only_with_no_admin_override() {
    let src = read(SERVER_CHAT);
    let win = edit_window(&src);
    assert!(
        !win.is_empty(),
        "syschat_edit_ep must still exist in {SERVER_CHAT}"
    );
    // AC1's rule: NOBODY edits another user's message — "except admin delete"
    // carves out delete only. The author check and the edited stamp are the
    // contract; an admin bypass must not creep in.
    assert!(
        win.contains("msg.user != user") && win.contains("FORBIDDEN"),
        "the edit handler must keep rejecting edits of others' messages"
    );
    assert!(
        win.contains("edited"),
        "an accepted edit must stamp the message so the UI can show '(edited)'"
    );
    assert!(
        !win.contains("user_can_manage") && !win.contains("is_admin"),
        "edit must stay author-only — the AC's only exception is admin DELETE"
    );
}

// --- AC2: the 5-emoji reaction set is undeclared and unenforced (RED) --------

#[test]
fn the_reaction_set_is_a_declared_five_emoji_const() {
    let src = read(SYSTEM_CHAT);
    let Some((name, init)) = reaction_set_const(&src) else {
        panic!(
            "no reaction emoji set is declared in {SYSTEM_CHAT} — `react` accepts \
             ANY emoji today; declare the AC's 5-emoji set as a named const beside \
             the aggregate that owns `react` (name must contain EMOJI or REACTION)"
        );
    };
    let n = emoji_literals(&init);
    assert_eq!(
        n, 5,
        "`{name}` must hold EXACTLY 5 emojis (the AC's set size), found {n}"
    );
}

#[test]
fn react_enforces_the_declared_set_server_side() {
    let sys = read(SYSTEM_CHAT);
    let Some((name, _)) = reaction_set_const(&sys) else {
        panic!(
            "the 5-emoji set is not declared, so nothing can enforce it — declare \
             it first (see the_set test)"
        );
    };
    let aggregate = react_method_window(&sys);
    let server = read(SERVER_CHAT);
    let endpoint = react_ep_window(&server);
    assert!(
        aggregate.contains(&name) || endpoint.contains(&name),
        "the emoji set `{name}` is declared but enforced nowhere on the server \
         path — `SystemChat::react` (or `syschat_react_ep`) must reject emojis \
         outside the set; a picker-limited UI is not enforcement"
    );
}

// --- AC3: pin permission and the 5-pin cap are missing (RED) -----------------

#[test]
fn the_pin_rule_is_owner_or_admin_enforced_server_side() {
    let src = read(SERVER_CHAT);
    let win = pin_window(&src);
    assert!(
        !win.is_empty(),
        "syschat_pin_ep must still exist in {SERVER_CHAT}"
    );
    assert!(
        has_authority_token(win),
        "syschat_pin_ep lets ANY caller pin today — the AC restricts pinning to \
         the channel owner or an admin; enforce it server-side in the handler \
         (the `user_can_manage`/`is_admin`/`can_pin`/`owner` vocabulary)"
    );
    // Toggle semantics (pin ↔ unpin on the same call) are already right.
    assert!(
        win.contains("retain"),
        "the same endpoint must unpin — the toggle is the unpin affordance"
    );
}

#[test]
fn the_five_pin_cap_is_a_named_const_beside_the_chat_caps() {
    let candidates = [read(SYSTEM_CHAT), read(STATE_CHAT), read(STATE_MOD)];
    let found = candidates.iter().find_map(|src| pin_cap_const(src));
    assert_eq!(
        found,
        Some(5),
        "no MAX_PIN* cap const is declared beside the chat caps (MAX_CHAT in \
         {STATE_MOD}, the chat rules in {STATE_CHAT}/{SYSTEM_CHAT}) — declare \
         the AC's 5-pin cap as a named const (house cap convention)"
    );
}

#[test]
fn the_pin_handler_enforces_the_five_pin_cap() {
    let src = read(SERVER_CHAT);
    let win = pin_window(&src);
    assert!(
        win.contains("MAX_PIN"),
        "syschat_pin_ep never checks a cap — a 6th pin is accepted as happily as \
         the 1st; enforce the declared MAX_PIN* const in the handler (a channel \
         with 5 pins must refuse the 6th)"
    );
}

// --- AC4: the e2e contract for the ergonomics is missing (RED) ---------------

#[test]
fn the_chat_e2e_spec_exercises_the_ergonomics_under_the_console_gate() {
    let spec = read(E2E_SPEC);
    assert!(
        spec.contains("armConsoleGate") && spec.contains("toHaveScreenshot"),
        "{E2E_SPEC} must run the ergonomics under the shared console gate and \
         refresh the chat golden"
    );
    let edit =
        spec.contains("PATCH") || spec.contains("startEditMsg") || spec.contains("saveEditInline");
    let delete = spec.contains("deleteMsg") || spec.contains("DELETE");
    let react = spec.contains("toggleReact") || spec.contains("/api/chat/react");
    let pin = spec.contains("togglePin") || spec.contains("/pin");
    assert!(
        edit && delete && react && pin,
        "{E2E_SPEC} exercises none of the four ergonomics today (edit={edit} \
         delete={delete} react={react} pin={pin}) — cover edit, delete, \
         reactions and pins in the browser so the goldens and the console gate \
         actually gate them"
    );
}
