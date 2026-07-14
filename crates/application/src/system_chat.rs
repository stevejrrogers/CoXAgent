//! System-wide chat: channels shared across the whole hub, not per project.
//!
//! Three kinds of channel:
//! - `#general` — every user in the system is a member (implicit).
//! - project channels — one per project, id `#<alias>`, membership mirrors the
//!   people assigned to that project. Auto-provisioned; never hand-created.
//! - private channels — created by a lead/admin, explicit membership with
//!   owner-delegated invites.
//!
//! Membership for `#general` and project channels is *computed* from the auth
//! users + project registry (passed in as [`ChatContext`]); only private
//! channels and the messages are persisted here.

use crate::state::{now_rfc3339, slugify, Attachment, Channel, ChatMsg, GENERAL_CHANNEL, MAX_CHAT};
use serde::{Deserialize, Serialize};

/// The persisted system-chat aggregate: private channels + all messages.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SystemChat {
    /// Owner-created private channels (general + project channels are computed).
    #[serde(default)]
    pub channels: Vec<Channel>,
    /// Every message across every channel, tagged by `channel` id.
    #[serde(default)]
    pub chat: Vec<ChatMsg>,
}

/// A project the hub knows about, for provisioning its channel.
#[derive(Debug, Clone)]
pub struct ProjectRef {
    pub id: String,
    pub alias: String,
    pub name: String,
}

/// One system user, for computing membership.
#[derive(Debug, Clone)]
pub struct UserRef {
    pub username: String,
    /// Project ids this user is assigned to.
    pub projects: Vec<String>,
    /// Whether the user is an admin (sees every channel).
    pub admin: bool,
}

/// The live context needed to resolve computed channels/membership: who exists
/// and what projects there are. Rebuilt per request from auth + registry.
pub struct ChatContext {
    pub users: Vec<UserRef>,
    pub projects: Vec<ProjectRef>,
}

impl ChatContext {
    fn user(&self, username: &str) -> Option<&UserRef> {
        self.users.iter().find(|u| u.username == username)
    }

    /// The id of a project's channel: `alias` slugified (falls back to the id).
    fn project_channel_id(p: &ProjectRef) -> String {
        let base = if p.alias.trim().is_empty() {
            &p.id
        } else {
            &p.alias
        };
        slugify(base)
    }
}

impl SystemChat {
    /// Build the `#general` channel record (everyone).
    fn general() -> Channel {
        Channel {
            id: GENERAL_CHANNEL.to_owned(),
            name: "general".to_owned(),
            owner: String::new(),
            members: Vec::new(),
            inviters: Vec::new(),
            created_at: String::new(),
            kind: "general".to_owned(),
            project: String::new(),
        }
    }

    /// Build a project channel record with membership from the context.
    fn project_channel(p: &ProjectRef, ctx: &ChatContext) -> Channel {
        let members = ctx
            .users
            .iter()
            .filter(|u| u.projects.iter().any(|pid| pid == &p.id))
            .map(|u| u.username.clone())
            .collect();
        Channel {
            id: ChatContext::project_channel_id(p),
            name: p.alias.clone().to_lowercase(),
            owner: String::new(),
            members,
            inviters: Vec::new(),
            created_at: String::new(),
            kind: "project".to_owned(),
            project: p.id.clone(),
        }
    }

    /// Resolve a channel by id against the live context (general, project, or a
    /// stored private channel).
    #[must_use]
    pub fn resolve(&self, id: &str, ctx: &ChatContext) -> Option<Channel> {
        if id == GENERAL_CHANNEL {
            return Some(Self::general());
        }
        if let Some(p) = ctx
            .projects
            .iter()
            .find(|p| ChatContext::project_channel_id(p) == id)
        {
            return Some(Self::project_channel(p, ctx));
        }
        self.channels.iter().find(|c| c.id == id).cloned()
    }

