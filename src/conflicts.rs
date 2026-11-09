//! What a conflict shows next to its record: who made each version and, on
//! demand, the versions themselves (US00006, ADR-0010).
//!
//! The list metadata is derived from committed state in memory, so every
//! status snapshot can carry it. [`Workspace::conflict_detail`] reads one
//! conflict's bytes and consults Git for history; it works on a read-only
//! [`Workspace::inspect`] and never waits for the runner.

use crate::{
    engine::Workspace,
    git::CommitInfo,
    model::{Conflict, Session},
    status::{ConflictSide, ConflictStatus, Contributor, purposes_covering},
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;

/// One conflict with its three versions, read on demand.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConflictDetail {
    #[serde(flatten)]
    pub status: ConflictStatus,
    #[serde(skip)]
    pub base: Option<Vec<u8>>,
    /// The version in the folder.
    #[serde(skip)]
    pub current: Option<Vec<u8>>,
    /// The version kept aside.
    #[serde(skip)]
    pub other: Option<Vec<u8>>,
    /// When this device saved the file again after the conflict was recorded:
    /// the folder no longer shows the current version. `None` otherwise.
    pub edited_after: Option<u64>,
    /// The commit on `main` that made the current version, only when its
    /// content is exactly that version. A later commit may postdate it.
    pub current_history: Option<CommitInfo>,
    /// The same lookup for the common base.
    pub base_history: Option<CommitInfo>,
}

fn contributor(session: &Session, path: &str) -> Contributor {
    Contributor {
        author: session.author.clone(),
        device: session.device.clone(),
        last_saved: session
            .changes
            .get(path)
            .map_or(session.last_edit, |change| change.last_edit),
        purposes: purposes_covering(session, Some(path)),
    }
}

fn size(version: &Option<Vec<u8>>) -> u64 {
    version.as_ref().map_or(0, |bytes| bytes.len() as u64)
}

impl Workspace {
    /// The list metadata of one conflict, from committed state only.
    pub(crate) fn conflict_status(&self, conflict: &Conflict) -> ConflictStatus {
        let sessions = &self.state.manifest.sessions;
        let mut current: Vec<Contributor> = conflict
            .accepted_sessions
            .keys()
            .filter_map(|id| sessions.get(id))
            .map(|session| contributor(session, &conflict.path))
            .collect();
        current.sort_by_key(|c| std::cmp::Reverse(c.last_saved));
        // The record names the other version's session; keep its author even
        // if the session record itself is gone.
        let other = match sessions.get(&conflict.session) {
            Some(session) => contributor(session, &conflict.path),
            None => Contributor {
                author: conflict.author.clone(),
                device: String::new(),
                last_saved: conflict.created_at,
                purposes: Vec::new(),
            },
        };
        ConflictStatus {
            id: conflict.id.clone(),
            path: conflict.path.clone(),
            binary: conflict.binary,
            created_at: conflict.created_at,
            current: ConflictSide {
                shared_history: current.is_empty(),
                contributors: current,
                deleted: conflict.accepted.is_none(),
                size: size(&conflict.accepted),
            },
            other: ConflictSide {
                contributors: vec![other],
                shared_history: false,
                deleted: conflict.incoming.is_none(),
                size: size(&conflict.incoming),
            },
        }
    }

    /// One unresolved conflict's versions, history and whether the file was
    /// saved again since. Errors for an unknown or already resolved conflict.
    pub fn conflict_detail(&self, id: &str) -> Result<ConflictDetail> {
        let conflict = self.unresolved_conflict(id)?;
        Ok(ConflictDetail {
            status: self.conflict_status(conflict),
            current_history: self.history_of(&conflict.path, &conflict.accepted),
            base_history: self.history_of(&conflict.path, &conflict.base),
            base: conflict.base.clone(),
            current: conflict.accepted.clone(),
            other: conflict.incoming.clone(),
            edited_after: self.edited_after(conflict),
        })
    }

    /// Only [`ConflictDetail::edited_after`] of one unresolved conflict: no
    /// Git and no copies of the versions, so a window can follow a later save
    /// cheaply while the conflict is open.
    pub fn conflict_edited_after(&self, id: &str) -> Result<Option<u64>> {
        Ok(self.edited_after(self.unresolved_conflict(id)?))
    }

    fn unresolved_conflict(&self, id: &str) -> Result<&Conflict> {
        let conflict = self.state.conflicts.get(id).context("Conflict not found")?;
        ensure!(
            !self.state.manifest.resolutions.contains_key(id),
            "Conflict is already resolved"
        );
        Ok(conflict)
    }

    fn edited_after(&self, conflict: &Conflict) -> Option<u64> {
        (self.state.observed.get(&conflict.path) != conflict.accepted.as_ref()).then(|| {
            self.state
                .manifest
                .sessions
                .values()
                .filter(|session| session.device == self.config.device)
                .filter_map(|session| session.changes.get(&conflict.path))
                .map(|change| change.last_edit)
                .fold(conflict.created_at, u64::max)
        })
    }

    /// The last commit on `main` touching `path`, if its blob is `version`.
    /// History is a nicety, so a Git failure reads as "unknown".
    fn history_of(&self, path: &str, version: &Option<Vec<u8>>) -> Option<CommitInfo> {
        if self.state.main.is_empty() {
            return None;
        }
        let commit = self.git.last_commit(&self.state.main, path).ok()??;
        let matches = match (version, &commit.blob) {
            (None, None) => true,
            (Some(bytes), Some(blob)) => self.git.blob_id(bytes).is_ok_and(|id| &id == blob),
            _ => false,
        };
        matches.then_some(commit)
    }
}
