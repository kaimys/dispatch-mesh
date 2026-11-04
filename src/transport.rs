//! Device branches contain ordinary files plus a small versioned metadata index.
//! All referenced blobs are linked into that same commit's tree, so fetching and
//! Git garbage collection preserve every version needed for conflict recovery.
use crate::{
    codec::BlobRef,
    git::{Git, validate_path},
    model::*,
};
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeMap, BTreeSet};

pub fn encode(git: &Git, mut manifest: Manifest) -> Result<BTreeMap<String, String>> {
    manifest.version = 2;
    let mut cache = BTreeMap::<String, String>::new();
    let references = manifest.map_blobs(&mut |bytes: Vec<u8>| {
        let hash = digest(&bytes);
        let object = if let Some(object) = cache.get(&hash) {
            object.clone()
        } else {
            let object = git.object(&bytes)?;
            cache.insert(hash, object.clone());
            object
        };
        Ok(BlobRef { blob: object })
    })?;
    let mut entries = BTreeMap::new();
    let mut add = |path: String, reference: &Option<BlobRef>| -> Result<()> {
        validate_path(&path)?;
        if let Some(reference) = reference {
            entries.insert(path, reference.blob.clone());
        }
        Ok(())
    };
    for (id, session) in &references.sessions {
        for (path, change) in &session.changes {
            add(format!("drafts/{id}/base/{path}"), &change.base)?;
            add(format!("drafts/{id}/files/{path}"), &change.content)?;
        }
    }
    for (id, conflict) in &references.conflicts {
        add(format!("conflicts/{id}/base"), &conflict.base)?;
        add(format!("conflicts/{id}/accepted"), &conflict.accepted)?;
        add(format!("conflicts/{id}/incoming"), &conflict.incoming)?;
    }
    for (id, resolution) in &references.resolutions {
        add(format!("resolutions/{id}/selected"), &resolution.selected)?;
    }
    entries.insert(
        "state.json".into(),
        git.object(&serde_json::to_vec_pretty(&references)?)?,
    );
    Ok(entries)
}

pub fn decode(git: &Git, reference: &str, max_file_bytes: u64) -> Result<Manifest> {
    let entries = git.tree_entries(reference)?;
    let metadata = entries
        .get("state.json")
        .context("Device snapshot has no state.json index")?;
    let json = git.read_blob(metadata, 128 * 1024 * 1024)?;
    let value: serde_json::Value = serde_json::from_slice(&json)?;
    match value.get("version").and_then(serde_json::Value::as_u64) {
        Some(1) => {
            // Old device branches can still be received during a rolling upgrade.
            // The next publication by that device uses the new tree format.
            Ok(serde_json::from_value(value)?)
        }
        Some(2) => {
            let references: Manifest<BlobRef> = serde_json::from_value(value)?;
            let reachable: BTreeSet<_> = entries.values().collect();
            let mut cache = BTreeMap::<String, Vec<u8>>::new();
            let mut total = 0_u64;
            references.map_blobs(&mut |reference: BlobRef| {
                // Never resolve an arbitrary local object named by remote metadata.
                ensure!(
                    reachable.contains(&reference.blob) && &reference.blob != metadata,
                    "Snapshot references a blob outside its file tree"
                );
                if let Some(bytes) = cache.get(&reference.blob) {
                    total += bytes.len() as u64;
                    ensure!(
                        total <= 128 * 1024 * 1024,
                        "Device snapshot exceeds the preview memory limit"
                    );
                    return Ok(bytes.clone());
                }
                let bytes = git.read_blob(&reference.blob, max_file_bytes)?;
                total += bytes.len() as u64;
                ensure!(
                    total <= 128 * 1024 * 1024,
                    "Device snapshot exceeds the preview memory limit"
                );
                cache.insert(reference.blob, bytes.clone());
                Ok(bytes)
            })
        }
        _ => anyhow::bail!("Unsupported remote manifest format; update Mesh before syncing"),
    }
}
