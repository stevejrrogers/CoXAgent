//! CXA-F371 — Bootstrap lessons shipped with the binary: a fresh install
//! starts with the accumulated lesson base instead of re-earning every rule
//! by failing on its own.
//!
//! Today the hard-won operating lessons live in the RUNTIME stores of the
//! install that learned them (`hub_lessons.md`, `state.lessons`); a
//! brand-new machine boots wise-less and re-learns everything the expensive
//! way. This module owns the curated, portable distillation — the rules
//! every project should start with — and the pure seed that installs them
//! into a project's lesson base on first boot (wired in
//! `crates/app/src/builders.rs`, the one place every project boots through).
//!
//! Texts are distilled from what the codebase already enforces and learned:
//! the depth bar verbatim from `prompts::ENGINEERING_STANDARDS` (the canary
//! test below fails if the shipped copy ever drifts from the house
//! standards), the gate-exit and mode-switch rules from the BASE operating
//! rules, the engine-vs-task failure discipline from `faults::is_infra_fault`
//! — the single source of truth the DEV failure counter and the circuit
//! breaker already consult — plus the hub-port and build-artifact hygiene
//! rules this repo's own history keeps re-teaching.
//!
//! THE SEED CONTRACT — pure, additive-only, idempotent:
//! * a lesson whose stable id is already in the ledger is skipped, so a
//!   second boot seeds nothing (idempotent) and an operator's edit of a
//!   shipped lesson's text still counts as seeded (the id survives the edit);
//! * a text the project already learned at runtime is skipped — the team's
//!   own record wins; seeding never overwrites, re-stamps or re-counts an
//!   existing lesson as a re-learning;
//! * an upgraded binary carrying NEW shipped ids adds only those new ids —
//!   previously shipped and locally learned lessons are preserved verbatim;
//! * the store caps are enforced exactly as
//!   [`crate::state::ProjectState::record_lesson`] does (oldest evicted
//!   first, prompt list [`MAX_PROMPT_LESSONS`] / ledger
//!   [`MAX_LESSON_RECORDS`]).

use crate::state::{LessonRecord, ProjectState, MAX_LESSON_RECORDS, MAX_PROMPT_LESSONS};

/// The source marker stamped on every shipped lesson record (CXA-F371): the
/// lessons UI badges it so shipped wisdom is distinguishable from a
/// project's locally learned lessons. An absent source means locally learned.
pub const SHIPPED_SOURCE: &str = "shipped";

/// When the shipped base was curated (CXA-F371). A shipped lesson's ledger
/// `at` is THIS stamp, not boot time: "recorded" must mean when the rule was
/// actually learned upstream, and a boot-time stamp would make every
/// install's ledger (and every golden screenshot of it) drift per boot.
pub const SHIPPED_AT: &str = "2026-09-05T00:00:00Z";

/// One lesson of the shipped set: a stable id (the seed's dedupe identity —
/// what an upgrade adds "only the new" of) and the rule text role prompts
/// carry verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShippedLesson {
    /// Stable across releases; never repurpose an id — an upgrade ships new
    /// ids, it does not rewrite old ones.
    pub id: &'static str,
    /// The distilled rule, one prompt-carryable line.
    pub text: &'static str,
}

/// The depth bar, verbatim from `prompts::ENGINEERING_STANDARDS` — the
/// canary test pins this copy to the standards, so the two can never drift
/// apart unnoticed.
const DEPTH_BAR_LESSON: &str = "\
- DEPTH BAR — no shallow features, however small (operator mandate). Function alone is not done; \
depth is part of done: (1) a LIST VIEW ships with a filter, a sensible sort, counts/totals, \
pagination past ~50 rows, and an empty state that says what fills it; (2) a NUMBER ships with \
its window (today/7d/lifetime), a trend or delta where history exists, and a zero-state hint \
instead of a bare 0; (3) a STATE ships with attribution — who changed it, when, why — and the UI \
always says WHY something is off/paused/failing instead of silently idling; (4) an ACTION ships \
with its inverse and its guardrail — undo where cheap, a confirm naming the blast radius where \
destructive, and permission-DIMMED (not hidden) where unauthorized. Before calling a UI ticket \
fixed, check: filter? counts? empty state? attribution? drill-down? Two or more missing = not \
done.";

