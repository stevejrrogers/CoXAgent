// Part of the server module split by concern — see server/mod.rs.
//! CXA-B287a / CXA-B323 — the HTTP WIRE-SURFACE REGISTRY: an explicit
//! inventory of every serde-derived request/response DTO that crosses the
//! hub's JSON wire, with the adapter file that owns it.
//!
//! This is the B287a scaffolding ticket (re-landed by CXA-B323 after the
//! original branch never reached main): it declares WHAT exists on the wire
//! and WHERE it lives — no fixtures, no assertions over individual payloads.
//! The contract guard work (CXA-B254 part 3/3 follow-up) consumes this
//! registry instead of hand-copying DTO lists; the declared row count is
//! asserted against the actual source tree at test time by
//! `crates/presentation/tests/wire_registry_guard_b287a.rs`.
//!
//! Why names rather than real types: the presentation layer is a thin inbound
//! adapter; nearly all shapes here are `pub(super)` and must not leak into the
//! crate's public API. A name-keyed table keeps them encapsulated while still
//! giving the guards an exhaustive, duplicate-checked list to iterate.

/// Which direction(s) a shape crosses the wire.
///
/// Inbound = deserialized from a client payload (`serde::Deserialize`),
/// Outbound = serialized into a response (`serde::Serialize`). Most request
/// shapes also drive validation-error replies, so they carry both directions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Deserialized from a client request (body or query string).
    Inbound,
    /// Serialized into a server response body.
    Outbound,
}

/// One wire surface: a named DTO shape and the adapter file that owns it.
#[derive(Clone, Copy, Debug)]
pub struct WireSurface {
    /// The Rust type name exactly as declared in the owning file.
    pub dto: &'static str,
    /// Owning adapter file under `crates/presentation/src/server/`.
    pub owner_file: &'static str,
    /// Every direction the shape currently crosses (never empty).
    pub directions: &'static [Direction],
}

/// Build one [`WireSurface`] row; ties name, owner and directions together so
/// a row cannot half-drift.
macro_rules! wire_dto {
    ($name:ident => $file:expr, $dirs:expr) => {
        WireSurface {
            dto: stringify!($name),
            owner_file: $file,
            directions: $dirs,
        }
    };
}

