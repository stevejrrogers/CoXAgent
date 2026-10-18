// Part of the run_dev module split by concern — see run_dev/mod.rs.
//! Slot-WIP checkpoint orchestration (CXA-F318).
//!
//! One seam, three callers: the dev run's release path (the ticket is known),
//! the cycle's slot-hygiene sweep (residue of unknown origin), and — once it
//! lands — the orphaned-slot reclamation sweep. Every caller wants the same
//! thing: before a slot worktree is cleaned or released, its uncommitted work
//! is committed to a LOCAL ref in the shared repo and recorded where the team
//! can find it, because an anonymous `git stash` is exactly what
//! `git stash clear` loses (recorded team lesson).
//!
//! Pure orchestration over `GitPort` + `StateStorePort` — no direct IO here.

use crate::ports::outbound::{mutate_state, GitAuthor, GitPort, StateStorePort};
use crate::state::now_rfc3339;
use coxagent_domain::{Role, Status, TicketId, WipCheckpoint};
use std::collections::BTreeMap;
use std::path::Path;

/// The local, never-pushed ref namespace for parked slot work.
pub(crate) const WIP_REF_PREFIX: &str = "refs/coxagent/wip/";
/// How many unattributed hygiene checkpoints survive a prune — bounds the
/// namespace when residue cannot be tied to a ticket.
pub(crate) const MAX_KEPT_UNATTRIBUTED: usize = 3;

/// One successful park: the record that was created (and, when the ticket was
/// known, written onto the ticket itself).
pub(crate) struct ParkedWip {
    pub checkpoint: WipCheckpoint,
}

/// The commit identity for parked work — the same bot identity the ship sweep
/// commits under, honouring the project's configured commit email.
#[must_use]
pub(crate) fn bot_author(commit_email: &str) -> GitAuthor {
    GitAuthor {
        name: "coxagent-bot".to_owned(),
        email: if commit_email.trim().is_empty() {
            "coxagent-bot@users.noreply.github.com".to_owned()
        } else {
            commit_email.trim().to_owned()
        },
    }
}

/// The checkpoint ref for `id`: one deterministic ref per ticket, so repeated
/// parks of the same ticket move the same ref (the newest sha wins).
#[must_use]
pub(crate) fn wip_ref_for(id: &TicketId) -> String {
    format!("{WIP_REF_PREFIX}{id}")
}

/// The ref for residue no ticket can be attributed to. The worktree's own
/// name (unique per slot) is part of it: two slots parking in the same second
/// must NEVER land on one ref — the second `update-ref` would silently
/// overwrite the first park before its tree is wiped. Instant-first keeps the
/// lexicographic order chronological for the prune's newest-N cap.
#[must_use]
pub(crate) fn unattributed_wip_ref(work_dir: &Path, rfc3339_now: &str) -> String {
    let slot = work_dir.file_name().map_or_else(
        || "unknown-slot".to_owned(),
        |n| n.to_string_lossy().chars().take(60).collect::<String>(),
    );
    format!(
        "{WIP_REF_PREFIX}unattributed-{}-{slot}",
        compact_instant(rfc3339_now)
    )
}

/// Whether `work_dir` is a concurrency-slot worktree rather than the leader's
/// shared checkout. Same test the cycle hygiene has always used.
#[must_use]
pub(crate) fn is_slot_worktree(work_dir: &Path) -> bool {
    work_dir
        .components()
        .any(|c| c.as_os_str() == ".coxagent-worktrees")
}

/// The human-readable one-liner stored on the checkpoint: branch + diffstat.
#[must_use]
pub(crate) fn checkpoint_note(branch: &str, diffstat: &str, untracked: usize) -> String {
    let mut parts = vec![format!("branch {branch}")];
    if !diffstat.is_empty() {
        parts.push(diffstat.to_owned());
    }
    if untracked > 0 {
        parts.push(format!("{untracked} untracked"));
    }
    parts.join("; ")
}

