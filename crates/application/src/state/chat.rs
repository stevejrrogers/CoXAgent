// Part of the `state` module split by bounded context — see state/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Chat types: messages, channels, comments, and agent questions.

use serde::{Deserialize, Serialize};

use super::*;

/// A file or image attached to a chat message or discussion comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// Original filename shown to the user.
    pub name: String,
    /// Path to fetch it, e.g. `/api/projects/<pid>/media/<stored>`.
    pub url: String,
    /// MIME type (e.g. `image/png`), used to render images inline.
    pub mime: String,
    /// Size in bytes.
    pub size: u64,
}

/// One message on a discussion thread — an agent or the user commenting on a
/// ticket (`ticket = Some`) or on the team channel (`ticket = None`). This is
/// the teamwork surface the original workflow lacked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    /// Stable id (minted on post) so reactions can target a specific comment.
    #[serde(default)]
    pub id: String,
    pub at: String,
    /// Author: an agent role (e.g. `SM`, `PO`) or `USER`.
    pub author: String,
    /// For an agent message, the worker identity (`operator@host`) that produced
    /// it — so you can tell whose DEV/SA/... posted, when several run in parallel.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    /// Files/images attached to the comment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// Emoji reactions, each with the users who reacted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<Reaction>,
}

/// Keep discussion threads bounded per project.
/// One human-to-human message in the project's team chat channel. Unlike
/// [`Comment`] (which is dominated by agent scrum chatter), this is a plain
/// channel for the people on the project to talk to each other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMsg {
    /// Stable id (minted on post) so reactions can target a specific message.
    #[serde(default)]
    pub id: String,
    pub at: String,
    /// The authenticated username of the sender.
    pub user: String,
    pub body: String,
    /// Edited timestamp (set when message is edited)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited: Option<String>,
    /// The channel this message belongs to. Defaults to [`GENERAL_CHANNEL`] for
    /// messages written before channels existed.
    #[serde(default = "general_channel")]
    pub channel: String,
    /// Files/images attached to the message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// Emoji reactions, each with the users who reacted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<Reaction>,
    /// Thread parent message id (absent for top-level messages)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// Number of thread replies (populated for top-level messages)
    #[serde(default)]
    pub reply_count: u32,
    /// Whether this message was deleted (soft delete)
    #[serde(default)]
    pub deleted: bool,
}

/// One emoji reaction on a message and the users who added it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    pub emoji: String,
    pub users: Vec<String>,
}

impl ChatMsg {
    #[must_use]
    pub fn reply(user: &str, body: &str, channel: &str, thread_id: &str) -> Self {
        Self {
            id: mint_id(),
            at: now_rfc3339(),
            user: user.to_owned(),
            body: body.to_owned(),
            edited: None,
            channel: channel.to_owned(),
            attachments: Vec::new(),
            reactions: Vec::new(),
            thread_id: Some(thread_id.to_owned()),
            reply_count: 0,
            deleted: false,
        }
    }
}

/// Keep the team chat bounded per project.
/// A Slack-style chat channel. `#general` is implicit (open to everyone, no
/// owner); every other channel is private to its `members`, created and owned
/// by one person who may delegate invite rights to others via `inviters`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    /// URL-safe slug used as the stable id (e.g. `design-review`).
    pub id: String,
    /// Human display name.
    pub name: String,
    /// Username of the owner. Empty for the system `#general` channel.
    pub owner: String,
    /// Members who can see and post. Empty for `#general` (everyone).
    #[serde(default)]
    pub members: Vec<String>,
    /// Members the owner delegated invite permission to (owner always can).
    #[serde(default)]
    pub inviters: Vec<String>,
    #[serde(default)]
    pub created_at: String,
    /// Channel kind: `"general"` (everyone), `"project"` (auto-mirrors a
    /// project's membership, id = `#<alias>`), or `"private"` (owner-created).
    #[serde(default = "chan_kind_private")]
    pub kind: String,
    /// For a `"project"` channel, the project id it mirrors. Empty otherwise.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub project: String,
    /// Optional channel topic/description
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub topic: String,
    /// Parent channel id when this is a sub-channel, empty at the top level.
    /// A sub-channel is a room inside a room — same members by default, its own
    /// thread of conversation — so a project channel does not have to carry
    /// every side discussion.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent: String,
    /// Whether any member may invite others. Off by default: a private channel
    /// anyone can add people to is a privacy surprise, so this is the setting a
    /// team turns ON deliberately, not one they discover.
    #[serde(default)]
    pub open_invite: bool,
}

