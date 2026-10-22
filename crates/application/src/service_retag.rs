//! CXA-F420e — retag suggestion pass (spec §4): after a service joins a live
//! project, propose which existing tickets look like they belong to it.
//! Suggestions + Apply, never silent: this module only PROPOSES; the endpoint
//! applies exactly what the human ticked.
//!
//! Pure: a snapshot of tickets and services in, a list of suggestions out.
//! The signals are deliberately conservative — a design file under one
//! service's declared scopes, or the service's name in the title as a word —
//! and a ticket matching several services gets NO suggestion (ambiguity is
//! the human's call, not a coin flip).

use crate::config::ServiceConfig;
use serde::Serialize;

/// One proposed move, with the evidence that produced it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RetagSuggestion {
    pub ticket: String,
    pub to: String,
    /// Human-readable evidence ("design file crates/x lies in scope crates/").
    pub why: String,
}

/// A ticket's view the pass needs — kept struct-literal small so tests and
/// the endpoint build it without dragging the whole domain aggregate in.
#[derive(Debug, Default, Clone)]
pub struct TicketView {
    pub id: String,
    pub title: String,
    pub service_tag: Option<String>,
    /// `design.technical.files`, when a design exists.
    pub design_files: Vec<String>,
    /// Terminal tickets are history, not backlog — never suggested.
    pub terminal: bool,
}

/// Propose retags for every open ticket that is untagged (or tagged with the
/// default/first service, which is where legacy backlog sits) and whose
/// evidence points at exactly ONE other active service.
#[must_use]
pub fn suggest(tickets: &[TicketView], services: &[ServiceConfig]) -> Vec<RetagSuggestion> {
    let active: Vec<&ServiceConfig> = services.iter().filter(|s| s.status == "active").collect();
    if active.len() < 2 {
        return Vec::new();
    }
    let default_name = active[0].name.as_str();
    let mut out = Vec::new();
    for t in tickets {
        if t.terminal {
            continue;
        }
        // Only the untagged / default-tagged backlog is eligible: an explicit
        // non-default tag was someone's decision — never second-guess it.
        if t.service_tag.as_deref().is_some_and(|s| s != default_name) {
            continue;
        }
        let mut hits: Vec<(&str, String)> = Vec::new();
        for svc in active.iter().skip(1) {
            if let Some(why) = evidence(t, svc) {
                hits.push((svc.name.as_str(), why));
            }
        }
        // Exactly one candidate service, or stay silent.
        if let [(name, why)] = hits.as_slice() {
            out.push(RetagSuggestion {
                ticket: t.id.clone(),
                to: (*name).to_owned(),
                why: why.clone(),
            });
        }
    }
    out
}

/// The evidence one ticket offers for one service, if any.
fn evidence(t: &TicketView, svc: &ServiceConfig) -> Option<String> {
    for f in &t.design_files {
        let f_norm = f.trim_start_matches("./");
        for scope in &svc.scopes {
            if !scope.is_empty() && f_norm.starts_with(scope.as_str()) {
                return Some(format!("design file {f_norm} lies in scope {scope}"));
            }
        }
    }
    if title_names_service(&t.title, &svc.name) {
        return Some(format!("title names '{}'", svc.name));
    }
    None
}

/// Whole-word, case-insensitive mention — "harxes" must not match "harxesque".
fn title_names_service(title: &str, name: &str) -> bool {
    if name.len() < 3 {
        return false; // too short to be a signal
    }
    let lt = title.to_lowercase();
    let ln = name.to_lowercase();
    let mut start = 0;
    while let Some(i) = lt[start..].find(&ln) {
        let at = start + i;
        let before_ok = at == 0 || !lt.as_bytes()[at - 1].is_ascii_alphanumeric();
        let after = at + ln.len();
        let after_ok = after >= lt.len() || !lt.as_bytes()[after].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
        start = at + ln.len();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(name: &str, scopes: &[&str]) -> ServiceConfig {
        ServiceConfig {
            name: name.into(),
            repo: String::new(),
            work_dir: String::new(),
            scopes: scopes.iter().map(|s| (*s).to_owned()).collect(),
            build_cmd: String::new(),
            test_cmd: String::new(),
            status: "active".into(),
        }
    }
    fn tk(id: &str, title: &str, tag: Option<&str>, files: &[&str]) -> TicketView {
        TicketView {
            id: id.into(),
            title: title.into(),
            service_tag: tag.map(Into::into),
            design_files: files.iter().map(|s| (*s).to_owned()).collect(),
            terminal: false,
        }
    }

    #[test]
    fn scope_evidence_beats_silence_and_names_the_file() {
        let s = suggest(
            &[tk("T-1", "fix parser", None, &["crates/engine/src/lib.rs"])],
            &[
                svc("hub", &["crates/hub/"]),
                svc("engine", &["crates/engine/"]),
            ],
        );
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].to, "engine");
        assert!(s[0].why.contains("crates/engine/"));
    }

    #[test]
    fn explicit_non_default_tags_and_terminal_tickets_are_never_second_guessed() {
        let mut done = tk("T-3", "engine cleanup", None, &[]);
        done.terminal = true;
        let s = suggest(
            &[
                tk("T-2", "engine tweak", Some("engine"), &[]),
                done,
                tk("T-4", "default-tagged engine fix", Some("hub"), &[]),
            ],
            &[svc("hub", &[]), svc("engine", &[])],
        );
        // Only T-4 (default-tagged, title evidence) is suggested.
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].ticket, "T-4");
    }

    #[test]
    fn ambiguity_or_single_service_yields_nothing() {
        // Matches two services → silent.
        let s = suggest(
            &[tk("T-5", "touch engine and web", None, &[])],
            &[svc("hub", &[]), svc("engine", &[]), svc("web", &[])],
        );
        assert!(s.is_empty());
        // One active service → pass disabled.
        let s2 = suggest(&[tk("T-6", "engine fix", None, &[])], &[svc("engine", &[])]);
        assert!(s2.is_empty());
    }

    #[test]
    fn title_match_is_whole_word_case_insensitive() {
        assert!(title_names_service("Fix Harxes connect hang", "harxes"));
        assert!(!title_names_service("harxesque behavior", "harxes"));
        assert!(!title_names_service("short hx name", "hx"));
    }
}
