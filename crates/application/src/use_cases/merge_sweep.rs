//! On-demand SA merge sweep: walk the open-PR queue (oldest first) and merge
//! everything green — mergeable and not failing CI — in one deterministic pass
//! (no engine tokens). Triggered by the human from the Review tab or chat
//! (`/merge`), it reports what it merged and why it skipped the rest into the
//! `#agents` channel, so "SA, merge the queue" is one click instead of waiting
//! for the next cycle.

use crate::ports::outbound::{ForgePort, StateStorePort};
use std::fmt::Write as _;

/// What one sweep did: merged PR numbers, and skipped `(number, reason)`.
#[derive(Debug, Default, serde::Serialize)]
pub struct SweepOutcome {
    pub merged: Vec<u64>,
    pub skipped: Vec<(u64, String)>,
}

/// Run one sweep over PRs into `target`. Posts a summary to `#agents` as SA.
pub async fn merge_sweep<S: StateStorePort + ?Sized>(
    forge: &dyn ForgePort,
    store: &S,
    target: &str,
    vi: bool,
) -> SweepOutcome {
    let mut out = SweepOutcome::default();
    let Ok(prs) = forge.list_open_prs().await else {
        return out;
    };
    let mut queue: Vec<_> = prs.into_iter().filter(|p| p.base == target).collect();
    queue.sort_by(|a, b| a.created.cmp(&b.created));
    for pr in queue.into_iter().take(12) {
        if !pr.mergeable {
            out.skipped.push((pr.number, "merge conflict".to_owned()));
            continue;
        }
        if pr.ci == "failing" {
            out.skipped.push((pr.number, "CI failing".to_owned()));
            continue;
        }
        if pr.ci == "pending" {
            out.skipped.push((pr.number, "CI pending".to_owned()));
            continue;
        }
        match forge.merge_pr(pr.number).await {
            Ok(()) => out.merged.push(pr.number),
            Err(e) => out.skipped.push((pr.number, format!("merge refused: {e}"))),
        }
    }

    // Tell the team what happened, in one message.
    let list = |v: &[u64]| {
        v.iter()
            .map(|n| format!("#{n}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut msg = if vi {
        if out.merged.is_empty() {
            "🔀 SA merge sweep: không có PR nào đủ điều kiện merge ngay.".to_owned()
        } else {
            format!("🔀 SA merge sweep: đã merge {}.", list(&out.merged))
        }
    } else if out.merged.is_empty() {
        "🔀 SA merge sweep: nothing was green enough to merge right now.".to_owned()
    } else {
        format!("🔀 SA merge sweep: merged {}.", list(&out.merged))
    };
    if !out.skipped.is_empty() {
        let _ = write!(msg, "{}", if vi { " Còn lại: " } else { " Remaining: " });
        let parts: Vec<String> = out
            .skipped
            .iter()
            .map(|(n, r)| format!("#{n} ({r})"))
            .collect();
        let _ = write!(msg, "{}", parts.join(", "));
        msg.push_str(if vi {
            " — conflict sẽ được DEV gỡ ở cycle tới."
        } else {
            " — conflicts get fixed by DEV next cycle."
        });
    }
    let _ = crate::ports::outbound::mutate_state(store, |s| {
        s.post_chat_in("SA", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
        Ok(())
    })
    .await;
    out
}