fn chan_kind_private() -> String {
    "private".to_owned()
}

impl Channel {
    /// The open, everyone-can-see `#general` channel.
    #[must_use]
    pub fn is_general(&self) -> bool {
        self.id == GENERAL_CHANNEL
    }

    /// An open system channel everyone can read (`#general`, `#agents`).
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.id == GENERAL_CHANNEL || self.id == AGENTS_CHANNEL
    }

    /// Whether `user` may see and read this channel.
    #[must_use]
    pub fn can_view(&self, user: &str) -> bool {
        self.is_open() || self.owner == user || self.members.iter().any(|m| m == user)
    }

    /// Whether `user` may invite others. With `open_invite` any member can;
    /// otherwise it is the owner and whoever they delegated it to. Admins are
    /// handled above this, at the endpoint: their authority does not depend on
    /// a channel's settings.
    #[must_use]
    pub fn can_invite(&self, user: &str) -> bool {
        if self.is_open() {
            return true;
        }
        if self.owner == user || self.inviters.iter().any(|m| m == user) {
            return true;
        }
        self.open_invite && self.members.iter().any(|m| m == user)
    }

    /// Whether `user` may remove members. Never the whole membership — losing
    /// someone from a room is not something a room-mate should be able to do
    /// to another on a whim.
    #[must_use]
    pub fn can_kick(&self, user: &str) -> bool {
        !self.is_open() && (self.owner == user || self.inviters.iter().any(|m| m == user))
    }

    /// Whether this channel may be made private. `#general` may not: a team
    /// needs one room nobody can be shut out of.
    #[must_use]
    pub fn can_change_privacy(&self) -> bool {
        self.id != GENERAL_CHANNEL
    }
}

/// One agent asking another a question it must not guess the answer to.
///
/// A developer who cannot tell what the requirement means, or a BA who does
/// not know what the product already does, has exactly one correct move: ask
/// the person who knows. Without this the only options were to guess and fail
/// a gate, or stall — which is how a ticket burned three attempts on the same
/// misunderstanding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentQuestion {
    /// Stable id (`<ticket>#<n>` or `open#<n>` when not about one ticket).
    pub id: String,
    /// Ticket the question is about; empty for a product-level question.
    pub ticket: String,
    /// Role label that asked (`DEV-BUG`, `BA`).
    pub from: String,
    /// Role label expected to answer (`BA`, `SA`).
    pub to: String,
    /// The question, as asked.
    pub body: String,
    /// The answer; empty while unanswered.
    #[serde(default)]
    pub answer: String,
    pub asked_at: String,
    #[serde(default)]
    pub answered_at: String,
    /// Whether this question has already been handed to the other role once.
    /// A second forward would be two roles passing it back and forth.
    #[serde(default)]
    pub forwarded: bool,
    /// Whether the SLA escalation for a human-addressed question has fired —
    /// once: repeated escalation is just a second kind of spam.
    #[serde(default)]
    pub escalated: bool,
}

impl AgentQuestion {
    /// Whether this question is still waiting for an answer.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.answer.trim().is_empty()
    }
}

#[cfg(test)]
mod channel_tests {
    use super::{slugify, ProjectState, AGENTS_CHANNEL, GENERAL_CHANNEL};

    #[test]
    fn slugify_makes_safe_ids() {
        assert_eq!(slugify("Design Review!"), "design-review");
        assert_eq!(slugify("  Q4   Planning  "), "q4-planning");
        assert_eq!(slugify("###"), "");
    }

    #[test]
    fn create_and_view_permissions() {
        let mut s = ProjectState::default();
        let ch = s.create_channel("Design Review", "alice").expect("create");
        assert_eq!(ch.id, "design-review");
        assert!(ch.can_view("alice"));
        assert!(!ch.can_view("bob"));
        // general + agents are always visible; alice additionally sees hers.
        assert_eq!(s.channels_for("alice").len(), 3);
        assert_eq!(s.channels_for("bob").len(), 2);
        assert_eq!(s.channels_for("bob")[0].id, GENERAL_CHANNEL);
        assert_eq!(s.channels_for("bob")[1].id, AGENTS_CHANNEL);
    }

