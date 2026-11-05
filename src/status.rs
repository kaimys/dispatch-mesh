//! Workspace status as reported by `status` and, later, published to desktop
//! hosts. The model is decided in ADR-0010: one workspace summary with a fixed
//! precedence, plus a stage per session.
//!
//! Computing a snapshot never writes state or takes the workspace operation
//! lock, so it works on a read-only `Workspace::inspect` (ADR-0007).

use crate::{
    engine::Workspace,
    model::{Identity, Session, now},
};
use serde::Serialize;
use std::collections::BTreeSet;

/// Workspace summary, listed in precedence order: the first that applies wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Summary {
    NeedsAttention,
    Offline,
    SavedLocally,
    DraftUploaded,
    WaitingForFinalization,
    Synced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    SavedLocally,
    DraftUploaded,
    WaitingForFinalization,
    Published,
    Discarded,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SessionStatus {
    pub id: String,
    pub author: Identity,
    pub stage: Stage,
    pub closed: bool,
    pub published: bool,
    pub discarded: bool,
    pub files: Vec<String>,
    pub dependencies: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConflictStatus {
    pub id: String,
    pub path: String,
    pub binary: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Presence {
    pub path: String,
    pub author: Identity,
    pub device: String,
    pub last_saved: u64,
    pub signal: &'static str,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Status {
    pub status: Summary,
    /// Cause of `needs_attention` or `offline`; `None` otherwise.
    pub reason: Option<String>,
    pub device: String,
    pub main: String,
    pub commit_message_mode: &'static str,
    pub attention: Option<String>,
    pub offline: Option<String>,
    pub summary_failure: Option<String>,
    pub capture_error: Option<String>,
    pub local_changes_waiting: bool,
    pub recovery_pending: bool,
    pub active_session: Option<String>,
    pub sessions: Vec<SessionStatus>,
    pub unresolved_conflicts: Vec<ConflictStatus>,
    pub presence: Vec<Presence>,
}

impl Workspace {
    /// Where a session is on its way to `main`. Sessions received from another
    /// device reached us through the remote, so they are uploaded by definition;
    /// a workspace without a remote has no upload step.
    pub fn session_stage(&self, session: &Session) -> Stage {
        if session.published {
            return Stage::Published;
        }
        if session.discarded {
            return Stage::Discarded;
        }
        let own = session.device == self.config.device;
        let local_only = own && self.config.remote.is_none();
        let uploaded = !own || self.state.uploaded.get(&session.id) == Some(&session.revision);
        match (session.closed, local_only, uploaded) {
            (false, false, true) => Stage::DraftUploaded,
            (false, _, _) => Stage::SavedLocally,
            (true, true, _) | (true, false, true) => Stage::WaitingForFinalization,
            (true, false, false) => Stage::SavedLocally,
        }
    }

    pub fn status_snapshot(&self) -> Status {
        let scan = self.scan();
        let capture_error = scan.as_ref().err().map(|error| format!("{error:#}"));
        let local_changes_waiting = scan
            .as_ref()
            .is_ok_and(|files| files != &self.state.observed);
        let unresolved: Vec<_> = self
            .unresolved()
            .map(|c| ConflictStatus {
                id: c.id.clone(),
                path: c.path.clone(),
                binary: c.binary,
            })
            .collect();
        let own_stages: Vec<_> = self
            .state
            .manifest
            .sessions
            .values()
            .filter(|s| s.device == self.config.device)
            .map(|s| self.session_stage(s))
            .collect();
        let has = |stage| own_stages.contains(&stage);

        let attention_reason = if !unresolved.is_empty() {
            Some(match unresolved.len() {
                1 => "1 unresolved conflict".to_string(),
                n => format!("{n} unresolved conflicts"),
            })
        } else if let Some(error) = &capture_error {
            Some(format!("Local capture waiting: {error}"))
        } else {
            self.state
                .attention
                .clone()
                .or_else(|| self.state.summary_failure.clone())
        };
        let (status, reason) = if attention_reason.is_some() {
            (Summary::NeedsAttention, attention_reason)
        } else if self.state.offline.is_some() {
            (Summary::Offline, self.state.offline.clone())
        } else if local_changes_waiting || has(Stage::SavedLocally) {
            (Summary::SavedLocally, None)
        } else if has(Stage::DraftUploaded) {
            (Summary::DraftUploaded, None)
        } else if has(Stage::WaitingForFinalization) {
            (Summary::WaitingForFinalization, None)
        } else {
            (Summary::Synced, None)
        };

        let timestamp = now();
        let presence = self
            .state
            .manifest
            .sessions
            .values()
            .filter(|s| !s.closed && !s.settled())
            .flat_map(|s| {
                s.changes
                    .iter()
                    .map(move |(path, change)| (s, path, change))
            })
            .filter(|(_, _, change)| timestamp.saturating_sub(change.last_edit) < 300)
            .map(|(s, path, change)| Presence {
                path: path.clone(),
                author: s.author.clone(),
                device: s.device.clone(),
                last_saved: change.last_edit,
                signal: "recent_save",
            })
            .collect();

        Status {
            status,
            reason,
            device: self.config.device.clone(),
            main: self.state.main.clone(),
            commit_message_mode: match &self.config.provider {
                None => "local_fallback",
                Some(provider) if provider.send_text => "ai_with_text",
                Some(_) => "local_metadata",
            },
            attention: self.state.attention.clone(),
            offline: self.state.offline.clone(),
            summary_failure: self.state.summary_failure.clone(),
            capture_error,
            local_changes_waiting,
            recovery_pending: self.state.applying.is_some(),
            active_session: self.active_session().map(|s| s.id.clone()),
            sessions: self
                .state
                .manifest
                .sessions
                .values()
                .map(|s| SessionStatus {
                    id: s.id.clone(),
                    author: s.author.clone(),
                    stage: self.session_stage(s),
                    closed: s.closed,
                    published: s.published,
                    discarded: s.discarded,
                    files: s.changes.keys().cloned().collect(),
                    dependencies: s.dependencies.clone(),
                })
                .collect(),
            unresolved_conflicts: unresolved,
            presence,
        }
    }
}