/// The curated bootstrap lesson base (CXA-F371): what a fresh install knows
/// on day one. Ship NEW rules as NEW ids at the end — never edit or retire an
/// existing id, the idempotent seed merges by it.
pub const SHIPPED_LESSONS: &[ShippedLesson] = &[
    ShippedLesson {
        id: "CXA-F371-depth-bar",
        text: DEPTH_BAR_LESSON,
    },
    ShippedLesson {
        id: "CXA-F371-gate-must-handle-failure",
        text: "every gate names its EXIT before it ships and handles the process it depends on \
               NOT succeeding: state what unblocks the gate and who can act — a gate whose exit \
               assumes another process succeeds parks mergeable work forever",
    },
    ShippedLesson {
        id: "CXA-F371-no-keyword-mode-switches",
        text: "free prose is never a mode switch: sprint goals, ticket titles and chat quote \
               each other, so keyword-sniffing them flips modes by accident — modes are explicit \
               state with a set-and-clear lifecycle, never a substring match",
    },
    ShippedLesson {
        id: "CXA-F371-engine-vs-task-failure",
        text: "an engine/infra fault — revoked auth (401), quota wall, rate limit, network \
               outage, the engine's own death (timed out, provider unavailable) — is not the \
               ticket's fault: never charge it to the ticket; a genuine task failure (compiler \
               errors, red tests) always is",
    },
    ShippedLesson {
        id: "CXA-F371-never-bind-the-hub-port",
        text: "never let a build of this project bind the hub's port (4000 by default): a hub \
               that finds its port taken moves to another one and the app then looks dead while \
               everything is in fact running — use the project's own deploy.host_port or \
               COXAGENT_PORT",
    },
    ShippedLesson {
        id: "CXA-F371-tmp-build-artifact-hygiene",
        text: "keep tmp and build artifacts out of the repo: build outputs, scratch dirs, logs \
               and downloaded binaries belong in target/, /tmp or gitignored paths — a clean \
               clone must build and reviews must never wade through machine-local residue",
    },
];

/// Seed the shipped lesson base into `state`'s lesson stores (the
/// prompt-facing `lessons` and the CXA-F306 efficacy ledger). PURE over the
/// state — the boot wiring owns the load/save around it — and IDEMPOTENT:
/// returns the ids newly added, empty on an already-seeded state, so a
/// second boot persists nothing.
///
/// Additive-only, never overwriting: a record already carrying the stable id
/// (shipped, possibly operator-edited since) or a lesson already learned
/// under the same text is preserved untouched — an upgrade adds only the new
/// shipped ids.
#[must_use]
pub fn seed_lessons(state: &mut ProjectState) -> Vec<&'static str> {
    let mut seeded = Vec::new();
    for lesson in SHIPPED_LESSONS {
        let already_known = state.lessons.iter().any(|l| l.as_str() == lesson.text)
            || state.lesson_records.iter().any(|record| {
                record.id.as_deref() == Some(lesson.id) || record.text == lesson.text
            });
        if already_known {
            continue;
        }
        state.lesson_records.push(LessonRecord {
            text: lesson.text.to_owned(),
            at: SHIPPED_AT.to_owned(),
            cycle: state.cycle,
            re_recordings: 0,
            recurrences: Vec::new(),
            escalated: None,
            id: Some(lesson.id.to_owned()),
            source: Some(SHIPPED_SOURCE.to_owned()),
        });
        state.lessons.push(lesson.text.to_owned());
        seeded.push(lesson.id);
    }
    // The same bounded-store discipline `record_lesson` applies: overflow
    // drains from the OLDEST end, so a full base rotates its oldest entries
    // and never the just-seeded rules.
    let overflow = state
        .lesson_records
        .len()
        .saturating_sub(MAX_LESSON_RECORDS);
    if overflow > 0 {
        state.lesson_records.drain(0..overflow);
    }
    let overflow = state.lessons.len().saturating_sub(MAX_PROMPT_LESSONS);
    if overflow > 0 {
        state.lessons.drain(0..overflow);
    }
    seeded
}