/// A timestamp compacted to ref-name-safe characters (`:` is illegal in refs).
#[must_use]
pub(crate) fn compact_instant(rfc3339: &str) -> String {
    rfc3339
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

/// Pure prune decision: which `refs/coxagent/wip/*` names are dead.
///
/// A ticket-named ref dies with its ticket — absent from state, or closed
/// (terminal status, the same set backlog hygiene treats as finished).
/// Unattributed checkpoints are bounded: the newest [`MAX_KEPT_UNATTRIBUTED`]
/// survive, older ones are pruned.
#[must_use]
pub(crate) fn stale_wip_refs(
    ref_names: &[String],
    tickets: &BTreeMap<String, bool>,
    keep_unattributed: usize,
) -> Vec<String> {
    let mut unattributed: Vec<&String> = Vec::new();
    let mut stale = Vec::new();
    for name in ref_names {
        let Some(leaf) = name.strip_prefix(WIP_REF_PREFIX) else {
            continue;
        };
        if leaf.starts_with("unattributed-") {
            unattributed.push(name);
        } else {
            match tickets.get(leaf) {
                // Gone from state, or closed — nothing can restore this work
                // anymore, so the ref is dead weight in the shared repo.
                None | Some(true) => stale.push((*name).clone()),
                Some(false) => {}
            }
        }
    }
    unattributed.sort();
    unattributed.reverse(); // newest (lexicographically largest) first
    stale.extend(unattributed.into_iter().skip(keep_unattributed).cloned());
    stale
}

/// Whether `status` is a closed (terminal) ticket status — the set backlog
/// hygiene already treats as finished; nothing further happens to these.
fn is_closed(status: Status) -> bool {
    matches!(
        status,
        Status::Done | Status::Documented | Status::Verified | Status::Rejected
    )
}

/// Park the uncommitted WIP in a slot worktree on a durable checkpoint ref.
///
/// A clean tree (or a non-slot tree) parks NOTHING: no commit, no ref, no
/// evidence entry. A dirty tree is committed to `refs/coxagent/wip/<ticket>`
/// (or an `unattributed-<instant>-<slot>` ref when no ticket is known), and —
/// when the ticket is known — the ref, sha and diffstat are recorded on the
/// ticket itself: the aggregate's checkpoint history, a `wip` evidence entry,
/// and a journal note telling the next attempt how to restore.
///
/// # Errors
/// The failure detail when the checkpoint commit could not be created (e.g. a
/// git index lock) or could not be recorded. The CALLER decides what a
/// failure means — the slot must still release either way.
pub(crate) async fn park_slot_wip<S: StateStorePort + ?Sized>(
    git: &dyn GitPort,
    work_dir: &Path,
    store: &S,
    ticket: Option<&TicketId>,
    author: &GitAuthor,
) -> Result<Option<ParkedWip>, String> {
    if !is_slot_worktree(work_dir) {
        return Ok(None);
    }
    let (ok, status) = git.raw(work_dir, &["status", "--porcelain"]).await;
    if !ok || status.trim().is_empty() {
        // Clean tree: no checkpoint commit and no evidence entry (AC2).
        return Ok(None);
    }
    let branch = git
        .current_branch(work_dir)
        .await
        .unwrap_or_else(|_| "HEAD".to_owned());
    let (_, stat) = git.raw(work_dir, &["diff", "HEAD", "--stat"]).await;
    let diffstat = stat
        .lines()
        .rev()
        .find(|l| l.contains("changed") && l.contains("file"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let untracked = status.lines().filter(|l| l.starts_with("?? ")).count();

    let now = now_rfc3339();
    let (ref_name, subject) = match ticket {
        Some(id) => (wip_ref_for(id), id.to_string()),
        None => (
            unattributed_wip_ref(work_dir, &now),
            "unattributed slot residue".to_owned(),
        ),
    };
    let message =
        format!("WIP checkpoint: {subject} — parked by slot hygiene (engine died mid-edit)");
    let Some(sha) = git
        .checkpoint_tree(work_dir, &ref_name, &message, author)
        .await
        .map_err(|e| format!("{subject}: {e}"))?
    else {
        // Raced clean between the status probe and the park — nothing to record.
        return Ok(None);
    };

    let checkpoint = WipCheckpoint {
        ref_name: ref_name.clone(),
        sha: sha.clone(),
        note: checkpoint_note(&branch, &diffstat, untracked),
        recorded_at: now,
    };
    if let Some(id) = ticket {
        record_on_ticket(store, id, &checkpoint).await?;
    }
    Ok(Some(ParkedWip { checkpoint }))
}

/// Write the park record onto the ticket: the aggregate's guarded checkpoint
/// history (System-only), a `wip` evidence entry the dashboards render, and a
/// journal note so the NEXT attempt restarts from the ref, not from scratch.
async fn record_on_ticket<S: StateStorePort + ?Sized>(
    store: &S,
    id: &TicketId,
    cp: &WipCheckpoint,
) -> Result<(), String> {
    let id = id.clone();
    let cp = cp.clone();
    let detail = format!("{} @ {} — {}", cp.ref_name, cp.sha, cp.note);
    let note = format!(
        "SYSTEM: WIP parked at {} ({}) — restore with `git checkout {} -- .`",
        cp.ref_name, cp.sha, cp.ref_name
    );
    let ref_name = cp.ref_name.clone();
    mutate_state(store, move |s| {
        let Some(t) = s.ticket_mut(&id) else {
            return Ok(());
        };
        t.record_wip_checkpoint(Role::System, cp.clone())
            .map_err(|e| crate::PortError::Corrupt(e.to_string()))?;
        s.add_evidence_for(id.as_str(), "wip", "WIP CHECKPOINT", &detail, &[], "SYSTEM");
        s.journal_note(id.as_str(), &note);
        Ok(())
    })
    .await
    .map_err(|e| format!("{ref_name}: recording checkpoint failed: {e}"))
}

/// Delete every checkpoint ref that has died with its ticket (closed or no
/// longer in state) and bound the unattributed namespace. Returns the refs
/// pruned, for the hygiene log. Best-effort: a ref that cannot be deleted is
/// left for the next sweep, never fatal.
pub(crate) async fn prune_ticket_wip_refs<S: StateStorePort + ?Sized>(
    git: &dyn GitPort,
    work_dir: &Path,
    store: &S,
) -> Vec<String> {
    let (ok, out) = git
        .raw(
            work_dir,
            &["for-each-ref", "--format=%(refname)", WIP_REF_PREFIX],
        )
        .await;
    if !ok {
        return Vec::new();
    }
    let refs: Vec<String> = out
        .lines()
        .map(str::to_owned)
        .filter(|l| !l.trim().is_empty())
        .collect();
    if refs.is_empty() {
        return Vec::new();
    }
    // One snapshot of state decides everything; the deletes are mechanical.
    let Ok(state) = store.load().await else {
        return Vec::new();
    };
    let tickets: BTreeMap<String, bool> = state
        .tickets
        .iter()
        .map(|t| (t.id().to_string(), is_closed(t.status())))
        .collect();
    let dead_refs = stale_wip_refs(&refs, &tickets, MAX_KEPT_UNATTRIBUTED);
    let mut pruned = Vec::new();
    let mut closed_tickets: Vec<String> = Vec::new();
    for r in &dead_refs {
        let (deleted, _) = git.raw(work_dir, &["update-ref", "-d", r]).await;
        if deleted {
            pruned.push(r.clone());
            if let Some(leaf) = r.strip_prefix(WIP_REF_PREFIX) {
                if tickets.get(leaf) == Some(&true) {
                    closed_tickets.push(leaf.to_owned());
                }
            }
        }
    }
    if !closed_tickets.is_empty() {
        // The refs are gone — the closed tickets' pointers to them go too, so
        // the aggregate never advertises a checkpoint that no longer exists.
        let _ = mutate_state(store, move |s| {
            for leaf in &closed_tickets {
                let Ok(id) = TicketId::new(leaf) else {
                    continue;
                };
                if let Some(t) = s.ticket_mut(&id) {
                    let _ = t.clear_wip_checkpoints(Role::System);
                }
            }
            Ok(())
        })
        .await;
    }
    pruned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_note_composes_branch_diffstat_and_untracked() {
        assert_eq!(
            checkpoint_note("main", "2 files changed, 3 insertions(+)", 0),
            "branch main; 2 files changed, 3 insertions(+)"
        );
        assert_eq!(
            checkpoint_note("feat/x", "", 2),
            "branch feat/x; 2 untracked"
        );
        assert_eq!(checkpoint_note("HEAD", "", 0), "branch HEAD");
    }

    #[test]
    fn compact_instant_produces_a_ref_safe_name() {
        assert_eq!(
            compact_instant("2026-09-02T10:11:12.123Z"),
            "20260902T101112123Z"
        );
    }

    #[test]
    fn wip_ref_for_namespaces_by_ticket() {
        let id = TicketId::new("BUG-2281").expect("id");
        assert_eq!(wip_ref_for(&id), "refs/coxagent/wip/BUG-2281");
    }

    #[test]
    fn unattributed_refs_differ_per_slot_even_in_the_same_second() {
        let now = "2026-09-02T10:11:12.123Z";
        let slot_a = Path::new("/hub/.coxagent-worktrees/cxa-p1-slot-1-aaaa");
        let slot_b = Path::new("/hub/.coxagent-worktrees/cxa-p1-slot-2-bbbb");
        let a = unattributed_wip_ref(slot_a, now);
        let b = unattributed_wip_ref(slot_b, now);
        assert_ne!(a, b, "two slots parking in one second must not collide");
        assert!(a.starts_with("refs/coxagent/wip/unattributed-"));
        assert!(
            a.contains("slot-1"),
            "the slot name is part of the ref: {a}"
        );
        // Instant-first, zero-padded: lexicographic order stays chronological,
        // which is what the prune's newest-N cap relies on.
        let later = unattributed_wip_ref(slot_a, "2026-09-02T10:11:13.000Z");
        assert!(a < later, "{a} must sort before {later}");
    }

    #[test]
    fn stale_wip_refs_prunes_closed_and_absent_tickets_but_keeps_live_ones() {
        let refs = [
            "refs/coxagent/wip/LIVE-1".to_owned(),
            "refs/coxagent/wip/DONE-1".to_owned(),
            "refs/coxagent/wip/GONE-9".to_owned(),
        ];
        let mut tickets = BTreeMap::new();
        tickets.insert("LIVE-1".to_owned(), false);
        tickets.insert("DONE-1".to_owned(), true);
        let stale = stale_wip_refs(&refs, &tickets, MAX_KEPT_UNATTRIBUTED);
        assert_eq!(
            stale,
            vec!["refs/coxagent/wip/DONE-1", "refs/coxagent/wip/GONE-9"]
        );
    }

    #[test]
    fn stale_wip_refs_bounds_the_unattributed_namespace_to_the_newest() {
        let mk = |t: &str| format!("{WIP_REF_PREFIX}unattributed-{t}");
        let refs: Vec<String> = ["20260101", "20260102", "20260103", "20260104"]
            .iter()
            .map(|t| mk(t))
            .collect();
        let stale = stale_wip_refs(&refs, &BTreeMap::new(), 3);
        // Only the OLDEST unattributed checkpoint goes; the newest three stay.
        assert_eq!(stale, vec![mk("20260101")]);
    }

    #[test]
    fn stale_wip_refs_ignores_names_outside_the_namespace() {
        let refs = [
            "refs/heads/main".to_owned(),
            "refs/coxagent/last-good".to_owned(),
        ];
        assert!(stale_wip_refs(&refs, &BTreeMap::new(), 3).is_empty());
    }
}
