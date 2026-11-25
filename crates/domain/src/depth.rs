//! The UI depth contract (CXA-B188a) — a PURE checker over declared elements.
//!
//! The house depth bar says no shipped surface may be shallow: every list,
//! number, state and action carries its named depth elements (a filter on a
//! list, a window on a number, attribution on a state, an inverse on an
//! action). This module is the single definition of what each element kind
//! REQUIRES and how violations are reported. It knows nothing about files,
//! sources or the repo — the depth gate test (CXA-B188b) scans the real UI
//! registry and feeds the parsed [`DepthElement`]s into [`evaluate`] here, so
//! the decision stays a pure function over data.

/// The four element kinds the depth bar governs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthElementKind {
    /// A rendered list of things (queue, board, table).
    List,
    /// A displayed number (KPI tile, counter).
    Number,
    /// A displayed state that can be off/paused/failing.
    State,
    /// A user-invokable action.
    Action,
}

impl DepthElementKind {
    /// Parses the kind name used in the depth registry (`"list"`, `"number"`,
    /// `"state"`, `"action"`). Unknown names are a registry authoring error
    /// the gate must refuse, never silently pass.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "list" => Some(Self::List),
            "number" => Some(Self::Number),
            "state" => Some(Self::State),
            "action" => Some(Self::Action),
            _ => None,
        }
    }

    /// The capability name used in registry messages.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Number => "number",
            Self::State => "state",
            Self::Action => "action",
        }
    }

    /// The depth elements this kind MUST carry. The depth bar: a list ships
    /// with a filter, a sort, counts, pagination past ~50 rows and an empty
    /// state; a number ships with its window, a trend/delta and a zero-state
    /// hint; a state ships with attribution and a reason when off; an action
    /// ships with its inverse and a confirm naming the blast radius, and
    /// its permission stays DIMMED rather than hidden where unauthorized.
    #[must_use]
    pub fn required(self) -> &'static [&'static str] {
        match self {
            Self::List => &["filter", "sort", "counts", "pagination", "empty_state"],
            Self::Number => &["window", "delta", "zero_state_hint"],
            Self::State => &["attribution", "off_reason"],
            Self::Action => &["inverse", "confirm", "permission_dimmed"],
        }
    }

    /// Whether `capability` is a legal `has:` entry for this kind. A registry
    /// may only claim capabilities its kind actually requires — a typo'd
    /// capability would otherwise make a shallow element look compliant.
    #[must_use]
    pub fn accepts(self, capability: &str) -> bool {
        self.required().contains(&capability)
    }
}

/// One registered UI element, as declared by the depth registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepthElement {
    /// Stable element id, e.g. `"overview.kpi"`. Names the element in every
    /// violation message.
    pub id: String,
    /// Which depth-bar kind of element this is.
    pub kind: DepthElementKind,
    /// The depth elements the registry declares this element already has.
    pub has: Vec<String>,
}

/// One violation: an element and the depth elements it is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepthViolation {
    /// The element id that violates the depth bar.
    pub element: String,
    /// Its kind.
    pub kind: DepthElementKind,
    /// The missing depth element names.
    pub missing: Vec<String>,
}

impl DepthViolation {
    /// One actionable line: names the element, its kind and exactly what is
    /// missing — the message the gate prints on failure.
    #[must_use]
    pub fn message(&self) -> String {
        format!(
            "element `{}` (kind {}) is missing depth: {}",
            self.element,
            self.kind.name(),
            self.missing.join(", ")
        )
    }
}