#[cfg(test)]
mod bootstrap_lessons_tests {
    use super::{seed_lessons, DEPTH_BAR_LESSON, SHIPPED_AT, SHIPPED_LESSONS, SHIPPED_SOURCE};
    use crate::prompts;
    use crate::state::{LessonRecord, ProjectState, MAX_LESSON_RECORDS, MAX_PROMPT_LESSONS};

    /// AC4 canary: the shipped depth bar IS the house standards' own bullet,
    /// byte for byte. A prompts.rs edit that moves it fails here and must be
    /// re-shipped consciously — the two copies may never silently diverge.
    #[test]
    fn the_shipped_depth_bar_is_the_house_standards_bullet_verbatim() {
        let standards = prompts::ENGINEERING_STANDARDS;
        let start = standards
            .find("- DEPTH BAR")
            .expect("the depth bar bullet is in ENGINEERING_STANDARDS");
        let end = standards[start..]
            .find("\n- An unclear ticket")
            .expect("the depth bar bullet ends at the next house rule");
        assert_eq!(
            DEPTH_BAR_LESSON,
            standards[start..start + end].trim(),
            "the shipped depth bar drifted from prompts::ENGINEERING_STANDARDS — \
             re-ship the bootstrap copy together with the standards"
        );
    }

    /// AC5 seed-on-empty: a fresh project's lesson base comes out of the box
    /// with the whole shipped set, every record id-ed and marked shipped,
    /// none of it a fabricated re-learning.
    #[test]
    fn seed_on_an_empty_state_installs_the_whole_shipped_base() {
        let mut s = ProjectState::default();
        assert!(s.lessons.is_empty() && s.lesson_records.is_empty());
        let seeded = seed_lessons(&mut s);
        assert_eq!(
            seeded,
            SHIPPED_LESSONS.iter().map(|l| l.id).collect::<Vec<_>>(),
            "a fresh install seeds every shipped id"
        );
        assert_eq!(s.lessons.len(), SHIPPED_LESSONS.len());
        for lesson in SHIPPED_LESSONS {
            assert!(
                s.lessons.iter().any(|l| l.as_str() == lesson.text),
                "{} reached the prompt-facing list",
                lesson.id
            );
        }
        assert!(
            s.lesson_records.iter().all(|r| r.id.is_some()
                && r.source.as_deref() == Some(SHIPPED_SOURCE)
                && r.at == SHIPPED_AT),
            "every seeded record is id-ed, marked shipped, and stamped with the \
             ship date — never boot time (that would drift per install)"
        );
        assert!(
            !s.lesson_records.iter().any(LessonRecord::is_repeating),
            "seeding is not a re-learning signal"
        );
    }

    /// AC5 seed-idempotence: a second boot seeds nothing and never doubles.
    #[test]
    fn seed_twice_is_idempotent_a_second_boot_seeds_nothing() {
        let mut s = ProjectState::default();
        assert!(!seed_lessons(&mut s).is_empty(), "the first boot seeds");
        assert!(
            seed_lessons(&mut s).is_empty(),
            "a second boot seeds nothing"
        );
        assert_eq!(
            s.lessons.len(),
            SHIPPED_LESSONS.len(),
            "no doubled prompt entries"
        );
        assert_eq!(
            s.lesson_records.len(),
            SHIPPED_LESSONS.len(),
            "no doubled ledger records"
        );
    }

    /// AC5 merge-with-existing: runtime-learned and operator-edited lessons
    /// keep their entries and records untouched — position, stamp, count.
    #[test]
    fn seed_with_existing_lessons_preserves_runtime_and_operator_text_verbatim() {
        let mut s = ProjectState::default();
        assert!(s.record_lesson("runtime-learned: rebase before pushing a conflict resolution"));
        // The operator edits state.json directly (no lessons-edit endpoint):
        // arbitrary text in the persisted prompt list IS an operator lesson.
        s.lessons
            .push("operator-edited: keep the retro working agreement verbatim".to_owned());
        let seeded = seed_lessons(&mut s);
        assert_eq!(seeded.len(), SHIPPED_LESSONS.len());
        assert_eq!(
            s.lessons.first().map(String::as_str),
            Some("runtime-learned: rebase before pushing a conflict resolution"),
            "existing entries keep their position — never reordered or rewritten"
        );
        assert!(
            s.lessons
                .contains(&"operator-edited: keep the retro working agreement verbatim".to_owned()),
            "the operator's text survives seeding verbatim"
        );
        let runtime = s
            .lesson_records
            .iter()
            .find(|r| r.text.contains("runtime-learned"))
            .expect("the runtime record exists");
        assert_eq!(
            (
                runtime.re_recordings,
                runtime.source.as_deref(),
                runtime.id.as_deref()
            ),
            (0, None, None),
            "seeding never re-stamps, re-counts or re-marks a runtime record"
        );
    }

