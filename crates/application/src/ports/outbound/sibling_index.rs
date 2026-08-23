//! `SiblingIndexPort` — one immutable snapshot of the sibling projects
//! co-located with this one on the hub.
//!
//! Cross-project knowledge (CXA-F010) needs to know which other projects exist
//! and where their doc/wiki roots live. Today that knowledge exists only as a
//! flat write-only `hub_lessons.md`, so nothing on the hub can point a brief at
//! a sibling's wiki, closed tickets or team memory. This port is the plumbing:
//! it enumerates siblings once, behind an IO boundary, and leaves every decision
//! about what to surface as a PURE function of that snapshot — following the
//! `GitPort::working_tree` pattern (snapshot-then-pure-decision).
//!
//! The adapter does all IO (reading each sibling's registry entry / coxagent.json,
//! resolving paths). This file holds only types and pure functions over them,
//! so there is no application-layer direct `std::fs` access — which is what keeps
//! this module inside the hexagonal ratchet (`crates/app/tests/hexagonal_gate.rs`).

use crate::error::PortError;
use async_trait::async_trait;
use std::path::{PathBuf};

/// Optional cross-project knowledge roots a sibling exposes for reuse.
#[derive(Debug, Clone, Default)]
pub struct KnowledgeHandles {
    /// Absolute path to a repo-map document (`REPO_MAP.md`) when present.
    pub repo_map_path: Option<PathBuf>,
    /// Absolute path to a wiki/docs root when present.
    pub wiki: Option<PathBuf>,
    /// Absolute path to a codegraph index root when present.
    pub codegraph: Option<PathBuf>,
}

/// One co-located sibling project on the hub.
#[derive(Debug, Clone)]
pub struct SiblingProject {
    /// Directory name under the base dir this project shares with its siblings.
    pub name: String,
    /// Working-tree path (the project's root).
    pub path: PathBuf,
    /// The host port it publishes (`deploy.host_port`), when any is configured.
    pub host_port: Option<u16>,
    /// Optional doc/wiki/codegraph handles for cross-project reuse.
    pub knowledge_handles: KnowledgeHandles,
}

/// Enumerate co-located sibling projects on the hub.
#[async_trait]
pub trait SiblingIndexPort: Send + Sync {
    /// ONE immutable snapshot of this project's co-located siblings. Returns an
    /// EMPTY list — not an error — when there are no other projects or no index
    /// can be read, so single-project behavior is byte-identical whether or not
    /// siblings exist (CXA-F015 AC3).
    ///
    /// # Errors
    ///
    /// [`PortError::Backend`] only for an IO failure that means we cannot even
    /// attempt discovery; absence of siblings/index is deliberately NOT an error.
    async fn siblings(&self) -> Result<Vec<SiblingProject>, PortError>;
}

/// Sort by directory-name and drop duplicate names (keeping each first-seen),
/// so any adapter scan yields a deterministic list regardless of filesystem order —
/// CXA-F015 AC4 ("sorted, de-duplicated"). A pure function over an already-built
/// snapshot; callers never reach into `std::fs` themselves for ordering reasons.
#[must_use]
pub fn sorted_unique_siblings(mut siblings: Vec<SiblingProject>) -> Vec<SiblingProject> {
    siblings.sort_by(|a, b| a.name.cmp(&b.name));
    siblings.dedup_by(|a, b| a.name == b.name);
    siblings
}

/// Render a prompt block naming each co-located sibling project so a brief can
/// point at a sibling's wiki/docs/closed tickets. Empty when there are no
/// siblings — so nothing changes for a single-project hub (CXA-F015 AC5). This
/// is shipped as an additive, tested decision-layer helper; wiring it into the
/// task prompt (`run_dev/briefing.rs::build_request`) lands with CXA-F010.
#[must_use]
pub fn siblings_block(siblings: &[SiblingProject]) -> String {
  if siblings.is_empty() { return String::new(); }
  let mut out = String::from(
      "\n\n## Sibling projects on this hub (reusable cross-project knowledge):\n",
  );
  for sib in siblings {
      let port = match sib.host_port {
          Some(p) => format!(" — publishes host port {p}"),
          None => String::new(),
      };
      out.push_str(&format!("- {} at {}{port}\n", sib.name, sib.path.display()));
  }
  out
}

#[cfg(test)]
mod tests {
  use super::{sorted_unique_siblings, KnowledgeHandles, SiblingProject};
  use std::path::{PathBuf};

  fn sib(name: &str) -> SiblingProject {
      SiblingProject {
          name: name.to_owned(),
          path: PathBuf::from("/base").join(name),
          host_port: None,
          knowledge_handles: KnowledgeHandles::default(),
      }
  }

  fn names(list: &[SiblingProject]) -> Vec<String> {
      list.iter().map(|s| s.name.clone()).collect()
  }

  #[test]
  fn zero_siblings_stays_empty() {
      assert!(sorted_unique_siblings(vec![]).is_empty());
  }

  #[test]
  fn one_sibling_is_returned_as_is() {
      let list = sorted_unique_siblings(vec![sib("alpha")]);
      assert_eq!(names(&list), ["alpha"]);
  }

  #[test]
  fn many_siblings_are_sorted_and_deduplicated() {
      // Deliberately unsorted AND containing duplicates (filesystem order can be either).
      let list = sorted_unique_siblings(vec![
          sib("zeta"),
          sib("alpha"),
          sib("mid"),
          sib("alpha"),
          sib("zeta"),
      ]);
      assert_eq!(names(&list), ["alpha", "mid", "zeta"]);
  }

}