    #[test]
    fn duplicate_channel_rejected() {
        let mut s = ProjectState::default();
        s.create_channel("Design", "alice").expect("first");
        assert!(s.create_channel("design", "bob").is_err());
        assert!(s.create_channel("general", "bob").is_err());
    }

    #[test]
    fn invite_requires_permission_then_grants_view() {
        let mut s = ProjectState::default();
        s.create_channel("Secret", "alice").expect("create");
        // bob can't invite; alice can.
        assert!(s.invite_to_channel("secret", "bob", "carol").is_err());
        s.invite_to_channel("secret", "alice", "bob")
            .expect("invite");
        assert!(s.channel("secret").expect("ch").can_view("bob"));
        // bob still can't invite (not delegated).
        assert!(s.invite_to_channel("secret", "bob", "carol").is_err());
    }

    #[test]
    fn delegation_lets_grantee_invite() {
        let mut s = ProjectState::default();
        s.create_channel("Secret", "alice").expect("create");
        assert!(s.delegate_invite("secret", "bob", "carol").is_err());
        s.delegate_invite("secret", "alice", "bob")
            .expect("delegate");
        s.invite_to_channel("secret", "bob", "carol")
            .expect("bob invites");
        assert!(s.channel("secret").expect("ch").can_view("carol"));
    }
}

/// A short, collision-free message id (nanos + a process-local counter).
#[cfg(test)]
mod comment_tests {
    use super::{ProjectState, MAX_COMMENTS};

    #[test]
    fn posts_and_bounds_the_thread() {
        let mut s = ProjectState::default();
        s.post_comment("SM", "hello", None);
        s.post_comment("USER", "hi", Some("CXC-F001".to_owned()));
        assert_eq!(s.comments.len(), 2);
        assert_eq!(s.comments[0].author, "SM");
        assert_eq!(s.comments[1].ticket.as_deref(), Some("CXC-F001"));
        for i in 0..MAX_COMMENTS + 10 {
            s.post_comment("BA", &format!("m{i}"), None);
        }
        assert_eq!(s.comments.len(), MAX_COMMENTS);
        // Oldest were dropped; the very latest survives.
        assert_eq!(
            s.comments.last().unwrap().body,
            format!("m{}", MAX_COMMENTS + 9)
        );
    }
}

#[cfg(test)]
mod question_tests {
    use super::ProjectState;

    #[test]
    fn one_open_question_per_ticket_and_answers_are_readable_back() {
        let mut s = ProjectState::default();
        assert!(s.ask_question("COX-B1", "DEV-BUG", "BA", "what does archive mean?"));
        // A second unanswered question means the first was not the blocker.
        assert!(
            !s.ask_question("COX-B1", "DEV-BUG", "BA", "and what about purge?"),
            "a ticket may hold only one open question"
        );
        // A different ticket is unaffected.
        assert!(s.ask_question("COX-B2", "BA", "SA", "does the product already export?"));
        let open = s.open_question("COX-B1").expect("open");
        assert_eq!((open.from.as_str(), open.to.as_str()), ("DEV-BUG", "BA"));
        assert!(s.answered_questions("COX-B1").is_empty());

        let id = open.id.clone();
        assert!(s.answer_question(&id, "  soft-delete: the row stays, hidden  "));
        assert!(
            !s.answer_question(&id, "   "),
            "a blank answer is no answer"
        );
        assert!(s.open_question("COX-B1").is_none(), "no longer waiting");
        let answered = s.answered_questions("COX-B1");
        assert_eq!(answered.len(), 1);
        assert_eq!(answered[0].answer, "soft-delete: the row stays, hidden");
        // Asking again is allowed once the first is answered.
        assert!(s.ask_question("COX-B1", "DEV-BUG", "BA", "and what about purge?"));
    }