    /// AC5 upgrade: a newer binary adds only the ids the state lacks;
    /// previously shipped records are never re-stamped or rewritten.
    #[test]
    fn seed_after_an_upgrade_adds_only_the_new_shipped_ids() {
        let mut s = ProjectState::default();
        // What an older binary left behind: the first three shipped lessons.
        for lesson in &SHIPPED_LESSONS[..3] {
            s.lesson_records.push(LessonRecord {
                text: lesson.text.to_owned(),
                at: "2026-09-01T00:00:00Z".to_owned(),
                cycle: 0,
                re_recordings: 0,
                recurrences: Vec::new(),
                escalated: None,
                id: Some(lesson.id.to_owned()),
                source: Some(SHIPPED_SOURCE.to_owned()),
            });
            s.lessons.push(lesson.text.to_owned());
        }
        let seeded = seed_lessons(&mut s);
        assert_eq!(
            seeded,
            SHIPPED_LESSONS[3..]
                .iter()
                .map(|l| l.id)
                .collect::<Vec<_>>(),
            "an upgrade adds only the new ids"
        );
        for old in &SHIPPED_LESSONS[..3] {
            let record = s
                .lesson_records
                .iter()
                .find(|r| r.id.as_deref() == Some(old.id))
                .expect("the previously shipped record survives");
            assert_eq!(record.at, "2026-09-01T00:00:00Z", "never re-stamped");
            assert_eq!(record.text, old.text, "never rewritten");
            assert_eq!(record.re_recordings, 0, "never re-counted");
        }
        assert_eq!(s.lessons.len(), SHIPPED_LESSONS.len());
    }

    /// The cap discipline matches `record_lesson`: a full prompt list
    /// rotates its OLDEST entries while every shipped rule survives — and
    /// the efficacy ledger keeps the rotated history.
    #[test]
    fn seed_on_a_full_prompt_list_rotates_the_oldest_and_keeps_every_shipped_rule() {
        let mut s = ProjectState::default();
        for i in 0..MAX_PROMPT_LESSONS {
            s.record_lesson(&format!("runtime lesson {i}: pin the dependency"));
        }
        assert_eq!(s.lessons.len(), MAX_PROMPT_LESSONS);
        let seeded = seed_lessons(&mut s);
        assert_eq!(seeded.len(), SHIPPED_LESSONS.len());
        assert_eq!(
            s.lessons.len(),
            MAX_PROMPT_LESSONS,
            "the prompt list stays bounded"
        );
        for lesson in SHIPPED_LESSONS {
            assert!(
                s.lessons.iter().any(|l| l.as_str() == lesson.text),
                "{} survives the cap",
                lesson.id
            );
        }
        assert_eq!(
            s.lesson_records.len(),
            MAX_PROMPT_LESSONS + SHIPPED_LESSONS.len(),
            "the ledger (cap {MAX_LESSON_RECORDS}) still carries the rotated \
             runtime history"
        );
    }

    /// AC4 executable: role prompts composed on a fresh install carry the
    /// depth bar and the engine-vs-task failure rule — via the existing
    /// team-memory transport, zero new plumbing.
    #[test]
    fn seed_on_a_fresh_state_puts_the_depth_bar_and_engine_vs_task_rule_into_role_prompts() {
        let mut s = ProjectState::default();
        assert!(!seed_lessons(&mut s).is_empty(), "the fresh state seeds");
        let block = prompts::team_memory_block(&[], &s.lessons);
        assert!(block.contains("- [lesson] "), "lessons render as lessons");
        assert!(
            block.contains("Two or more missing = not done."),
            "the depth bar reaches the fresh-install role prompt: {block}"
        );
        assert!(
            block.contains("not the ticket's fault"),
            "the engine-vs-task failure rule reaches the fresh-install role prompt: {block}"
        );
    }
}
