// Part of the cycle module split by concern — see cycle/mod.rs.
//! Learning from merged-then-reverted work (CXA-F047).
//!
//! A `git revert` that lands on the base branch undoes already-shipped work:
//! it passed every gate, merged, and then broke anyway. Until now that class
//! of failure was invisible to the loop — the forge only reports merges and
//! unmerged closes, never an undo of a merge. One leader pass per cycle reads
//! recent history through [`GitPort::raw`], links each revert to the ticket
//! whose deploy record it undoes (within `workflow.revert_scan_days`), and
//! records it as first-class state with a human approve/dismiss verdict.
//! Only approved events may influence planning — a detection is a suspicion;
//! a human makes it a fact.

use super::RunCycleUseCase;
use crate::ports::outbound::GitPort;
use crate::state::{now_rfc3339, DeployRecord, RevertDecision, RevertEvent};
use coxagent_domain::{Ticket, TicketType};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// One revert commit the scan attributed to a shipped ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RevertHit {
    pub sha: String,
    pub subject: String,
    /// RFC3339 commit date of the revert, as git reported it.
    pub reverted_at: String,
    pub ticket: String,
}

/// git's own revert subjects: `Revert "…"` (the CLI's default) or a
/// `revert:`/`Revert ` conventional prefix. Anything else is not an undo.
fn is_revert_subject(subject: &str) -> bool {
    let s = subject.trim();
    let lower = s.to_lowercase();
    lower.starts_with("revert \"") || lower.starts_with("revert:") || lower.starts_with("revert ")
}

/// Ticket-id-shaped tokens in free text (`CXA-F041`, `CXC-B001`, `FEAT-001`):
/// 2–10 uppercase-alphanumeric project prefix, one dash, then 1–6 letters and
/// 1–6 digits. Conservative by shape — a match only matters when a deploy
/// record carries the same id, so prose like "RFC-2119" cannot misattribute.
fn ticket_ids_in(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut token = String::new();
    let flush = |token: &mut String, ids: &mut Vec<String>| {
        if is_ticket_shaped(token) && !ids.contains(token) {
            ids.push(std::mem::take(token));
        } else {
            token.clear();
        }
    };
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            token.push(ch);
        } else {
            flush(&mut token, &mut ids);
        }
    }
    flush(&mut token, &mut ids);
    ids
}

fn is_ticket_shaped(token: &str) -> bool {
    let Some((prefix, suffix)) = token.split_once('-') else {
        return false;
    };
    let upper = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_uppercase());
    let alnum = |s: &str, max: usize| {
        !s.is_empty()
            && s.len() <= max
            && s.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    };
    alnum(prefix, 10)
        && upper(prefix)
        && (2..=17).contains(&token.len())
        && suffix.len() >= 2
        && suffix.len() <= 12
        && suffix
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
        && suffix.chars().last().is_some_and(|c| c.is_ascii_digit())
        && suffix
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// Parse `git log --format=%H%x1f%cI%x1f%s` output. The unit separator keeps
/// subjects with tabs intact; a tab-separated line (the design's `%x09`
/// shape) parses too. A line without a date code still yields its subject —
/// the window filter, not the parser, decides what is usable.
fn parse_log_commits(out: &str) -> Vec<(String, Option<String>, String)> {
    let fields = |l: &str, sep: char| {
        let mut parts = l.splitn(3, sep);
        let sha = parts.next().unwrap_or_default().trim().to_owned();
        let date = parts.next().map(str::trim).filter(|d| !d.is_empty());
        let subject = parts.next().unwrap_or_default().to_owned();
        (sha, date.map(str::to_owned), subject)
    };
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let by_unit = fields(l, '\u{1f}');
            if by_unit.1.is_some() {
                return by_unit;
            }
            let by_tab = fields(l, '\t');
            if by_tab.1.is_some() {
                by_tab
            } else {
                (by_unit.0, None, by_unit.2)
            }
        })
        .collect()
}

fn parse_rfc3339(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s.trim(), &Rfc3339).ok()
}

/// The role that ships a ticket's work — the same mapping the merge sync
/// applies (a bug reaches `Fixed` through DEV-BUG, anything else reaches
/// `Done` through DEV-FEATURE). A pruned ticket defaults to the feature dev:
/// the deploy record proves SOMETHING shipped, only its agent label is lost.
fn shipping_role_label(tickets: &[Ticket], id: &str) -> String {
    let bug = tickets
        .iter()
        .any(|t| t.id().as_str() == id && t.ticket_type() == TicketType::Bug);
    if bug {
        "DEV-BUG".to_owned()
    } else {
        "DEV-FEATURE".to_owned()
    }
}

