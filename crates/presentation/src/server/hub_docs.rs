// Part of the server module split by concern — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Hub-level persisted documents: the system chat, workspace settings,
//! spaces, and meetings — each a JSON doc with a small lock-guarded handle —
//! plus the request shapes their endpoints accept.

use super::*;

/// A project's live team-chat channel: a broadcast fan-out to every connected
/// WebSocket, plus a mutex that serializes the load→append→save of chat writes
/// so two simultaneous messages can't clobber each other.
#[derive(Clone)]
pub(super) struct ChatChannel {
    pub(super) tx: tokio::sync::broadcast::Sender<String>,
    pub(super) write_lock: Arc<tokio::sync::Mutex<()>>,
}

/// Hub-level, system-wide chat: one store shared across every project. Holds the
/// [`SystemChat`] aggregate (private channels + all messages) behind a mutex,
/// the JSON file it persists to, a broadcast bus for live WebSockets, and the
/// directory uploaded chat media lives in.
/// Hub-wide chat key in the shared KV store.
const SYSCHAT_KEY: &str = "system_chat";

#[derive(Clone)]
pub(super) struct SysChat {
    pub(super) inner: Arc<tokio::sync::Mutex<coxagent_application::SystemChat>>,
    /// Local-file fallback path, used only when no shared store is configured.
    pub(super) path: PathBuf,
    /// Shared DB store (Postgres). When set, it is the system of record and the
    /// file is not touched.
    pub(super) store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
    pub(super) tx: tokio::sync::broadcast::Sender<String>,
}

impl SysChat {
    /// Load the store from the shared DB when `store` is set, else from
    /// `dir/system_chat.json` (empty if absent).
    pub(super) async fn load(
        dir: &std::path::Path,
        store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
    ) -> Self {
        let path = dir.join("system_chat.json");
        let text = if let Some(s) = &store {
            s.load(SYSCHAT_KEY).await.ok().flatten()
        } else {
            std::fs::read_to_string(&path).ok()
        };
        let inner = text
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(inner)),
            path,
            store,
            tx: tokio::sync::broadcast::channel(256).0,
        }
    }

    /// Persist the current state (best-effort) to the shared DB, or the local
    /// file when no store is configured.
    pub(super) async fn save(&self) {
        let json = { serde_json::to_string(&*self.inner.lock().await).unwrap_or_default() };
        if let Some(s) = &self.store {
            if let Err(e) = s.save(SYSCHAT_KEY, &json).await {
                tracing::warn!("system chat save failed: {e}");
            }
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&self.path, json);
    }
}

/// Workspace identity + invites: the company-level document (name, branding,
/// pending invite links) persisted in the shared KV store (Postgres) when
/// configured, else a local `workspace.json` under the hub dir.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct WorkspaceDoc {
    #[serde(default)]
    pub(super) name: String,
    #[serde(default)]
    pub(super) tagline: String,
    #[serde(default)]
    pub(super) accent: String,
    /// Company-wide engineering conventions (coding standards, style, do/don't).
    /// Injected into every agent's prompt across every project.
    #[serde(default)]
    pub(super) conventions: String,
    #[serde(default)]
    pub(super) invites: Vec<Invite>,
    /// Per-project public share links (CXA-F069): unguessable tokens that each
    /// unlock one project's read-only status page. Kept here — the company-level
    /// doc — so admins can list/revoke them across projects in one place, the
    /// same home as the invite tokens.
    #[serde(default)]
    pub(super) share_links: Vec<ShareLink>,
    /// Client-app distribution: where users download CoXAgent for each
    /// platform, refreshed automatically from GitHub Releases when
    /// `releases_repo` is set (manual URLs act as overrides).
    #[serde(default)]
    pub(super) downloads: DownloadsCfg,
}

