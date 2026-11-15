// LOCKDOWN: CXA-MYP-B007 — scope covers blockers AND backlog-preceding bugs.
use coxagent_application::selection::burn_down_scope;
use coxagent_domain::{Complexity, Priority, Role, TechnicalDesign, Ticket, TicketId, TicketType};

fn ready_feature(id: &str) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("id"),
        TicketType::Feature,
        "f",
        "",
        Priority::High,
        Complexity::Small,
        false,
    )
    .expect("feature");
    t.set_technical_design(Role::Sa, TechnicalDesign::default())
        .expect("design");
    t.transition_to(Role::Sa, coxagent_domain::Status::Ready)
        .expect("ready");
    t
}

#[test]
fn cxaf415_myp_b007_burndown_scope_covers_blockers_and_preceding_bugs() {
    let mut blocked = ready_feature("F415-b7-f");
    blocked
        .add_dependency(Role::Sa, TicketId::new("F415-b7-dep").expect("id"))
        .expect("dep");
    let state = coxagent_application::state::ProjectState {
        tickets: vec![
            super::open_bug("F415-b7-dep", Priority::High),
            super::open_bug("F415-b7-pre", Priority::High),
            blocked,
            super::open_bug("F415-b7-post", Priority::High),
        ],
        ..coxagent_application::state::ProjectState::default()
    };
    let scope = burn_down_scope(&state);
    let has = |s: &str| scope.iter().any(|i| i.as_str() == s);
    assert!(
        has("F415-b7-dep"),
        "CXA-MYP-B007: named dependency missing from scope"
    );
    assert!(
        has("F415-b7-pre"),
        "CXA-MYP-B007: preceding open bug missing from scope"
    );
    assert!(
        !has("F415-b7-post"),
        "CXA-MYP-B007: trailing bug must stay out of the sprint scope"
    );
}