/// Pure detection: scan `git log` output for revert commits attributable to a
/// shipped ticket whose deploy record sits within `window_days` of the revert
/// commit. Attribution prefers the ticket named in the revert subject and
/// falls back to the subject of the commit the revert undoes (via
/// `reverted_subject_of`, the orchestration's one git read); several matching
/// deploys resolve to the one closest in time — when tickets shared files,
/// the nearest ship date is the honest best guess. Unattributable reverts are
/// skipped: a suspicion without a ticket is not feedback.
pub(super) fn detect_reverts(
    log_out: &str,
    deploys: &[DeployRecord],
    window_days: u64,
    reverted_subject_of: &dyn Fn(&str) -> Option<String>,
) -> Vec<RevertHit> {
    let window = time::Duration::days(i64::try_from(window_days).unwrap_or(i64::MAX));
    let mut hits = Vec::new();
    for (sha, date, subject) in parse_log_commits(log_out) {
        if !is_revert_subject(&subject) {
            continue;
        }
        let Some(revert_at) = date.as_deref().and_then(parse_rfc3339) else {
            continue;
        };
        let mut ids = ticket_ids_in(&subject);
        if ids.is_empty() {
            if let Some(original) = reverted_subject_of(&sha) {
                ids = ticket_ids_in(&original);
            }
        }
        let deploy = deploys
            .iter()
            .filter(|d| ids.iter().any(|id| id == d.ticket.as_str()))
            .filter_map(|d| parse_rfc3339(&d.at).map(|at| (d, at)))
            .filter(|(_, at)| (revert_at - *at).abs() <= window)
            .min_by_key(|(_, at)| (revert_at - *at).abs());
        if let Some((d, _)) = deploy {
            hits.push(RevertHit {
                sha,
                subject,
                reverted_at: date.unwrap_or_default(),
                ticket: d.ticket.to_string(),
            });
        }
    }
    hits
}

