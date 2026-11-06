//! Convert in-memory byte buffers to explicit, content-addressed references.
//! Both local persistence and Git transport use this typed conversion, so newly
//! added recovery fields cannot accidentally serialize file contents into JSON.
use crate::model::*;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BlobRef {
    pub blob: String,
}

fn values<A, B>(
    input: BTreeMap<String, A>,
    mut map: impl FnMut(A) -> Result<B>,
) -> Result<BTreeMap<String, B>> {
    input
        .into_iter()
        .map(|(key, value)| Ok((key, map(value)?)))
        .collect()
}

impl<A> Change<A> {
    pub fn map_blobs<B>(self, map: &mut impl FnMut(A) -> Result<B>) -> Result<Change<B>> {
        Ok(Change {
            base: self.base.map(&mut *map).transpose()?,
            content: self.content.map(&mut *map).transpose()?,
            intent: self.intent,
            last_edit: self.last_edit,
        })
    }
}

impl<A> Session<A> {
    pub fn map_blobs<B>(self, map: &mut impl FnMut(A) -> Result<B>) -> Result<Session<B>> {
        Ok(Session {
            id: self.id,
            device: self.device,
            author: self.author,
            created_at: self.created_at,
            last_edit: self.last_edit,
            revision: self.revision,
            closed: self.closed,
            published: self.published,
            discarded: self.discarded,
            changes: values(self.changes, |change| change.map_blobs(map))?,
            dependencies: self.dependencies,
            intents: self.intents,
            message: self.message,
            message_base: self.message_base,
            excluded: self.excluded,
        })
    }
}

impl<A> Conflict<A> {
    pub fn map_blobs<B>(self, map: &mut impl FnMut(A) -> Result<B>) -> Result<Conflict<B>> {
        Ok(Conflict {
            id: self.id,
            path: self.path,
            base: self.base.map(&mut *map).transpose()?,
            accepted: self.accepted.map(&mut *map).transpose()?,
            incoming: self.incoming.map(&mut *map).transpose()?,
            session: self.session,
            author: self.author,
            binary: self.binary,
            created_at: self.created_at,
            accepted_sessions: self.accepted_sessions,
        })
    }
}

impl<A> Resolution<A> {
    pub fn map_blobs<B>(self, map: &mut impl FnMut(A) -> Result<B>) -> Result<Resolution<B>> {
        Ok(Resolution {
            conflict: self.conflict,
            selected: self.selected.map(map).transpose()?,
            author: self.author,
            created_at: self.created_at,
        })
    }
}

impl<A> Manifest<A> {
    pub fn map_blobs<B>(self, map: &mut impl FnMut(A) -> Result<B>) -> Result<Manifest<B>> {
        Ok(Manifest {
            version: self.version,
            sessions: values(self.sessions, |session| session.map_blobs(map))?,
            resolutions: values(self.resolutions, |resolution| resolution.map_blobs(map))?,
            conflicts: values(self.conflicts, |conflict| conflict.map_blobs(map))?,
        })
    }
}

impl<A> State<A> {
    pub fn map_blobs<B>(self, map: &mut impl FnMut(A) -> Result<B>) -> Result<State<B>> {
        Ok(State {
            main: self.main,
            observed: values(self.observed, &mut *map)?,
            manifest: self.manifest.map_blobs(map)?,
            conflicts: values(self.conflicts, |conflict| conflict.map_blobs(map))?,
            intent: self.intent,
            applying: self
                .applying
                .map(|files| values(files, &mut *map))
                .transpose()?,
            attention: self.attention,
            remote_main: self.remote_main,
            pending_capture: self
                .pending_capture
                .map(|files| values(files, &mut *map))
                .transpose()?,
            apply_backups: self.apply_backups,
            uploaded: self.uploaded,
            offline: self.offline,
            summary_failure: self.summary_failure,
            last_sync: self.last_sync,
            saved_copies: self.saved_copies,
        })
    }
}