    /// Whether `user` may view `channel_id` (general: everyone; project: assigned
    /// or admin; private: member/owner or admin).
    #[must_use]
    pub fn can_view(&self, channel_id: &str, user: &str, ctx: &ChatContext) -> bool {
        let is_admin = ctx.user(user).is_some_and(|u| u.admin);
        match self.resolve(channel_id, ctx) {
            Some(c) if c.kind == "general" => true,
            Some(c) => is_admin || c.can_view(user),
            None => false,
        }
    }

    /// All channels `user` can see: `#general`, then the project channels they're
    /// in (admins see all), then their private channels. Ordered for display.
    #[must_use]
    pub fn channels_for(&self, user: &str, ctx: &ChatContext) -> Vec<Channel> {
        let is_admin = ctx.user(user).is_some_and(|u| u.admin);
        let mut out = vec![Self::general()];
        for p in &ctx.projects {
            let ch = Self::project_channel(p, ctx);
            if is_admin || ch.can_view(user) {
                out.push(ch);
            }
        }
        for c in &self.channels {
            if is_admin || c.can_view(user) {
                out.push(c.clone());
            }
        }
        out
    }

    /// Messages in one channel, oldest first.
    #[must_use]
    pub fn messages_in(&self, channel_id: &str) -> Vec<ChatMsg> {
        self.chat
            .iter()
            .filter(|m| m.channel == channel_id)
            .cloned()
            .collect()
    }

    /// Append a message to `channel_id`, trimming the oldest beyond [`MAX_CHAT`].
    pub fn post(&mut self, user: &str, body: &str, channel_id: &str, attachments: Vec<Attachment>) {
        self.chat.push(ChatMsg {
            at: now_rfc3339(),
            user: user.to_owned(),
            body: body.to_owned(),
            channel: channel_id.to_owned(),
            attachments,
        });
        let overflow = self.chat.len().saturating_sub(MAX_CHAT);
        if overflow > 0 {
            self.chat.drain(0..overflow);
        }
    }

    /// Create a private channel owned by `owner`. Collides against general,
    /// project channels, and existing private channels.
    ///
    /// # Errors
    /// A human-readable message when the name is invalid or already taken.
    pub fn create_channel(
        &mut self,
        name: &str,
        owner: &str,
        ctx: &ChatContext,
    ) -> Result<Channel, String> {
        let id = slugify(name);
        if id.is_empty() {
            return Err("channel name must contain letters or numbers".to_owned());
        }
        let taken = id == GENERAL_CHANNEL
            || ctx
                .projects
                .iter()
                .any(|p| ChatContext::project_channel_id(p) == id)
            || self.channels.iter().any(|c| c.id == id);
        if taken {
            return Err(format!("channel #{id} already exists"));
        }
        let ch = Channel {
            id,
            name: name.trim().to_owned(),
            owner: owner.to_owned(),
            members: vec![owner.to_owned()],
            inviters: Vec::new(),
            created_at: now_rfc3339(),
            kind: "private".to_owned(),
            project: String::new(),
        };
        self.channels.push(ch.clone());
        Ok(ch)
    }

    /// Invite `invitee` to a private channel (`actor` must be owner or inviter).
    /// Project/general channels manage membership via project assignment, not here.
    ///
    /// # Errors
    /// When the channel isn't a private channel or `actor` lacks permission.
    pub fn invite(&mut self, channel_id: &str, actor: &str, invitee: &str) -> Result<(), String> {
        let ch = self
            .channels
            .iter_mut()
            .find(|c| c.id == channel_id)
            .ok_or("channel not found (only private channels take invites)")?;
        if !ch.can_invite(actor) {
            return Err("you don't have permission to invite to this channel".to_owned());
        }
        let invitee = invitee.trim();
        if invitee.is_empty() {
            return Err("no user to invite".to_owned());
        }
        if ch.owner != invitee && !ch.members.iter().any(|m| m == invitee) {
            ch.members.push(invitee.to_owned());
        }
        Ok(())
    }

