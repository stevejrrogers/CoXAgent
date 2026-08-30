//! How risky is it to let this ticket through without a person?
//!
//! A PURE function of the ticket and what the board already shipped — no
//! model call, no network, so the answer is the same every time and testable
//! with a struct literal. See docs/ADAPTIVE_APPROVAL.md for the policy this
//! implements.

use coxagent_domain::{Complexity, Ticket, TicketType};

/// Paths where a change reaches beyond the product into how it is built,
/// shipped or governed. Same list the merge policy holds humans for.
const SENSITIVE: &[&str] = &[
    ".github/workflows",
    "Dockerfile",
    "docker-compose",
    "scripts/",
    "Cargo.toml",
    "coxagent.json",
    "deploy/",
];

/// What the adaptive gate should do with a designed ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Routine: let it through, announce it, allow an undo.
    Auto,
    /// Ask a person.
    Ask,
}

/// The verdict, with the reason a human will read in the announcement.
#[derive(Debug, Clone)]
pub struct RiskVerdict {
    pub lane: Lane,
    /// 0 (trivial) … 100 (do not touch without a person).
    pub score: u8,
    /// One sentence, plain language, for the `#approvals` post.
    pub why: String,
}

/// Score a designed ticket. `shipped_similar` is how many tickets of the same
/// shape already reached a terminal good state — prior art is the strongest
/// evidence that a class is routine.
#[must_use]
pub fn assess(ticket: &Ticket, shipped_similar: usize, previously_parked: bool) -> RiskVerdict {
    let mut score: i32 = 20;
    let mut notes: Vec<String> = Vec::new();

    match ticket.complexity() {
        // Large work shapes the system; a person sees it, always.
        Complexity::Large => {
            score += 60;
            notes.push("large".to_owned());
        }
        Complexity::Medium => score += 15,
        Complexity::Small => score -= 5,
    }

    let files: Vec<&str> = ticket
        .design()
        .technical
        .as_ref()
        .map(|d| d.files.iter().map(String::as_str).collect())
        .unwrap_or_default();
    if files
        .iter()
        .any(|f| SENSITIVE.iter().any(|s| f.contains(s)))
    {
        score += 60;
        notes.push("touches build/deploy config".to_owned());
    }
    if files.len() > 8 {
        score += 20;
        notes.push(format!("{} files", files.len()));
    }

    if ticket.acceptance_criteria().is_empty() {
        score += 25;
        notes.push("no acceptance criteria".to_owned());
    }

    // Test/doc/chore work changes what we KNOW about the system, not what it
    // does at runtime — the cheapest class to let through.
    let routine_type = ticket.ticket_type() == TicketType::Chore
        || title_is_testish(ticket.title())
        || title_is_docish(ticket.title());
    if routine_type {
        score -= 25;
        notes.push("test/docs/chore".to_owned());
    }

    if files_are_all_tests(&files) {
        score -= 20;
        notes.push("test files only".to_owned());
    }

    if previously_parked {
        score += 30;
        notes.push("previously parked".to_owned());
    }

    // Prior art: each similar ticket already shipped is evidence this shape
    // works here, capped so history can never fully outvote the signals above.
    let credit = i32::try_from(shipped_similar.min(6)).unwrap_or(0) * 5;
    score -= credit;
    if shipped_similar >= 2 {
        notes.push(format!("{shipped_similar} similar shipped"));
    }

    let score = u8::try_from(score.clamp(0, 100)).unwrap_or(100);
    // Hard floors: no amount of history auto-approves large or sensitive work.
    let hard_ask = ticket.complexity() == Complexity::Large
        || files
            .iter()
            .any(|f| SENSITIVE.iter().any(|s| f.contains(s)));
    // 35, not 25: the first live round showed a human approving every
    // test-only ticket in the queue while the score sat just above the line.
    // The hard floors below (large, build/deploy paths) do the real guarding.
    let lane = if !hard_ask && score <= 35 {
        Lane::Auto
    } else {
        Lane::Ask
    };
    let why = if notes.is_empty() {
        "ordinary change".to_owned()
    } else {
        notes.join(", ")
    };
    RiskVerdict { lane, score, why }
}

/// Shared with the filing-time feasibility preview (CXA-F250), which reads
/// the same title signals so the two estimates speak one language.
pub(crate) fn title_is_testish(t: &str) -> bool {
    // Any ticket whose SUBJECT is testing — the earlier phrase list missed
    // "Add compress+content-retrieval integration test", which a human then
    // approved without a second thought.
    let t = t.to_lowercase();
    t.split(|c: char| !c.is_alphanumeric())
        .any(|w| w == "test" || w == "tests" || w == "testing")
}

/// Whether every file the design touches lives in test code. Such a change
/// cannot alter runtime behaviour: the strongest routine signal there is.
fn files_are_all_tests(files: &[&str]) -> bool {
    !files.is_empty()
        && files.iter().all(|f| {
            let f = f.to_lowercase();
            f.contains("/tests/")
                || f.starts_with("tests/")
                || f.ends_with("_test.rs")
                || f.ends_with("_tests.rs")
                || f.contains(".test.")
                || f.contains("/e2e/")
        })
}