    #[test]
    fn a_question_may_be_handed_over_once_then_must_be_answered() {
        let mut s = ProjectState::default();
        assert!(s.ask_question("COX-B1", "DEV-BUG", "BA", "is archive a soft delete?"));
        let id = s.open_question("COX-B1").expect("open").id.clone();
        // The BA reads it as a systems question and hands it to the SA.
        assert!(s.forward_question(&id, "SA"));
        assert_eq!(s.open_question("COX-B1").expect("open").to, "SA");
        // A second hand-off would be the two roles passing it back and forth.
        assert!(
            !s.forward_question(&id, "BA"),
            "one hop only — after that someone has to read the code and answer"
        );
        // Handing it to the role that already holds it is not a hand-off.
        assert!(!s.forward_question(&id, "SA"));
        assert!(s.answer_question(&id, "soft delete; rows stay, hidden by a flag"));
        assert!(
            !s.forward_question(&id, "BA"),
            "answered questions do not move"
        );
    }

    #[test]
    fn empty_questions_are_not_recorded() {
        let mut s = ProjectState::default();
        assert!(!s.ask_question("COX-B1", "DEV-BUG", "BA", "   "));
        assert!(s.questions.is_empty());
    }
}

#[cfg(test)]
mod channel_settings_tests {
    use super::{ProjectState, GENERAL_CHANNEL};

    #[test]
    fn a_sub_channel_inherits_the_room_it_was_opened_inside() {
        let mut s = ProjectState::default();
        let parent = s.create_channel("Design", "alice").expect("parent");
        s.invite_to_channel(&parent.id, "alice", "bob")
            .expect("invite");
        let sub = s
            .create_sub_channel(&parent.id, "Icons", "alice", "private")
            .expect("sub");
        assert_eq!(sub.parent, parent.id);
        assert!(
            sub.members.iter().any(|m| m == "bob"),
            "the people already in the conversation follow it without a second invite"
        );
        // Someone outside the parent cannot open a room inside it.
        assert!(s
            .create_sub_channel(&parent.id, "Nope", "mallory", "private")
            .is_err());
    }

    #[test]
    fn general_may_never_be_made_private() {
        let mut s = ProjectState::default();
        s.channels.push(super::Channel {
            id: GENERAL_CHANNEL.to_owned(),
            name: "general".to_owned(),
            owner: String::new(),
            members: Vec::new(),
            inviters: Vec::new(),
            created_at: String::new(),
            kind: "general".to_owned(),
            project: String::new(),
            topic: String::new(),
            parent: String::new(),
            open_invite: false,
        });
        let err = s
            .update_channel_settings(GENERAL_CHANNEL, Some("private"), None, None)
            .expect_err("must refuse");
        assert!(err.contains("#general"), "{err}");
        // Its other settings still move.
        assert!(s
            .update_channel_settings(GENERAL_CHANNEL, None, None, Some("say hi"))
            .is_ok());
    }

    #[test]
    fn closing_invites_narrows_who_can_add_people() {
        let mut s = ProjectState::default();
        let ch = s.create_channel("Ops", "alice").expect("channel");
        s.invite_to_channel(&ch.id, "alice", "bob").expect("invite");
        // Closed by default: a plain member cannot bring someone in.
        assert!(!s.channels[0].can_invite("bob"));
        s.update_channel_settings(&ch.id, None, Some(true), None)
            .expect("open invites");
        assert!(
            s.channels[0].can_invite("bob"),
            "turning it on is what lets members invite"
        );
        s.update_channel_settings(&ch.id, None, Some(false), None)
            .expect("close again");
        let ch = &s.channels[0];
        assert!(!ch.can_invite("bob"), "a plain member no longer can");
        assert!(ch.can_invite("alice"), "the owner always can");
        assert!(!ch.can_kick("bob"), "and cannot remove anyone");
        assert!(ch.can_kick("alice"));
    }

    #[test]
    fn the_owner_cannot_be_removed_from_their_own_channel() {
        let mut s = ProjectState::default();
        let ch = s.create_channel("Ops", "alice").expect("channel");
        s.invite_to_channel(&ch.id, "alice", "bob").expect("invite");
        assert!(s.remove_channel_member(&ch.id, "alice").is_err());
        let after = s.remove_channel_member(&ch.id, "bob").expect("removed");
        assert!(!after.members.iter().any(|m| m == "bob"));
    }
}