/// The registry itself: one row per serde-derived DTO on the hub's HTTP wire,
/// sorted by type name. Add a row when you add a wire shape; a missing or
/// stale row fails `wire_registry_guard_b287a.rs`'s count assertion.
pub const WIRE_REGISTRY: &[WireSurface] = &[
    wire_dto!(AddKeyBody => "user_keys.rs", &[Direction::Inbound]),
    wire_dto!(AddServiceReq => "services.rs", &[Direction::Inbound]),
    wire_dto!(AgentLogQuery => "transcripts.rs", &[Direction::Inbound]),
    wire_dto!(AgentLogStreamQuery => "transcripts.rs", &[Direction::Inbound]),
    wire_dto!(AlertView => "alerts.rs", &[Direction::Outbound]),
    wire_dto!(AnalyzeReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(ArchiveQuery => "requests.rs", &[Direction::Inbound]),
    wire_dto!(Args => "store_rpc.rs", &[Direction::Inbound]),
    wire_dto!(AssignReq => "inbox.rs", &[Direction::Inbound]),
    wire_dto!(AssignServicesReq => "services.rs", &[Direction::Inbound]),
    wire_dto!(AuditQ => "store_rpc.rs", &[Direction::Inbound]),
    wire_dto!(Beat => "store_rpc.rs", &[Direction::Inbound]),
    wire_dto!(BrakeHoldReq => "tunecockpit.rs", &[Direction::Inbound]),
    wire_dto!(BugCounts => "share_page.rs", &[Direction::Outbound]),
    wire_dto!(BurnModeReq => "mod.rs", &[Direction::Inbound]),
    wire_dto!(ChannelMemberReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(ChannelSettingsReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(ChatListQuery => "requests.rs", &[Direction::Inbound]),
    wire_dto!(ChatReplyReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(CodeGraphQuery => "mod.rs", &[Direction::Inbound]),
    wire_dto!(CodeReq => "mod.rs", &[Direction::Inbound]),
    wire_dto!(CommentQuery => "requests.rs", &[Direction::Inbound]),
    wire_dto!(CommentReactReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(ConnectReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(CreateChannelReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(CreateMyTokenReq => "people.rs", &[Direction::Inbound]),
    wire_dto!(CreateProjectReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(CreateTicketReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(CreateTokenReq => "people.rs", &[Direction::Inbound]),
    wire_dto!(CreateUserReq => "people.rs", &[Direction::Inbound]),
    wire_dto!(DiscussReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(DmReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(DocEditReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(DocUpsertReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(DownloadsCfg => "hub_docs.rs", &[Direction::Inbound, Direction::Outbound]),
    wire_dto!(DupeActionReq => "duplicate_radar.rs", &[Direction::Inbound]),
    wire_dto!(DupeAllow => "hub_docs.rs", &[Direction::Inbound, Direction::Outbound]),
    wire_dto!(EditReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(FleetCeilingReq => "fleet_spend.rs", &[Direction::Inbound]),
    wire_dto!(FolderReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(GoalReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(GoalTitleReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(GoalUpdateReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(HandbackReq => "claims.rs", &[Direction::Inbound]),
    wire_dto!(HoldReq => "mod.rs", &[Direction::Inbound]),
    wire_dto!(HookPostReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(HumanActionReq => "inbox.rs", &[Direction::Inbound]),
    wire_dto!(Invite => "hub_docs.rs", &[Direction::Inbound, Direction::Outbound]),
    wire_dto!(InviteCreateReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(JoinReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(LessonActionReq => "lessons.rs", &[Direction::Inbound]),
    wire_dto!(LoginReq => "mod.rs", &[Direction::Inbound]),
    wire_dto!(Meeting => "hub_docs.rs", &[Direction::Inbound, Direction::Outbound]),
    wire_dto!(MeetingPatch => "hub_docs.rs", &[Direction::Inbound]),
    wire_dto!(MeetingReq => "hub_docs.rs", &[Direction::Inbound]),
    wire_dto!(MeetingRingReq => "hub_docs.rs", &[Direction::Inbound]),
    wire_dto!(MeetingsDoc => "hub_docs.rs", &[Direction::Inbound, Direction::Outbound]),
    wire_dto!(MemberReq => "people.rs", &[Direction::Inbound]),
    wire_dto!(OpQ => "store_rpc.rs", &[Direction::Inbound]),
    wire_dto!(PathQuery => "requests.rs", &[Direction::Inbound]),
    wire_dto!(PostChatReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(PostCommentReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(PrActionReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(PriorityReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(Profile => "people.rs", &[Direction::Inbound, Direction::Outbound]),
    wire_dto!(ProfileReq => "people.rs", &[Direction::Inbound]),
    wire_dto!(ProfilesDoc => "people.rs", &[Direction::Inbound, Direction::Outbound]),
    wire_dto!(QueueScopeReq => "mod.rs", &[Direction::Inbound]),
    wire_dto!(QueueSprintReq => "mod.rs", &[Direction::Inbound]),
    wire_dto!(ReactReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(RefsQuery => "requests.rs", &[Direction::Inbound]),
    wire_dto!(RejectReq => "work.rs", &[Direction::Inbound]),
    wire_dto!(RenameProjectReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(ResetPasswordReq => "people.rs", &[Direction::Inbound]),
    wire_dto!(RetagApplyReq => "services.rs", &[Direction::Inbound]),
    wire_dto!(RetagAssignment => "services.rs", &[Direction::Inbound]),
    wire_dto!(RiverQuery => "fleet.rs", &[Direction::Inbound]),
    wire_dto!(ScanDepsReq => "deps.rs", &[Direction::Inbound]),
    wire_dto!(ScanServiceReq => "services.rs", &[Direction::Inbound]),
    wire_dto!(SearchParams => "search.rs", &[Direction::Inbound]),
    wire_dto!(SelfProfileReq => "people.rs", &[Direction::Inbound]),
    wire_dto!(ServiceStatusReq => "services.rs", &[Direction::Inbound]),
    wire_dto!(SetTicketGoalReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(ShapeReq => "approval_policy.rs", &[Direction::Inbound]),
    wire_dto!(ShareLink => "hub_docs.rs", &[Direction::Inbound, Direction::Outbound]),
    wire_dto!(ShareLinkCreateReq => "share_link.rs", &[Direction::Inbound]),
    wire_dto!(ShareSnapshot => "share_page.rs", &[Direction::Outbound]),
    wire_dto!(Space => "hub_docs.rs", &[Direction::Inbound, Direction::Outbound]),
    wire_dto!(SpaceReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(SpacesDoc => "hub_docs.rs", &[Direction::Inbound, Direction::Outbound]),
    wire_dto!(SprintGoalReq => "mod.rs", &[Direction::Inbound]),
    wire_dto!(SprintScopeReq => "mod.rs", &[Direction::Inbound]),
    wire_dto!(TopicReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(UpdateUserReq => "people.rs", &[Direction::Inbound]),
    wire_dto!(UserKey => "user_keys.rs", &[Direction::Inbound, Direction::Outbound]),
    wire_dto!(UserKeyView => "user_keys.rs", &[Direction::Outbound]),
    wire_dto!(UserKeysDoc => "user_keys.rs", &[Direction::Inbound, Direction::Outbound]),
    wire_dto!(VersionRow => "prompts_ledger.rs", &[Direction::Outbound]),
    wire_dto!(WebhookReq => "requests.rs", &[Direction::Inbound]),
    wire_dto!(WikiSearchParams => "docs.rs", &[Direction::Inbound]),
    wire_dto!(WorkspaceDoc => "hub_docs.rs", &[Direction::Inbound, Direction::Outbound]),
    wire_dto!(WorkspacePutReq => "requests.rs", &[Direction::Inbound]),
];

/// All DTO names in [`WIRE_REGISTRY`], in declaration (sorted) order.
#[must_use]
pub fn all_dto_names() -> Vec<&'static str> {
    WIRE_REGISTRY.iter().map(|w| w.dto).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_rows_are_sorted_by_dto_name() {
        let names = all_dto_names();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, names, "WIRE_REGISTRY must stay sorted by DTO name");
    }

    #[test]
    fn every_row_declares_at_least_one_direction() {
        assert!(
            WIRE_REGISTRY.iter().all(|w| !w.directions.is_empty()),
            "a wire shape must cross the wire in some direction"
        );
    }

    #[test]
    fn registry_names_and_owners_are_unique_per_row() {
        let mut seen = std::collections::BTreeSet::new();
        for w in WIRE_REGISTRY {
            assert!(
                seen.insert((w.dto, w.owner_file)),
                "duplicate registry row for {} in {}",
                w.dto,
                w.owner_file
            );
        }
    }
}
