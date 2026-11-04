use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub type Files = BTreeMap<String, Vec<u8>>;

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn digest(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value.as_ref()))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub version: u32,
    pub device: String,
    pub identity: Identity,
    pub remote: Option<String>,
    pub branch: String,
    pub idle_seconds: u64,
    pub max_file_bytes: u64,
    pub provider: Option<ProviderConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub base_url: String,
    pub model: String,
    /// Secrets stay in the process environment, never in workspace state.
    pub api_key_env: Option<String>,
    pub json_mode: bool,
    /// Explicit opt-in: otherwise summaries contain file metadata only.
    pub send_text: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub base: Option<Vec<u8>>,
    pub content: Option<Vec<u8>>,
    pub intent: Option<String>,
    pub last_edit: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Intent {
    pub id: String,
    pub purpose: String,
    pub paths: BTreeSet<String>,
    pub created_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub title: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub device: String,
    pub author: Identity,
    pub created_at: u64,
    pub last_edit: u64,
    pub revision: u64,
    pub closed: bool,
    pub published: bool,
    #[serde(default)]
    pub discarded: bool,
    pub changes: BTreeMap<String, Change>,
    pub dependencies: BTreeSet<String>,
    pub intents: BTreeMap<String, Intent>,
    pub message: Option<Message>,
    #[serde(default)]
    pub message_base: Option<String>,
}

impl Session {
    pub fn settled(&self) -> bool {
        self.published || self.discarded
    }

    pub fn snapshot_hash(&self) -> String {
        digest(serde_json::to_vec(&self.changes).expect("serializable changes"))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conflict {
    pub id: String,
    pub path: String,
    pub base: Option<Vec<u8>>,
    pub accepted: Option<Vec<u8>>,
    pub incoming: Option<Vec<u8>>,
    pub session: String,
    pub author: Identity,
    pub binary: bool,
    pub created_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Resolution {
    pub conflict: String,
    pub selected: Option<Vec<u8>>,
    pub author: Identity,
    pub created_at: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub sessions: BTreeMap<String, Session>,
    pub resolutions: BTreeMap<String, Resolution>,
    #[serde(default)]
    pub conflicts: BTreeMap<String, Conflict>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub main: String,
    /// Last view installed in the live folder; incoming changes never count as local edits.
    pub observed: Files,
    pub manifest: Manifest,
    pub conflicts: BTreeMap<String, Conflict>,
    pub intent: Option<Intent>,
    /// Write-ahead apply journal, persisted before touching the live folder.
    pub applying: Option<Files>,
    pub attention: Option<String>,
    #[serde(default)]
    pub remote_main: Option<String>,
    /// Saved bytes that cannot yet be attributed safely to an existing draft.
    #[serde(default)]
    pub pending_capture: Option<Files>,
    #[serde(default)]
    pub apply_backups: BTreeMap<String, String>,
}

pub fn binary(content: &Option<Vec<u8>>) -> bool {
    content
        .as_ref()
        .is_some_and(|bytes| bytes.contains(&0) || std::str::from_utf8(bytes).is_err())
}