/// Per-platform download links + the release source of truth.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct DownloadsCfg {
    /// `owner/name` GitHub repo whose Releases carry the app builds. When set,
    /// a background task polls the latest release and fills version + asset
    /// URLs automatically after every deploy that tags a release.
    #[serde(default)]
    pub(super) releases_repo: String,
    /// Newest published app version (auto from releases, or set manually).
    #[serde(default)]
    pub(super) latest_version: String,
    #[serde(default)]
    pub(super) macos: String,
    #[serde(default)]
    pub(super) windows: String,
    #[serde(default)]
    pub(super) linux: String,
    /// App Store / TestFlight link — iOS can't sideload, so this is a URL only.
    #[serde(default)]
    pub(super) ios: String,
    /// Release notes of the latest version (from the GitHub release body,
    /// capped) — shown as "What's new" in the update modal.
    #[serde(default)]
    pub(super) notes: String,
}

/// One shareable invite link: whoever opens it can create their own account
/// with the preset role + project membership, `uses_left` times.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct Invite {
    pub(super) token: String,
    pub(super) role: String,
    #[serde(default)]
    pub(super) projects: Vec<String>,
    pub(super) created_by: String,
    pub(super) created_at: String,
    pub(super) uses_left: u32,
}

/// One public share link (CXA-F069): whoever holds `token` can open
/// `/s/<token>` and read that project's status page — no login. The token IS
/// the credential and the record's lookup key, so it is minted by a CSPRNG;
/// revocation flips `revoked`, killing the URL on the next request. Revoked
/// records are kept (not deleted) so the Settings list can show what was
/// issued and when.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct ShareLink {
    pub(super) token: String,
    /// The project this link unlocks.
    pub(super) project_id: String,
    /// Optional admin label (e.g. "ACME client").
    #[serde(default)]
    pub(super) name: String,
    pub(super) created_by: String,
    pub(super) created_at: String,
    #[serde(default)]
    pub(super) revoked: bool,
}

/// One space: an organizational unit grouping projects + members under its own
/// admins. Spaces live in the shared KV (`app_kv` key `spaces`); a normal admin
/// manages only spaces that list them, a super admin manages all.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Space {
    /// URL-safe slug id.
    pub(super) id: String,
    pub(super) name: String,
    #[serde(default)]
    pub(super) tagline: String,
    /// Usernames who administer THIS space (invite, edit, assign projects).
    #[serde(default)]
    pub(super) admins: Vec<String>,
    /// Project ids belonging to this space.
    #[serde(default)]
    pub(super) projects: Vec<String>,
    /// Explicit member usernames. Saving the space additionally ASSIGNS each
    /// member to every project of the space (additive — never auto-revokes).
    #[serde(default)]
    pub(super) members: Vec<String>,
    /// Monthly USD spend cap for this space; 0 = no cap. Set by Super only.
    #[serde(default)]
    pub(super) budget_usd: f64,
    #[serde(default)]
    pub(super) created_by: String,
    #[serde(default)]
    pub(super) created_at: String,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct SpacesDoc {
    #[serde(default)]
    pub(super) spaces: Vec<Space>,
}

/// The hub-wide spaces store (see [`SpacesDoc`]).
#[derive(Clone)]
pub(super) struct Sp {
    pub(super) inner: Arc<tokio::sync::Mutex<SpacesDoc>>,
    pub(super) path: PathBuf,
    pub(super) store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
}

impl Sp {
    pub(super) async fn load(
        dir: &std::path::Path,
        store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
    ) -> Self {
        let path = dir.join("spaces.json");
        let text = if let Some(s) = &store {
            s.load("spaces").await.ok().flatten()
        } else {
            std::fs::read_to_string(&path).ok()
        };
        let inner = text
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(inner)),
            path,
            store,
        }
    }

    pub(super) async fn save(&self) {
        let json = { serde_json::to_string(&*self.inner.lock().await).unwrap_or_default() };
        if let Some(s) = &self.store {
            if let Err(e) = s.save("spaces", &json).await {
                tracing::warn!("spaces save failed: {e}");
            }
            return;
        }
        let _ = std::fs::write(&self.path, json);
    }
}

/// The hub-wide workspace store (see [`WorkspaceDoc`]).
#[derive(Clone)]
pub(super) struct Ws {
    pub(super) inner: Arc<tokio::sync::Mutex<WorkspaceDoc>>,
    pub(super) path: PathBuf,
    pub(super) store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
}