impl<S: crate::ports::outbound::StateStorePort, E: crate::ports::outbound::AgentEnginePort>
    RunCycleUseCase<S, E>
{
    /// The F047 pass: zero tokens, leader-only (it writes shared state).
    /// Git reads go exclusively through [`GitPort::raw`] — the same door the
    /// rest of forge hygiene uses, so no new IO surface exists to ratchet.
    pub(super) async fn learn_reverted_work(&self) {
        let Some(git) = &self.git else {
            return;
        };
        if !self.config.git.enabled {
            return;
        }
        // A bounded tail keeps the read cheap on long histories; the N-day
        // window filter, not the commit count, is the policy knob.
        let (ok, out) = git
            .raw(
                &self.work_dir,
                &[
                    "log",
                    "-n",
                    "400",
                    "--format=%H%x1f%cI%x1f%s",
                    self.flow_base(),
                ],
            )
            .await;
        if !ok {
            return;
        }
        // Reverts whose subject names no ticket get ONE fallback read each —
        // the subject of the commit they undo. Bounded so a history of
        // unattributable reverts cannot turn into a scan storm.
        let mut resolved = std::collections::BTreeMap::new();
        for (sha, _, subject) in parse_log_commits(&out) {
            if resolved.len() >= 10 {
                break;
            }
            if is_revert_subject(&subject) && ticket_ids_in(&subject).is_empty() {
                if let Some(original) = self.reverted_commit_subject(git.as_ref(), &sha).await {
                    resolved.insert(sha, original);
                }
            }
        }
        let Ok(s) = self.store.load().await else {
            return;
        };
        let window_days = self.config.workflow.revert_scan_days();
        let detected: Vec<(RevertHit, String)> =
            detect_reverts(&out, &s.history, window_days, &|sha| {
                resolved.get(sha).cloned()
            })
            .into_iter()
            .map(|h| {
                let role = shipping_role_label(&s.tickets, &h.ticket);
                (h, role)
            })
            .collect();
        if detected.is_empty() {
            return;
        }
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
            for (h, role) in &detected {
                // `record_revert` dedupes on the commit sha: a re-scan never
                // re-flags (or re-announces) an event a human already decided.
                let fresh = s.record_revert(RevertEvent {
                    sha: h.sha.clone(),
                    subject: h.subject.clone(),
                    ticket: h.ticket.clone(),
                    role: role.clone(),
                    reverted_at: h.reverted_at.clone(),
                    detected_at: now_rfc3339(),
                    decision: RevertDecision::Pending,
                    decided_at: None,
                    decided_by: None,
                });
                if fresh {
                    // The Overview feed's 'reverted work' line — posted once.
                    s.log_activity(
                        role,
                        &format!("reverted work: {}", h.subject),
                        Some(h.ticket.clone()),
                    );
                }
            }
            Ok(())
        })
        .await;
    }

    /// The subject of the commit a revert undoes — git names it in the
    /// revert's body (`This reverts commit <sha>`). `None` when git cannot
    /// answer: the revert stays unattributed rather than guessed at.
    async fn reverted_commit_subject(&self, git: &dyn GitPort, revert_sha: &str) -> Option<String> {
        let (_, body) = git
            .raw(&self.work_dir, &["show", "-s", "--format=%B", revert_sha])
            .await;
        let target = body.lines().find_map(|l| {
            l.trim()
                .strip_prefix("This reverts commit ")
                .and_then(|rest| rest.split_whitespace().next())
                .filter(|sha| !sha.is_empty())
        })?;
        let (ok, subject) = git
            .raw(&self.work_dir, &["show", "-s", "--format=%s", target])
            .await;
        ok.then(|| subject.trim().to_owned())
            .filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod revert_learning_tests {
    use super::*;
    use crate::state::RevertEvent;

    /// The subject a revert commit's ticket id is read from when its own
    /// subject names none — pure tests resolve nothing and skip the fallback.
    fn no_resolution(_: &str) -> Option<String> {
        None
    }

    fn deploy(ticket: &str, at: &str) -> DeployRecord {
        DeployRecord {
            version: coxagent_domain::SemVer::new(1, 0, 0),
            ticket: coxagent_domain::TicketId::new(ticket).unwrap(),
            title: "add widget".to_owned(),
            at: at.to_owned(),
        }
    }

    fn line(sha: &str, at: &str, subject: &str) -> String {
        format!("{sha}\u{1f}{at}\u{1f}{subject}")
    }

    #[test]
    fn revert_subjects_are_recognized_in_both_dialects() {
        assert!(is_revert_subject("Revert \"feat(CXA-F041): add widget\""));
        assert!(is_revert_subject("revert: add widget"));
        assert!(is_revert_subject("Revert work on the widget"));
        assert!(!is_revert_subject("feat(CXA-F041): add widget"));
        assert!(!is_revert_subject("prevent revert of the docs"));
    }

    #[test]
    fn ticket_ids_are_read_from_subjects_only_when_ticket_shaped() {
        assert_eq!(
            ticket_ids_in("Revert \"feat(CXA-F041): add widget\""),
            vec!["CXA-F041".to_owned()]
        );
        assert_eq!(
            ticket_ids_in("Revert \"feat: add widget\""),
            Vec::<String>::new()
        );
        // A digit-led suffix ("RFC-2119") is not a ticket id here — every
        // minted id starts its suffix with the type letter (F/B/C).
        assert_eq!(ticket_ids_in("align with RFC-2119"), Vec::<String>::new());
        assert_eq!(ticket_ids_in("see the 404 page"), Vec::<String>::new());
    }

    #[test]
    fn log_lines_parse_in_both_separators() {
        let parsed = parse_log_commits(&format!(
            "{}\n{}\n",
            line("c0ffee1", "2026-08-20T00:00:00Z", "Revert \"x\""),
            "badc0de\t2026-08-19T00:00:00Z\tfeat: y"
        ));
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "c0ffee1");
        assert_eq!(parsed[0].1.as_deref(), Some("2026-08-20T00:00:00Z"));
        assert_eq!(parsed[1].2, "feat: y");
    }

    #[test]
    fn in_window_revert_is_attributed_to_the_deployed_ticket() {
        let out = line(
            "c0ffee1",
            "2026-08-20T12:00:00Z",
            "Revert \"feat(CXA-F041): add widget\"",
        );
        let deploys = [deploy("CXA-F041", "2026-08-19T00:00:00Z")];
        let hits = detect_reverts(&out, &deploys, 30, &no_resolution);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].ticket, "CXA-F041");
        assert_eq!(hits[0].sha, "c0ffee1");
    }

    #[test]
    fn out_of_window_reverts_are_not_detected() {
        let out = line(
            "badc0de",
            "2020-01-01T00:00:00Z",
            "Revert \"feat(CXA-F041): add widget\"",
        );
        let deploys = [deploy("CXA-F041", "2026-08-19T00:00:00Z")];
        assert!(detect_reverts(&out, &deploys, 30, &no_resolution).is_empty());
    }

    #[test]
    fn attribution_falls_back_to_the_reverted_commit_subject() {
        let out = line(
            "c0ffee1",
            "2026-08-20T12:00:00Z",
            "Revert \"feat: add widget\"",
        );
        let deploys = [deploy("CXA-F041", "2026-08-19T00:00:00Z")];
        let resolve =
            |sha: &str| (sha == "c0ffee1").then(|| "feat(CXA-F041): add widget".to_owned());
        let hits = detect_reverts(&out, &deploys, 30, &resolve);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].ticket, "CXA-F041");
    }

    #[test]
    fn several_matching_deploys_resolve_to_the_closest_one() {
        let out = line(
            "c0ffee1",
            "2026-08-20T12:00:00Z",
            "Revert \"fix(CXA-B001): patch\"",
        );
        let deploys = [
            deploy("CXA-B001", "2026-08-01T00:00:00Z"),
            deploy("CXA-B001", "2026-08-19T00:00:00Z"),
        ];
        let hits = detect_reverts(&out, &deploys, 30, &no_resolution);
        assert_eq!(hits.len(), 1, "one revert commit is one event");
    }

    #[test]
    fn the_same_ticket_reverted_twice_yields_two_events() {
        let out = format!(
            "{}\n{}",
            line(
                "aaaaaaa",
                "2026-08-20T12:00:00Z",
                "Revert \"feat(CXA-F041): w\""
            ),
            line(
                "bbbbbbb",
                "2026-08-22T12:00:00Z",
                "Revert \"revert feat(CXA-F041): w\""
            ),
        );
        let deploys = [deploy("CXA-F041", "2026-08-19T00:00:00Z")];
        let hits = detect_reverts(&out, &deploys, 30, &no_resolution);
        assert_eq!(hits.len(), 2);
        assert_ne!(hits[0].sha, hits[1].sha);
    }

    #[test]
    fn reverts_for_tickets_never_deployed_are_not_attributed() {
        let out = line(
            "c0ffee1",
            "2026-08-20T12:00:00Z",
            "Revert \"feat(CXA-F041): add widget\"",
        );
        let deploys = [deploy("CXA-B002", "2026-08-19T00:00:00Z")];
        assert!(detect_reverts(&out, &deploys, 30, &no_resolution).is_empty());
    }

    #[test]
    fn the_ledger_dedupes_by_sha_and_never_redecides() {
        let mut s = crate::state::ProjectState::default();
        let ev = |sha: &str| RevertEvent {
            sha: sha.to_owned(),
            subject: "Revert \"feat(CXA-F041): w\"".to_owned(),
            ticket: "CXA-F041".to_owned(),
            role: "DEV-FEATURE".to_owned(),
            reverted_at: "2026-08-20T12:00:00Z".to_owned(),
            detected_at: now_rfc3339(),
            decision: RevertDecision::Pending,
            decided_at: None,
            decided_by: None,
        };
        assert!(s.record_revert(ev("c0ffee1")));
        assert!(!s.record_revert(ev("c0ffee1")), "a re-scan is a no-op");
        // Attribution drift cannot fork the ledger: one sha stays one event
        // even if a later scan would link it to a different ticket.
        let mut drifted = ev("c0ffee1");
        drifted.ticket = "CXA-B002".to_owned();
        assert!(!s.record_revert(drifted), "one git undo is one event");
        assert!(s.decide_revert("c0ffee1", RevertDecision::Dismissed, "root"));
        assert!(
            !s.decide_revert("c0ffee1", RevertDecision::Approved, "root"),
            "a decided event is final"
        );
        assert!(
            s.record_revert(ev("bbbbbbb")),
            "the same ticket reverted twice is two events"
        );
        assert!(s.decide_revert("bbbbbbb", RevertDecision::Approved, "root"));
        let decisions: Vec<(String, RevertDecision)> = s
            .reverted_work
            .iter()
            .map(|e| (e.sha.clone(), e.decision))
            .collect();
        assert_eq!(
            decisions,
            vec![
                ("c0ffee1".to_owned(), RevertDecision::Dismissed),
                ("bbbbbbb".to_owned(), RevertDecision::Approved),
            ]
        );
    }

    #[test]
    fn the_shipping_role_follows_the_ticket_type() {
        use coxagent_domain::{Complexity, Priority, TicketId, TicketType};
        let mut bug = Ticket::new(
            TicketId::new("CXA-B001").unwrap(),
            TicketType::Bug,
            "fix it",
            "the fix",
            Priority::High,
            Complexity::Small,
            false,
        )
        .unwrap();
        let feature = Ticket::new(
            TicketId::new("CXA-F001").unwrap(),
            TicketType::Feature,
            "build it",
            "the build",
            Priority::Medium,
            Complexity::Small,
            false,
        )
        .unwrap();
        bug.claim(coxagent_domain::Role::DevBug, "w", "2026-08-19T00:00:00Z")
            .unwrap();
        let tickets = vec![bug, feature];
        assert_eq!(shipping_role_label(&tickets, "CXA-B001"), "DEV-BUG");
        assert_eq!(shipping_role_label(&tickets, "CXA-F001"), "DEV-FEATURE");
        assert_eq!(
            shipping_role_label(&tickets, "CXA-GONE"),
            "DEV-FEATURE",
            "a pruned ticket still attributes to the role that ships features"
        );
    }
}
