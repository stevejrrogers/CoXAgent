// Part of the server module split by concern — see server/mod.rs.
//! The JSON request/query shapes the HTTP handlers accept — nothing but
//! serde structs, so handler modules stop sharing mod.rs to gain one field.

use super::hub_docs::DownloadsCfg;

#[derive(serde::Deserialize)]
pub(super) struct ConnectReq {
    pub(super) token: String,
}

#[derive(serde::Deserialize)]
pub(super) struct CreateProjectReq {
    pub(super) name: String,
    #[serde(default)]
    pub(super) alias: Option<String>,
    /// Adopt an existing codebase at this path (brownfield import).
    #[serde(default)]
    pub(super) existing: Option<String>,
    /// Clone this git URL and adopt it (brownfield import from remote).
    #[serde(default)]
    pub(super) git_url: Option<String>,
    /// Confirmed project goal/context to seed (from AI-assisted drafting).
    #[serde(default)]
    pub(super) goal: Option<String>,
    /// Space to file the new project under (super admin or that space's admin).
    #[serde(default)]
    pub(super) space: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct RenameProjectReq {
    pub(super) name: String,
}

#[derive(serde::Deserialize)]
pub(super) struct GoalReq {
    pub(super) goal: String,
}

#[derive(serde::Deserialize)]
pub(super) struct PriorityReq {
    pub(super) priority: coxagent_domain::Priority,
}

#[derive(serde::Deserialize)]
pub(super) struct CreateTicketReq {
    #[serde(default)]
    pub(super) ticket_type: Option<String>,
    pub(super) title: String,
    #[serde(default)]
    pub(super) description: String,
    #[serde(default)]
    pub(super) priority: Option<coxagent_domain::Priority>,
    #[serde(default)]
    pub(super) complexity: Option<coxagent_domain::Complexity>,
    #[serde(default)]
    pub(super) has_ui: bool,
    #[serde(default)]
    pub(super) acceptance_criteria: Vec<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct DiscussReq {
    pub(super) topic: String,
}

#[derive(serde::Deserialize)]
pub(super) struct AnalyzeReq {
    pub(super) description: String,
}

#[derive(serde::Deserialize)]
pub(super) struct EditReq {
    pub(super) title: String,
    #[serde(default)]
    pub(super) description: String,
}

#[derive(serde::Deserialize)]
pub(super) struct CommentQuery {
    pub(super) ticket: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct PostCommentReq {
    pub(super) body: String,
    #[serde(default)]
    pub(super) ticket: Option<String>,
    #[serde(default)]
    pub(super) attachments: Vec<coxagent_application::Attachment>,
}

#[derive(serde::Deserialize)]
pub(super) struct CommentReactReq {
    pub(super) emoji: String,
}

#[derive(serde::Deserialize)]
pub(super) struct ChatListQuery {
    /// Which channel's history to return; defaults to `#general`.
    pub(super) channel: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct CreateChannelReq {
    pub(super) name: String,
    #[serde(default)]
    pub(super) kind: Option<String>,
    /// Open this channel INSIDE another one (the `+` on a channel row).
    #[serde(default)]
    pub(super) parent: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct ChannelSettingsReq {
    /// `"private"` or `"public"`. `#general` may not change.
    #[serde(default)]
    pub(super) kind: Option<String>,
    /// Whether any member may invite; false leaves it to the owner and the
    /// people they delegated to.
    #[serde(default)]
    pub(super) open_invite: Option<bool>,
    #[serde(default)]
    pub(super) topic: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct ChannelMemberReq {
    /// Username to invite or delegate to.
    pub(super) user: String,
    /// When true, grant invite permission (owner only), not just membership.
    #[serde(default)]
    pub(super) delegate: bool,
}

#[derive(serde::Deserialize)]
pub(super) struct PostChatReq {
    pub(super) body: String,
    #[serde(default)]
    pub(super) channel: Option<String>,
    #[serde(default)]
    pub(super) attachments: Vec<coxagent_application::Attachment>,
}

#[derive(serde::Deserialize)]
pub(super) struct DocUpsertReq {
    #[serde(default)]
    pub(super) folder: String,
    pub(super) title: String,
    #[serde(default)]
    pub(super) body: String,
}

#[derive(serde::Deserialize)]
pub(super) struct DocEditReq {
    pub(super) instruction: String,
}

#[derive(serde::Deserialize)]
pub(super) struct FolderReq {
    pub(super) path: String,
}

#[derive(serde::Deserialize)]
pub(super) struct RefsQuery {
    pub(super) name: String,
}

#[derive(serde::Deserialize)]
pub(super) struct GoalUpdateReq {
    pub(super) goal: String,
}

#[derive(serde::Deserialize)]
pub(super) struct PrActionReq {
    #[serde(default)]
    pub(super) comment: String,
}

#[derive(serde::Deserialize)]
pub(super) struct DmReq {
    pub(super) user: String,
}

#[derive(serde::Deserialize)]
pub(super) struct ReactReq {
    pub(super) id: String,
    pub(super) emoji: String,
}

#[derive(serde::Deserialize)]
pub(super) struct WebhookReq {
    pub(super) channel: String,
    #[serde(default)]
    pub(super) label: String,
}

#[derive(serde::Deserialize)]
pub(super) struct TopicReq {
    pub(super) topic: String,
}

#[derive(serde::Deserialize)]
pub(super) struct HookPostReq {
    #[serde(default)]
    pub(super) text: String,
    #[serde(default)]
    pub(super) username: String,
}

#[derive(serde::Deserialize)]
pub(super) struct ChatReplyReq {
    pub(super) message: String,
}

#[derive(serde::Deserialize)]
pub(super) struct PathQuery {
    #[serde(default)]
    pub(super) path: String,
}

#[derive(serde::Deserialize)]
pub(super) struct WorkspacePutReq {
    #[serde(default)]
    pub(super) name: String,
    #[serde(default)]
    pub(super) tagline: String,
    #[serde(default)]
    pub(super) accent: String,
    #[serde(default)]
    pub(super) conventions: Option<String>,
    /// Download/release config — only overwritten when provided.
    #[serde(default)]
    pub(super) downloads: Option<DownloadsCfg>,
}

#[derive(serde::Deserialize)]
pub(super) struct InviteCreateReq {
    #[serde(default)]
    pub(super) role: Option<String>,
    #[serde(default)]
    pub(super) projects: Vec<String>,
    #[serde(default)]
    pub(super) uses: Option<u32>,
}

#[derive(serde::Deserialize)]
pub(super) struct JoinReq {
    pub(super) token: String,
    pub(super) username: String,
    pub(super) password: String,
    #[serde(default)]
    pub(super) name: String,
}

#[derive(serde::Deserialize)]
pub(super) struct SpaceReq {
    pub(super) name: String,
    #[serde(default)]
    pub(super) tagline: String,
    #[serde(default)]
    pub(super) admins: Vec<String>,
    #[serde(default)]
    pub(super) projects: Vec<String>,
    #[serde(default)]
    pub(super) members: Vec<String>,
    /// Monthly USD cap (0 = none). Applied by Super only.
    #[serde(default)]
    pub(super) budget_usd: f64,
}