impl Ws {
    pub(super) async fn load(
        dir: &std::path::Path,
        store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
    ) -> Self {
        let path = dir.join("workspace.json");
        let text = if let Some(s) = &store {
            s.load("workspace").await.ok().flatten()
        } else {
            std::fs::read_to_string(&path).ok()
        };
        let inner = text
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(inner)),
            path,
            store,
        }
    }

    pub(super) async fn save(&self) {
        let json = { serde_json::to_string(&*self.inner.lock().await).unwrap_or_default() };
        if let Some(s) = &self.store {
            if let Err(e) = s.save("workspace", &json).await {
                tracing::warn!("workspace save failed: {e}");
            }
            return;
        }
        let _ = std::fs::write(&self.path, json);
    }
}

/// A booked meeting. Times are RFC3339 UTC; the watchdog drives reminders,
/// start announcements, and auto-ringing of absent participants.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
#[allow(clippy::struct_excessive_bools)] // a persisted data aggregate, not a state machine
pub(super) struct Meeting {
    pub(super) id: String,
    pub(super) title: String,
    /// RFC3339 start instant.
    pub(super) start: String,
    pub(super) duration_min: u32,
    pub(super) created_by: String,
    pub(super) participants: Vec<String>,
    /// Minutes before start to remind (0 = no reminder).
    #[serde(default)]
    pub(super) remind_min: u32,
    /// Who has actually entered the meeting room.
    #[serde(default)]
    pub(super) joined: Vec<String>,
    #[serde(default)]
    pub(super) reminded: bool,
    #[serde(default)]
    pub(super) start_announced: bool,
    /// One automatic ring of the not-yet-joined, ~1 min after start.
    #[serde(default)]
    pub(super) auto_rang: bool,
    #[serde(default)]
    pub(super) cancelled: bool,
    #[serde(default)]
    pub(super) agenda: String,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(super) struct MeetingsDoc {
    pub(super) meetings: Vec<Meeting>,
}

/// Meeting store: shared KV (`app_kv` key `meetings`) when configured, else a
/// local `meetings.json` under the hub dir — same shape as [`Ws`].
#[derive(Clone)]
pub(super) struct Mt {
    pub(super) inner: Arc<tokio::sync::Mutex<MeetingsDoc>>,
    pub(super) path: PathBuf,
    pub(super) store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
}

impl Mt {
    pub(super) async fn load(
        dir: &std::path::Path,
        store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
    ) -> Self {
        let path = dir.join("meetings.json");
        let text = if let Some(s) = &store {
            s.load("meetings").await.ok().flatten()
        } else {
            std::fs::read_to_string(&path).ok()
        };
        let inner = text
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(inner)),
            path,
            store,
        }
    }

    pub(super) async fn save(&self) {
        let json = { serde_json::to_string(&*self.inner.lock().await).unwrap_or_default() };
        if let Some(s) = &self.store {
            if let Err(e) = s.save("meetings", &json).await {
                tracing::warn!("meetings save failed: {e}");
            }
            return;
        }
        let _ = std::fs::write(&self.path, json);
    }
}

pub(super) fn parse_rfc3339(s: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
}

#[derive(serde::Deserialize)]
pub(super) struct MeetingReq {
    pub(super) title: String,
    pub(super) start: String,
    pub(super) duration_min: Option<u32>,
    pub(super) participants: Vec<String>,
    #[serde(default)]
    pub(super) remind_min: Option<u32>,
    #[serde(default)]
    pub(super) agenda: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct MeetingPatch {
    #[serde(default)]
    pub(super) cancel: Option<bool>,
    #[serde(default)]
    pub(super) title: Option<String>,
    #[serde(default)]
    pub(super) start: Option<String>,
    #[serde(default)]
    pub(super) duration_min: Option<u32>,
    #[serde(default)]
    pub(super) participants: Option<Vec<String>>,
    #[serde(default)]
    pub(super) agenda: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct MeetingRingReq {
    pub(super) user: String,
}
