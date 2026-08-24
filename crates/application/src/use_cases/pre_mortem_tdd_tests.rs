//! TDD scaffolding for CXA-F019: Pre-mortem Analysis (risk assessment before
//! commitment).
//!
//! AC4 (complexity=Small → deterministic heuristics) is encoded here as pure
//! predicates over existing [`Ticket`] getters: `complexity()`, `depends_on()`,
//! `description()`. The other acceptance criteria need an analysis/pre-mortem
//! field on the Ticket aggregate that does not exist yet, or are presentation /
//! e2e concerns — nothing here fabricates them.
#[cfg(test)]
mod pre_mortem_tdd {
    use coxagent_domain::{Complexity, Priority, Role, Ticket, TicketId, TicketType};

    fn ticket(id: &str, desc: &str) -> Ticket {
        Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Feature,
            "premortem probe",
            desc,
            Priority::Medium,
            Complexity::Small,
            false,
        )
        .expect("ticket")
    }

    fn word_count(s: &str) -> usize {
        s.split_whitespace().count()
    }

    /// Blocked depends_on risk applies only to complexity=Small tickets.
    fn qualifies_blocker_risk(t: &Ticket) -> bool {
        t.complexity() == Complexity::Small && !t.depends_on().is_empty()
    }

    /// A Small ticket whose description crosses 500 words exposes unclear-scope
    /// risk per AC4.
    fn description_exceeds_five_hundred_words(t: &Ticket) -> bool {
        t.complexity() == Complexity::Small && word_count(t.description()) > 500
    }

    #[test]
    fn small_with_dependency_flags_blocker_risk() {
        let mut t = ticket("CXA-F002", "small but depends on another");
        t.add_dependency(Role::Sa, TicketId::new("CXA-F001").expect("id"))
            .expect("sa may add dep");

        assert_eq!(t.complexity(), Complexity::Small);
        assert_eq!(t.depends_on().len(), 1);
        assert!(qualifies_blocker_risk(&t));
    }

    #[test]
    fn small_without_dependency_fires_no_blocker_risk() {
        let t = ticket("CXA-F003", "leaf small ticket");

        assert_eq!(t.depends_on().len(), 0);
        assert_eq!(t.complexity(), Complexity::Small);
        assert!(!qualifies_blocker_risk(&t));
    }

    #[test]
    fn word_count_counts_real_descriptions() {
        let five_hundred = vec![String::from("word"); 500].join(" ");
        let six_hundred = vec![String::from("word"); 600].join(" ");

        assert_eq!(word_count(&five_hundred), 500);
        assert_eq!(word_count(&six_hundred), 600);
        // Boundary: exactly 500 words does NOT exceed five hundred words.
        let boundary = ticket("CXA-F007", &five_hundred);
        assert!(!description_exceeds_five_hundred_words(&boundary));

        // One word past the threshold DOES qualify for unclear-scope risk.
        let over = ticket("CXA-F008", &vec![String::from("word"); 501].join(" "));
        assert!(description_exceeds_five_hundred_words(&over));
    }
}