/// Shared with the filing-time feasibility preview (CXA-F250) — see
/// [`title_is_testish`].
pub(crate) fn title_is_docish(t: &str) -> bool {
    let t = t.to_lowercase();
    t.contains("document") || t.contains("docs") || t.contains("readme")
}

/// A coarse shape key for "tickets like this one" — used both to count prior
/// art and to group the preference samples. Deliberately crude: the type,
/// the complexity, and whether it is test/doc work.
#[must_use]
pub fn shape_key(ticket: &Ticket) -> String {
    let kind = if title_is_testish(ticket.title()) {
        "test"
    } else if title_is_docish(ticket.title()) {
        "docs"
    } else {
        match ticket.ticket_type() {
            TicketType::Bug => "bug",
            TicketType::Chore => "chore",
            TicketType::Feature => "feature",
        }
    };
    format!("{kind}/{:?}", ticket.complexity()).to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use coxagent_domain::{Priority, Role, TechnicalDesign, TicketId};

    fn ticket(title: &str, kind: TicketType, cx: Complexity, ac: bool) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new("COX-F001").expect("id"),
            kind,
            title.to_owned(),
            "body".to_owned(),
            Priority::Medium,
            cx,
            false,
        )
        .expect("ticket");
        if ac {
            t.set_acceptance_criteria(vec!["it works".to_owned()]);
        }
        t
    }

    fn design(t: &mut Ticket, files: Vec<&str>) {
        t.set_technical_design(
            Role::Sa,
            TechnicalDesign {
                approach: "do it".to_owned(),
                files: files.into_iter().map(str::to_owned).collect(),
                api_contract: String::new(),
                test_plan: "cargo test".to_owned(),
                alternatives: String::new(),
                data_changes: String::new(),
            },
        )
        .expect("design");
    }

    #[test]
    fn routine_test_coverage_with_prior_art_goes_auto() {
        let mut t = ticket(
            "Test coverage: auth adapter",
            TicketType::Chore,
            Complexity::Small,
            true,
        );
        design(&mut t, vec!["crates/app/src/auth.rs"]);
        let v = assess(&t, 5, false);
        assert_eq!(v.lane, Lane::Auto, "{v:?}");
    }

    #[test]
    fn large_work_always_asks_however_much_prior_art() {
        let mut t = ticket(
            "Rewrite the scheduler",
            TicketType::Feature,
            Complexity::Large,
            true,
        );
        design(&mut t, vec!["crates/app/src/lib.rs"]);
        assert_eq!(assess(&t, 50, false).lane, Lane::Ask);
    }

    #[test]
    fn touching_build_config_always_asks() {
        let mut t = ticket(
            "Test coverage: build",
            TicketType::Chore,
            Complexity::Small,
            true,
        );
        design(&mut t, vec!["Dockerfile"]);
        let v = assess(&t, 20, false);
        assert_eq!(v.lane, Lane::Ask);
        assert!(v.why.contains("build/deploy"), "{v:?}");
    }

    #[test]
    fn missing_acceptance_criteria_and_parking_push_toward_asking() {
        let mut t = ticket(
            "Add a widget",
            TicketType::Feature,
            Complexity::Small,
            false,
        );
        design(&mut t, vec!["src/w.rs"]);
        assert_eq!(assess(&t, 0, false).lane, Lane::Ask);
        assert_eq!(assess(&t, 0, true).lane, Lane::Ask);
    }

    #[test]
    fn a_test_only_change_is_routine_even_without_the_magic_words() {
        // The exact ticket a human waved through while the gate held it.
        let mut t = ticket(
            "Add compress+content-retrieval integration test",
            TicketType::Feature,
            Complexity::Medium,
            false,
        );
        design(&mut t, vec!["crates/app/tests/compress_integration.rs"]);
        assert_eq!(assess(&t, 0, false).lane, Lane::Auto);
    }

    #[test]
    fn criteria_alone_move_a_medium_feature_into_the_auto_lane() {
        // The live pile-up: six medium features, no acceptance criteria, all
        // scored 60 and queued. The BA writing criteria is what makes them
        // judgeable — and judgeable routine work should not need a person.
        let mut without = ticket(
            "Bug triage and burndown",
            TicketType::Feature,
            Complexity::Medium,
            false,
        );
        design(&mut without, vec!["docs/triage.md"]);
        assert_eq!(assess(&without, 0, false).lane, Lane::Ask);

        let mut with = ticket(
            "Bug triage and burndown",
            TicketType::Feature,
            Complexity::Medium,
            true,
        );
        design(&mut with, vec!["docs/triage.md"]);
        assert_eq!(
            assess(&with, 0, false).lane,
            Lane::Auto,
            "{:?}",
            assess(&with, 0, false)
        );
    }

    #[test]
    fn shape_key_groups_like_with_like() {
        let a = ticket(
            "Test coverage: x",
            TicketType::Chore,
            Complexity::Small,
            true,
        );
        let b = ticket(
            "Add test for y",
            TicketType::Feature,
            Complexity::Small,
            true,
        );
        assert_eq!(shape_key(&a), shape_key(&b), "both are small test work");
    }
}
