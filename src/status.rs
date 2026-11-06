//! Workspace status as reported by `status` and published to desktop hosts by
//! the runner (ADR-0009). The model is decided in ADR-0010: one workspace
//! summary with a fixed precedence, plus a stage per session.
//!
//! Computing a snapshot never writes state or takes the workspace operation
//! lock, so it works on a read-only `Workspace::inspect` (ADR-0007).

use crate::{
    engine::Workspace,
    model::{Identity, SavedCopy, Session, now},
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
    /// When the session's first edit was captured.
    pub started_at: u64,
    /// When its latest edit was captured.
    pub last_saved: u64,
    /// The summary title it was added to shared history with; `None` until published.
    pub title: Option<String>,
    /// Requested purposes of its recorded intents, in recorded order, without repeats.
    pub purposes: Vec<String>,
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
    /// Whether the workspace synchronizes with a remote at all.
    pub has_remote: bool,
    /// When a sync with the remote last succeeded, to the minute; `None` if never.
    pub last_sync: Option<u64>,
    /// Files Mesh moved aside before writing incoming changes, oldest first.
    /// Each lives in `.mesh/recovery/<id>/content`.
    pub saved_copies: Vec<SavedCopy>,
}

/// What is known about the live folder beyond committed state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalObservation {
    /// Saved files differ from the last captured view.
    pub changes_waiting: bool,
    /// Why the live folder could not be read, if it could not.
    pub capture_error: Option<String>,
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

    /// Status as the CLI reports it: scans the live folder, read-only.
    pub fn status_snapshot(&self) -> Status {
        let scan = self.scan();
        self.status_from(LocalObservation {
            capture_error: scan.as_ref().err().map(|error| format!("{error:#}")),
            changes_waiting: scan
                .as_ref()
                .is_ok_and(|files| files != &self.state.observed),
        })
    }

    /// Status from committed state plus what the caller already knows about the
    /// live folder. The runner supplies its own capture results here, so
    /// publishing a snapshot every tick costs no extra scan.
    pub fn status_from(&self, local: LocalObservation) -> Status {
        let LocalObservation {
            changes_waiting: local_changes_waiting,
            capture_error,
        } = local;
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
                    started_at: s.created_at,
                    last_saved: s.last_edit,
                    title: s
                        .message
                        .as_ref()
                        .filter(|_| s.published)
                        .map(|m| m.title.clone()),
                    purposes: purposes(s),
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
            has_remote: self.config.remote.is_some(),
            last_sync: self.state.last_sync,
            saved_copies: self.saved_copies(),
        }
    }
}

/// A session's requested purposes in the order they were recorded.
fn purposes(session: &Session) -> Vec<String> {
    let mut intents: Vec<_> = session.intents.values().collect();
    intents.sort_by_key(|intent| intent.created_at);
    let mut purposes: Vec<String> = Vec::new();
    for intent in intents {
        if !purposes.contains(&intent.purpose) {
            purposes.push(intent.purpose.clone());
        }
    }
    purposes
}