/// The pure checker: every element missing at least one required depth
/// element produces a [`DepthViolation`]. Compliant elements produce nothing.
/// Ordering is input order, so gate output is deterministic.
#[must_use]
pub fn evaluate(elements: &[DepthElement]) -> Vec<DepthViolation> {
    elements
        .iter()
        .filter_map(|e| {
            let missing: Vec<String> = e
                .kind
                .required()
                .iter()
                .filter(|req| !e.has.iter().any(|c| c == *req))
                .map(|req| (*req).to_owned())
                .collect();
            (!missing.is_empty()).then(|| DepthViolation {
                element: e.id.clone(),
                kind: e.kind,
                missing,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(id: &str, kind: DepthElementKind, has: &[&str]) -> DepthElement {
        DepthElement {
            id: id.to_owned(),
            kind,
            has: has.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    /// Every kind, once — the test matrix over the depth bar.
    const ALL_KINDS: [DepthElementKind; 4] = [
        DepthElementKind::List,
        DepthElementKind::Number,
        DepthElementKind::State,
        DepthElementKind::Action,
    ];

    #[test]
    fn a_compliant_element_produces_no_finding() {
        let all: Vec<DepthElement> = ALL_KINDS
            .iter()
            .map(|k| el("ok.element", *k, k.required()))
            .collect();
        assert!(evaluate(&all).is_empty());
    }

    #[test]
    fn each_kind_reports_its_own_missing_elements() {
        let findings = evaluate(&[
            el("a.list", DepthElementKind::List, &["filter", "sort"]),
            el("b.number", DepthElementKind::Number, &["window"]),
            el("c.state", DepthElementKind::State, &[]),
            el(
                "d.action",
                DepthElementKind::Action,
                &["inverse", "confirm"],
            ),
        ]);
        let named: Vec<(String, Vec<String>)> = findings
            .iter()
            .map(|f| (f.element.clone(), f.missing.clone()))
            .collect();
        assert_eq!(named.len(), 4, "every seeded element violates: {named:?}");
        assert_eq!(findings[0].missing, ["counts", "pagination", "empty_state"]);
        assert_eq!(findings[1].missing, ["delta", "zero_state_hint"]);
        assert_eq!(findings[2].missing, ["attribution", "off_reason"]);
        assert_eq!(findings[3].missing, ["permission_dimmed"]);
    }

    #[test]
    fn the_finding_message_names_the_element_and_the_missing_depth() {
        let findings = evaluate(&[el(
            "inbox.queue",
            DepthElementKind::List,
            &["filter", "sort", "counts", "empty_state"],
        )]);
        let msg = findings[0].message();
        assert!(msg.contains("inbox.queue"), "names the element: {msg}");
        assert!(msg.contains("pagination"), "names what is missing: {msg}");
        assert!(msg.contains("list"), "names the kind: {msg}");
    }

    #[test]
    fn an_unknown_capability_is_not_accepted_for_any_kind() {
        for kind in ALL_KINDS {
            assert!(
                !kind.accepts("pagination_x"),
                "a typo must not count as compliance for {kind:?}"
            );
            for req in kind.required() {
                assert!(kind.accepts(req), "every requirement is claimable");
            }
        }
    }

    #[test]
    fn kind_parse_refuses_unknown_names() {
        assert_eq!(
            DepthElementKind::parse("list"),
            Some(DepthElementKind::List)
        );
        assert_eq!(DepthElementKind::parse("kpi"), None);
    }

    #[test]
    fn each_required_element_is_reported_when_missing_alone() {
        // For every kind and every one of its required depth elements: an
        // element missing exactly that one must be flagged for exactly it —
        // no requirement may be unreportable or silently skippable.
        for kind in ALL_KINDS {
            for req in kind.required() {
                let has: Vec<&str> = kind
                    .required()
                    .iter()
                    .copied()
                    .filter(|r| *r != *req)
                    .collect();
                let findings = evaluate(&[el("solo.element", kind, &has)]);
                assert_eq!(
                    findings,
                    vec![DepthViolation {
                        element: "solo.element".to_owned(),
                        kind,
                        missing: vec![(*req).to_owned()],
                    }],
                    "{kind:?} missing `{req}` must be reported alone"
                );
            }
        }
    }

    #[test]
    fn the_required_sets_match_the_depth_bar_exactly() {
        assert_eq!(
            DepthElementKind::Number.required(),
            &["window", "delta", "zero_state_hint"]
        );
        assert_eq!(
            DepthElementKind::State.required(),
            &["attribution", "off_reason"]
        );
        assert_eq!(
            DepthElementKind::Action.required(),
            &["inverse", "confirm", "permission_dimmed"]
        );
    }
}
