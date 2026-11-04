use crate::{codec::BlobRef, model::*};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize)]
struct StoredState {
    format: u32,
    state: State<BlobRef>,
}

pub fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
        CREATE TABLE IF NOT EXISTS state (id INTEGER PRIMARY KEY CHECK(id=1), value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS blobs (id TEXT PRIMARY KEY, content BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS state_migrations (format INTEGER PRIMARY KEY, value TEXT NOT NULL);")?;
    Ok(())
}

pub fn load(db: &Connection) -> Result<State> {
    let json: String = db.query_row("SELECT value FROM state WHERE id=1", [], |row| row.get(0))?;
    let value: serde_json::Value = serde_json::from_str(&json)?;
    if value.get("format").is_none() {
        // Version-one workspaces remain readable; the next locked save performs
        // an atomic migration and retains the original JSON as a recovery backup.
        return Ok(serde_json::from_value(value)?);
    }
    let stored: StoredState = serde_json::from_value(value)?;
    ensure!(stored.format == 2, "Unsupported local state format");
    let mut cache = BTreeMap::<String, Vec<u8>>::new();
    stored.state.map_blobs(&mut |reference: BlobRef| {
        if let Some(bytes) = cache.get(&reference.blob) {
            return Ok(bytes.clone());
        }
        ensure!(
            reference.blob.len() == 64 && reference.blob.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid local blob reference"
        );
        let bytes: Vec<u8> = db
            .query_row(
                "SELECT content FROM blobs WHERE id=?1",
                [&reference.blob],
                |row| row.get(0),
            )
            .context("Referenced local snapshot blob is missing; recovery data was not modified")?;
        ensure!(
            digest(&bytes) == reference.blob,
            "Local snapshot blob failed its integrity check"
        );
        cache.insert(reference.blob, bytes.clone());
        Ok(bytes)
    })
}

pub fn save(db: &Connection, state: &State) -> Result<()> {
    let tx = db.unchecked_transaction()?;
    let references = state.clone().map_blobs(&mut |bytes: Vec<u8>| {
        let id = digest(&bytes);
        tx.execute(
            "INSERT OR IGNORE INTO blobs(id,content) VALUES(?1,?2)",
            params![id, bytes],
        )?;
        Ok(BlobRef { blob: id })
    })?;
    let json = serde_json::to_string(&StoredState {
        format: 2,
        state: references,
    })?;
    let previous: Option<String> = tx
        .query_row("SELECT value FROM state WHERE id=1", [], |row| row.get(0))
        .optional()?;
    if let Some(previous) = &previous
        && serde_json::from_str::<serde_json::Value>(previous)?
            .get("format")
            .is_none()
    {
        tx.execute(
            "INSERT OR IGNORE INTO state_migrations(format,value) VALUES(1,?1)",
            [previous],
        )?;
    }
    if previous.as_ref() != Some(&json) {
        tx.execute("INSERT INTO state(id,value) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET value=excluded.value", [json])?;
    }
    tx.commit()?;
    Ok(())
}