    /// Open (or fetch) a direct-message channel between `me` and `other`. DMs are
    /// private channels with a deterministic id so both users land in the same one.
    ///
    /// # Errors
    /// When `me == other`.
    pub fn open_dm(&mut self, me: &str, other: &str) -> Result<Channel, String> {
        if me == other {
            return Err("cannot DM yourself".to_owned());
        }
        let mut pair = [me, other];
        pair.sort_unstable();
        let id = format!("dm-{}-{}", slugify(pair[0]), slugify(pair[1]));
        if let Some(c) = self.channels.iter().find(|c| c.id == id) {
            return Ok(c.clone());
        }
        let ch = Channel {
            id,
            name: format!("{me} · {other}"),
            owner: String::new(),
            members: vec![me.to_owned(), other.to_owned()],
            inviters: Vec::new(),
            created_at: now_rfc3339(),
            kind: "dm".to_owned(),
            project: String::new(),
        };
        self.channels.push(ch.clone());
        Ok(ch)
    }

    /// Grant `grantee` invite permission on a private channel (owner only).
    ///
    /// # Errors
    /// When the channel isn't found or `actor` isn't the owner.
    pub fn delegate(&mut self, channel_id: &str, actor: &str, grantee: &str) -> Result<(), String> {
        let ch = self
            .channels
            .iter_mut()
            .find(|c| c.id == channel_id)
            .ok_or("channel not found")?;
        if ch.owner != actor {
            return Err("only the channel owner can delegate invite permission".to_owned());
        }
        let grantee = grantee.trim();
        if grantee.is_empty() {
            return Err("no user to delegate to".to_owned());
        }
        if !ch.members.iter().any(|m| m == grantee) {
            ch.members.push(grantee.to_owned());
        }
        if !ch.inviters.iter().any(|m| m == grantee) {
            ch.inviters.push(grantee.to_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{ChatContext, ProjectRef, SystemChat, UserRef};

    fn ctx() -> ChatContext {
        ChatContext {
            users: vec![
                UserRef {
                    username: "root".into(),
                    projects: vec![],
                    admin: true,
                },
                UserRef {
                    username: "alice".into(),
                    projects: vec!["p1".into()],
                    admin: false,
                },
                UserRef {
                    username: "bob".into(),
                    projects: vec![],
                    admin: false,
                },
            ],
            projects: vec![ProjectRef {
                id: "p1".into(),
                alias: "CXC".into(),
                name: "CoXChat".into(),
            }],
        }
    }

    #[test]
    fn general_visible_to_all_project_channel_scoped() {
        let sc = SystemChat::default();
        let c = ctx();
        assert!(sc.can_view("general", "bob", &c));
        // project channel #cxc: alice is in p1, bob isn't, root is admin.
        assert!(sc.can_view("cxc", "alice", &c));
        assert!(!sc.can_view("cxc", "bob", &c));
        assert!(sc.can_view("cxc", "root", &c));
    }

    #[test]
    fn channels_for_lists_general_and_projects() {
        let sc = SystemChat::default();
        let c = ctx();
        let alice = sc.channels_for("alice", &c);
        let ids: Vec<_> = alice.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["general", "cxc"]);
        // bob only sees general.
        assert_eq!(sc.channels_for("bob", &c).len(), 1);
        // root (admin) sees general + cxc.
        assert_eq!(sc.channels_for("root", &c).len(), 2);
    }

    #[test]
    fn private_channel_create_and_invite() {
        let mut sc = SystemChat::default();
        let c = ctx();
        let ch = sc.create_channel("War Room", "alice", &c).expect("create");
        assert_eq!(ch.id, "war-room");
        assert!(!sc.can_view("war-room", "bob", &c));
        sc.invite("war-room", "alice", "bob").expect("invite");
        assert!(sc.can_view("war-room", "bob", &c));
        // can't collide with a project channel name
        assert!(sc.create_channel("cxc", "alice", &c).is_err());
    }
}
