use crate::{
    git::{Git, validate_path},
    model::*,
    policy::{self, Policy},
    storage, transport,
};
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use rusqlite::Connection;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub struct Workspace {
    pub root: PathBuf,
    pub config: Config,
    pub state: State,
    pub git: Git,
    db: Connection,
    _lock: Option<File>,
}

#[derive(Debug)]
pub struct WorkspaceBusy;

impl std::fmt::Display for WorkspaceBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Another Mesh operation is using this workspace; retry shortly"
        )
    }
}

impl std::error::Error for WorkspaceBusy {}

/// How long a lock attempt keeps retrying before reporting `WorkspaceBusy`.
///
/// On Unix these are `flock` locks, which belong to the open file description.
/// When any thread of this process spawns a child (e.g. Git), the child holds a
/// copy of every descriptor until it execs, so a lock this process just released
/// can stay held for a moment. Without a short retry, an immediate re-lock fails
/// spuriously. Real contention from another operation lasts much longer and is
/// still reported.
const LOCK_SETTLE: std::time::Duration = std::time::Duration::from_millis(500);

fn operation_lock(meta: &Path, name: &str) -> Result<File> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(meta.join(name))?;
    let started = std::time::Instant::now();
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => return Ok(lock),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || [Some(33), Some(11)].contains(&error.raw_os_error()) =>
            {
                if started.elapsed() >= LOCK_SETTLE {
                    return Err(WorkspaceBusy.into());
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(error) => return Err(error).context("Cannot lock workspace storage"),
        }
    }
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    use std::io::Write;
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().context("Missing parent")?)?;
    temp.write_all(&serde_json::to_vec_pretty(value)?)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

impl Workspace {
    pub fn init(
        root: &Path,
        identity: Identity,
        remote: Option<String>,
        branch: String,
    ) -> Result<Self> {
        ensure!(
            !identity.name.trim().is_empty()
                && !identity.email.trim().is_empty()
                && !identity.name.contains(['\n', '\r'])
                && !identity.email.contains(['\n', '\r']),
            "A name and email are required"
        );
        ensure!(
            !branch.starts_with('-')
                && !branch.contains([' ', '\n', ':', '~', '^', '?', '*', '[', '\\'])
                && !branch.starts_with("mesh/"),
            "Invalid workspace branch"
        );
        if let Some(remote) = &remote {
            ensure!(
                !remote.starts_with('-') && !remote.contains(['\n', '\r']),
                "Invalid remote"
            );
            if !Path::new(remote).is_absolute()
                && let Ok(url) = reqwest::Url::parse(remote)
            {
                ensure!(
                    url.username().is_empty() && url.password().is_none() && url.query().is_none(),
                    "Do not embed credentials in a remote URL; use Git Credential Manager or SSH"
                );
                ensure!(
                    ["https", "ssh", "file"].contains(&url.scheme()),
                    "Use HTTPS, SSH, or a local Git remote"
                );
            }
        }
        fs::create_dir_all(root)?;
        let root = dunce::canonicalize(root)?;
        let meta = root.join(".mesh");
        fs::create_dir_all(&meta)?;
        ensure!(
            !meta.join("config.json").exists(),
            "This folder is already managed by Mesh"
        );
        let lock = operation_lock(&meta, "lock")?;
        let git = Git {
            dir: meta.join("objects.git"),
        };
        git.init()?;
        let config = Config {
            version: 1,
            device: Uuid::new_v4().to_string(),
            identity,
            remote,
            branch,
            idle_seconds: 300,
            max_file_bytes: 16 * 1024 * 1024,
            provider: None,
        };
        git.run(
            &["check-ref-format", &format!("refs/heads/{}", config.branch)],
            None,
        )?;
        if let Some(remote) = &config.remote {
            git.run(&["config", "remote.origin.url", remote], None)?;
        }
        let main = git.commit(
            &Files::new(),
            None,
            &config.identity,
            "Initialize Dispatch Mesh workspace",
        )?;
        git.run(&["update-ref", "refs/heads/mesh-history", &main], None)?;
        let db = Self::database(&meta)?;
        let mut workspace = Self {
            root,
            config,
            state: State {
                main,
                manifest: Manifest {
                    version: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
            git,
            db,
            _lock: Some(lock),
        };
        workspace.save()?;
        workspace.save_config()?;
        write_obsidian_excludes(&workspace.root)?;
        // Existing files become the first editing session, never an implicit overwrite.
        workspace.capture_at(now())?;
        Ok(workspace)
    }

    fn database(meta: &Path) -> Result<Connection> {
        let db = Connection::open(meta.join("state.sqlite"))?;
        storage::initialize(&db)?;
        Ok(db)
    }

    pub fn open(root: &Path) -> Result<Self> {
        let root = dunce::canonicalize(root).context("Workspace folder does not exist")?;
        let meta = root.join(".mesh");
        ensure!(
            !fs::symlink_metadata(&meta)?.file_type().is_symlink(),
            "Mesh storage must not be a symbolic link"
        );
        let lock = operation_lock(&meta, "lock")?;
        let config: Config = serde_json::from_slice(
            &fs::read(meta.join("config.json")).context("Run workspace init first")?,
        )?;
        ensure!(config.version == 1, "Unsupported workspace format");
        let db = Self::database(&meta)?;
        let state = storage::load(&db)?;
        let git = Git {
            dir: meta.join("objects.git"),
        };
        let mut workspace = Self {
            root,
            config,
            state,
            git,
            db,
            _lock: Some(lock),
        };
        workspace.recover_apply()?;
        // A no-op save also atomically upgrades a version-one local snapshot.
        workspace.save()?;
        Ok(workspace)
    }

    /// Read the last committed SQLite snapshot without competing with the writer.
    pub fn inspect(root: &Path) -> Result<Self> {
        let root = dunce::canonicalize(root).context("Workspace folder does not exist")?;
        let meta = root.join(".mesh");
        let config: Config = serde_json::from_slice(&fs::read(meta.join("config.json"))?)?;
        ensure!(config.version == 1, "Unsupported workspace format");
        let db = Connection::open_with_flags(
            meta.join("state.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let state = storage::load(&db)?;
        Ok(Self {
            git: Git {
                dir: meta.join("objects.git"),
            },
            root,
            config,
            state,
            db,
            _lock: None,
        })
    }

    /// The caller must also hold network.lock. File capture can proceed while Git
    /// waits on the remote. Reload afterward so no concurrently saved state is lost.
    fn network_call<T>(&mut self, operation: impl FnOnce(&Git) -> Result<T>) -> Result<T> {
        self.save()?;
        drop(self._lock.take());
        let result = operation(&self.git);
        let lock = match self.acquire_lock(std::time::Duration::from_secs(5)) {
            Ok(lock) => lock,
            // Never let lock contention hide why the network operation failed:
            // callers record that cause (sync_and_record), and re-acquire first.
            Err(_) if result.is_err() => return result,
            Err(error) => return Err(error),
        };
        self._lock = Some(lock);
        self.state = storage::load(&self.db)?;
        self.config = serde_json::from_slice(&fs::read(self.root.join(".mesh/config.json"))?)?;
        self.recover_apply()?;
        result
    }

    pub fn save_config(&self) -> Result<()> {
        ensure!(
            self._lock.is_some(),
            "Cannot modify configuration without the workspace lock"
        );
        write_json(&self.root.join(".mesh/config.json"), &self.config)
    }

    pub fn save(&self) -> Result<()> {
        ensure!(
            self._lock.is_some(),
            "Cannot save workspace state without its operation lock"
        );
        storage::save(&self.db, &self.state)
    }

    pub fn scan(&self) -> Result<Files> {
        let mut result = Files::new();
        let mut folded = BTreeSet::new();
        let overrides = ignore::overrides::OverrideBuilder::new(&self.root)
            .add("!.mesh/**")?
            .add("!.git/**")?
            .add("!.env")?
            .add("!.env.*")?
            .add("!*.pem")?
            .add("!*.key")?
            .build()?;
        let mut visited = Vec::new();
        for entry in ignore::WalkBuilder::new(&self.root)
            .hidden(false)
            .follow_links(false)
            .require_git(false)
            .git_ignore(true)
            .git_global(false)
            .git_exclude(false)
            .add_custom_ignore_filename(".meshignore")
            .overrides(overrides)
            .build()
        {
            let entry = entry.context("Cannot inspect workspace; snapshot was not advanced")?;
            if entry.depth() == 0 || entry.file_type().is_some_and(|kind| kind.is_dir()) {
                visited.push(entry.into_path());
                continue;
            }
            let path = self.relative(entry.path())?;
            let bytes = self.read_supported(entry.path(), &path, &mut folded)?;
            result.insert(path, bytes);
        }
        // Rule files always belong to the tree, even when a rule matches them
        // (ADR-0013): the walker applies them, so the shared policy must hold them.
        for directory in visited {
            for name in policy::RULE_FILES {
                let file = directory.join(name);
                let path = self.relative(&file)?;
                if result.contains_key(&path)
                    || policy::fixed_exclude(&path)
                    || fs::symlink_metadata(&file).is_err()
                {
                    continue;
                }
                let bytes = self.read_supported(&file, &path, &mut folded)?;
                result.insert(path, bytes);
            }
        }
        // A synchronized path that the rules now exclude simply leaves the tree
        // (ADR-0013). One that was skipped for any other reason is refused, so a
        // walker/policy disagreement can never turn into a recorded deletion.
        let policy = Policy::from_files(&self.root, &result)?;
        for path in self.state.observed.keys() {
            ensure!(
                result.contains_key(path) || policy.ignores(path) || !self.root.join(path).exists(),
                "A previously synchronized path is now unsupported: {path}. Restore the file before syncing; no deletion was recorded"
            );
        }
        Ok(result)
    }

    fn relative(&self, file: &Path) -> Result<String> {
        Ok(file
            .strip_prefix(&self.root)?
            .to_str()
            .context("Non-UTF8 filename")?
            .replace('\\', "/"))
    }

    /// Reads one workspace file, refusing what this preview doesn't support
    /// without following a link or reparse point.
    fn read_supported(
        &self,
        file: &Path,
        path: &str,
        folded: &mut BTreeSet<String>,
    ) -> Result<Vec<u8>> {
        ensure_regular_file(file)?;
        validate_path(path)?;
        ensure!(
            folded.insert(path.to_lowercase()),
            "Case-colliding paths are unsupported"
        );
        let before = fs::metadata(file)?;
        ensure!(
            before.is_file() && before.len() <= self.config.max_file_bytes,
            "File exceeds the configured size limit or is not a regular file: {path}"
        );
        let bytes = fs::read(file)
            .with_context(|| format!("Cannot read {path}; previous snapshot retained"))?;
        let after = fs::metadata(file)?;
        ensure!(
            before.len() == after.len()
                && before.modified()? == after.modified()?
                && bytes.len() as u64 == after.len(),
            "File changed while being read; retrying on the next scan"
        );
        Ok(bytes)
    }

    pub fn active_session(&self) -> Option<&Session> {
        self.state
            .manifest
            .sessions
            .values()
            .find(|s| s.device == self.config.device && !s.closed && !s.settled())
    }

    pub fn capture_at(&mut self, timestamp: u64) -> Result<bool> {
        let files = self.scan()?;
        self.capture(files, timestamp)
    }

    pub fn capture(&mut self, files: Files, timestamp: u64) -> Result<bool> {
        ensure!(
            self.state.applying.is_none(),
            "Pending incoming changes must be recovered before capture"
        );
        let paths: BTreeSet<_> = files
            .keys()
            .chain(self.state.observed.keys())
            .cloned()
            .collect();
        let changed: Vec<_> = paths
            .into_iter()
            .filter(|p| files.get(p) != self.state.observed.get(p))
            .collect();
        if changed.is_empty() {
            return Ok(false);
        }
        self.close_idle(timestamp)?;
        let id = self
            .active_session()
            .map(|s| s.id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let dependencies = self
            .state
            .manifest
            .sessions
            .values()
            .filter(|s| s.id != id && !s.settled())
            .map(|s| s.id.clone())
            .collect();
        let mut session = self
            .state
            .manifest
            .sessions
            .get(&id)
            .cloned()
            .unwrap_or_else(|| Session {
                id: id.clone(),
                device: self.config.device.clone(),
                author: self.config.identity.clone(),
                created_at: timestamp,
                last_edit: timestamp,
                revision: 0,
                closed: false,
                published: false,
                discarded: false,
                changes: BTreeMap::new(),
                dependencies,
                intents: BTreeMap::new(),
                message: None,
                message_base: None,
                excluded: BTreeSet::new(),
            });
        session.revision += 1;
        session.last_edit = timestamp;
        let canonical = self.git.files(&self.state.main)?;
        let policy = Policy::from_files(&self.root, &files)?;
        for path in changed {
            let intent = self
                .state
                .intent
                .as_ref()
                .filter(|intent| intent.paths.contains(&path));
            if let Some(intent) = intent {
                session.intents.insert(intent.id.clone(), intent.clone());
            }
            let edited = files.get(&path).cloned();
            let observed = self.state.observed.get(&path).cloned();
            // A synchronized file the rules now exclude: an explicit removal that
            // stays on disk, never a merge against this session's pending edit.
            let excluded = edited.is_none() && observed.is_some() && policy.ignores(&path);
            if !excluded
                && !session.changes.contains_key(&path)
                && observed.as_ref() != canonical.get(&path)
            {
                let contributors: Vec<_> = self
                    .state
                    .manifest
                    .sessions
                    .values()
                    .filter(|s| s.id != id && !s.settled() && s.changes.contains_key(&path))
                    .map(|s| s.id.clone())
                    .collect();
                for contributor in contributors {
                    if self.depends_on(&contributor, &id) {
                        self.state.pending_capture = Some(files.clone());
                        self.state.attention = Some("Dependent edits are saved locally. Finish the current session to avoid a circular draft dependency.".into());
                        self.save()?;
                        bail!(
                            "Dependent edits preserved; finish the current session before continuing synchronization"
                        );
                    }
                    session.dependencies.insert(contributor);
                }
            }
            let updated = if excluded {
                None
            } else if let Some(existing) = session.changes.get(&path) {
                // Apply only the new local delta to this author's own draft. Otherwise
                // received peer text would be copied into the local author's commit.
                match self.git.merge(&existing.content, &observed, &edited)? {
                    Some(content) => content,
                    None => {
                        self.state.pending_capture = Some(files.clone());
                        self.state.attention = Some("Edits depend on a received draft and cannot be attributed safely. Bytes are saved locally. Use session finish to checkpoint the current draft; the dependent edits will enter the next session.".into());
                        self.save()?;
                        bail!(
                            "Dependent edits preserved; finish the current session before continuing synchronization"
                        );
                    }
                }
            } else {
                edited
            };
            let change = session
                .changes
                .entry(path.clone())
                .or_insert_with(|| Change {
                    base: observed,
                    content: None,
                    intent: intent.map(|i| i.id.clone()),
                    last_edit: timestamp,
                });
            change.content = updated;
            change.last_edit = timestamp;
            // Mixed edits must not inherit a previous agent's purpose.
            if change.intent.as_deref() != intent.map(|i| i.id.as_str()) {
                change.intent = None;
            }
            if change.base == change.content {
                session.changes.remove(&path);
            }
        }
        self.state.manifest.sessions.insert(id, session);
        self.state.pending_capture = None;
        self.state.observed = files;
        self.save()?;
        Ok(true)
    }

    fn depends_on(&self, session: &str, target: &str) -> bool {
        let mut pending = vec![session.to_string()];
        let mut visited = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if id == target {
                return true;
            }
            if visited.insert(id.clone())
                && let Some(session) = self.state.manifest.sessions.get(&id)
            {
                pending.extend(session.dependencies.iter().cloned());
            }
        }
        false
    }

    pub fn close_idle(&mut self, timestamp: u64) -> Result<()> {
        for session in self
            .state
            .manifest
            .sessions
            .values_mut()
            .filter(|s| s.device == self.config.device && !s.closed)
        {
            if timestamp.saturating_sub(session.last_edit) >= self.config.idle_seconds {
                session.closed = true;
                session.discarded = session.changes.is_empty();
            }
        }
        self.save()
    }

    pub fn finish(&mut self) -> Result<Option<String>> {
        if self.state.pending_capture.is_some() {
            let id = self.active_session().map(|s| s.id.clone());
            if let Some(id) = &id {
                let session = self.state.manifest.sessions.get_mut(id).unwrap();
                session.closed = true;
                session.discarded = session.changes.is_empty();
            }
            self.state.intent = None;
            self.save()?;
            self.capture_at(now())?;
            return Ok(id);
        }
        self.capture_at(now())?;
        let id = self.active_session().map(|s| s.id.clone());
        if let Some(id) = &id {
            let session = self.state.manifest.sessions.get_mut(id).unwrap();
            session.closed = true;
            session.discarded = session.changes.is_empty();
        }
        self.state.intent = None;
        self.save()?;
        Ok(id)
    }

    pub fn register_intent(&mut self, purpose: String, paths: BTreeSet<String>) -> Result<String> {
        ensure!(
            !purpose.trim().is_empty() && purpose.len() <= 8192,
            "Purpose must contain 1–8192 bytes"
        );
        ensure!(
            !paths.is_empty(),
            "Specify exact relative paths for the intended edits"
        );
        for path in &paths {
            validate_path(path)?;
        }
        ensure!(
            self.state.intent.is_none(),
            "Another intent scope is active; end it before registering new intent"
        );
        self.capture_at(now())?;
        let id = Uuid::new_v4().to_string();
        self.state.intent = Some(Intent {
            id: id.clone(),
            purpose,
            paths,
            created_at: now(),
        });
        self.save()?;
        Ok(id)
    }

    pub fn end_intent(&mut self, id: &str) -> Result<()> {
        ensure!(
            self.state.intent.as_ref().is_some_and(|i| i.id == id),
            "Intent is not active on this device"
        );
        self.capture_at(now())?;
        self.state.intent = None;
        self.save()
    }

    pub fn sync(&mut self) -> Result<()> {
        let _network_lock = operation_lock(&self.root.join(".mesh"), "network.lock")?;
        self.capture_at(now())?;
        self.close_idle(now())?;
        if self.config.remote.is_some() {
            self.fetch()?;
            self.push_manifest()?;
        }
        self.reconcile_published()?;
        self.render()?;
        self.publish_ready()?;
        if self.config.remote.is_some() {
            self.push_manifest()?;
        }
        self.state.attention = None;
        self.state.offline = None;
        self.save()
    }

    fn fetch(&mut self) -> Result<()> {
        self.network_call(|git| {
            git.run(
                &[
                    "fetch",
                    "--prune",
                    "origin",
                    "+refs/heads/*:refs/remotes/origin/*",
                ],
                None,
            )
        })?;
        let remote_ref = format!("refs/remotes/origin/{}", self.config.branch);
        if let Ok(next) = self.git.text(&["rev-parse", "--verify", &remote_ref]) {
            if let Some(previous) = &self.state.remote_main {
                self.git
                    .run(&["merge-base", "--is-ancestor", previous, &next], None)
                    .context("Shared history was rewritten outside Mesh; synchronization paused")?;
            }
            self.state.main = next.clone();
            self.state.remote_main = Some(next);
        } else {
            ensure!(
                self.state.remote_main.is_none(),
                "The shared branch was deleted outside Mesh; synchronization paused"
            );
        }
        let refs = self.git.text(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/remotes/origin/mesh/devices/",
        ])?;
        for reference in refs.lines().filter(|r| !r.is_empty()) {
            let device = reference.rsplit('/').next().context("Invalid device ref")?;
            if device == self.config.device {
                continue;
            }
            let manifest = transport::decode(&self.git, reference, self.config.max_file_bytes)?;
            for (id, session) in manifest.sessions {
                ensure!(
                    id == session.id && session.device == device && Uuid::parse_str(&id).is_ok(),
                    "Invalid remote session identity"
                );
                for (path, change) in &session.changes {
                    validate_path(path)?;
                    ensure!(
                        change
                            .content
                            .as_ref()
                            .is_none_or(|v| v.len() as u64 <= self.config.max_file_bytes),
                        "Remote file exceeds size limit"
                    );
                }
                self.state.manifest.sessions.insert(id, session);
            }
            for (id, resolution) in manifest.resolutions {
                ensure!(id == resolution.conflict, "Invalid conflict resolution");
                // Deterministic tie-break for concurrent resolutions; retain each device manifest.
                let replace = self.state.manifest.resolutions.get(&id).is_none_or(|old| {
                    (
                        resolution.created_at,
                        digest(serde_json::to_vec(&resolution).unwrap()),
                    ) < (old.created_at, digest(serde_json::to_vec(old).unwrap()))
                });
                if replace {
                    self.state.manifest.resolutions.insert(id, resolution);
                }
            }
            for (id, conflict) in manifest.conflicts {
                validate_path(&conflict.path)?;
                ensure!(
                    id == conflict.id
                        && id
                            == digest(serde_json::to_vec(&(
                                &conflict.path,
                                &conflict.base,
                                &conflict.accepted,
                                &conflict.incoming,
                                &conflict.session
                            ))?),
                    "Invalid remote conflict record"
                );
                self.state.conflicts.entry(id).or_insert(conflict);
            }
        }
        self.save()
    }

    fn push_manifest(&mut self) -> Result<()> {
        // Revisions contained in this manifest; recorded only once the device
        // branch on the remote is known to hold them.
        let pushed: BTreeMap<String, u64> = self
            .state
            .manifest
            .sessions
            .values()
            .filter(|s| s.device == self.config.device)
            .map(|s| (s.id.clone(), s.revision))
            .collect();
        let manifest = Manifest {
            version: 2,
            sessions: self
                .state
                .manifest
                .sessions
                .iter()
                .filter(|(_, s)| s.device == self.config.device)
                .map(|(id, s)| (id.clone(), s.clone()))
                .collect(),
            resolutions: self.state.manifest.resolutions.clone(),
            conflicts: self.state.conflicts.clone(),
        };
        let entries = transport::encode(&self.git, manifest)?;
        let reference = format!("refs/heads/mesh/devices/{}", self.config.device);
        let previous = self.git.text(&["rev-parse", "--verify", &reference]).ok();
        // Retry a prior failed push even when the manifest has not changed.
        let commit = if previous
            .as_ref()
            .is_some_and(|p| self.git.tree_entries(p).is_ok_and(|old| old == entries))
        {
            previous.clone().unwrap()
        } else {
            self.git.commit_objects(
                &entries,
                previous.as_deref(),
                &self.config.identity,
                "Update recoverable Mesh drafts",
            )?
        };
        self.git.run(&["update-ref", &reference, &commit], None)?;
        let remote_ref = format!("refs/remotes/origin/mesh/devices/{}", self.config.device);
        if self
            .git
            .text(&["rev-parse", "--verify", &remote_ref])
            .ok()
            .as_ref()
            == Some(&commit)
        {
            self.record_uploaded(pushed);
            return Ok(());
        }
        self.network_call(|git| {
            git.run(
                &["push", "origin", &format!("{reference}:{reference}")],
                None,
            )
        })?;
        self.git.run(&["update-ref", &remote_ref, &commit], None)?;
        // network_call reloaded state from disk, so record after it returns.
        self.record_uploaded(pushed);
        Ok(())
    }

    /// Remember which own revisions reached the remote device branch. This is
    /// local-only state: it is never encoded into the manifest, so recording an
    /// upload never changes what the next push would send.
    fn record_uploaded(&mut self, pushed: BTreeMap<String, u64>) {
        let sessions = &self.state.manifest.sessions;
        self.state.uploaded = pushed
            .into_iter()
            .filter(|(id, _)| sessions.get(id).is_some_and(|s| !s.settled()))
            .collect();
    }

    /// Open, sync, and on failure persist its cause for `status`. Shared by the
    /// CLI `sync` command and the background runner.
    pub fn sync_and_record(root: &Path) -> Result<()> {
        let mut workspace = Workspace::open(root)?;
        let result = workspace.sync();
        if let Err(error) = &result
            && !error.is::<WorkspaceBusy>()
            && let Err(unrecorded) = workspace.persist_sync_failure(error)
        {
            return result.map_err(|error| {
                error.context(format!("The failure could not be recorded: {unrecorded:#}"))
            });
        }
        result
    }

    /// Persist a sync failure while owning the operation lock. The workspace
    /// normally still holds it; if a network step lost it to a concurrent
    /// capture, wait for it instead of dropping the outcome. Whatever the
    /// failed sync left half-applied in memory is discarded: the cause is
    /// recorded against durable state, as a fresh open would see it.
    fn persist_sync_failure(&mut self, error: &anyhow::Error) -> Result<()> {
        if self._lock.is_none() {
            self._lock = Some(self.acquire_lock(std::time::Duration::from_secs(30))?);
        }
        self.state = storage::load(&self.db)?;
        self.record_sync_failure(error);
        self.save()
    }

    /// Take the operation lock, retrying while another operation holds it.
    fn acquire_lock(&self, timeout: std::time::Duration) -> Result<File> {
        let started = std::time::Instant::now();
        loop {
            match operation_lock(&self.root.join(".mesh"), "lock") {
                Ok(lock) => return Ok(lock),
                Err(error) if error.is::<WorkspaceBusy>() && started.elapsed() < timeout => {
                    std::thread::sleep(std::time::Duration::from_millis(25))
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Record why a sync failed. An unreachable remote is `offline`; every other
    /// failure, including unclassified ones, needs the user's attention (ADR-0010).
    pub fn record_sync_failure(&mut self, error: &anyhow::Error) {
        const SYNC_FAILURE: &str = "Synchronization waiting: ";
        let message = format!("{error:#}");
        if crate::git::failure_kind(error) == Some(crate::git::GitFailureKind::Offline) {
            // Replace a previous sync failure, but keep engine-raised attention.
            if self
                .state
                .attention
                .as_deref()
                .is_some_and(|text| text.starts_with(SYNC_FAILURE))
            {
                self.state.attention = None;
            }
            self.state.offline = Some(message);
        } else {
            self.state.offline = None;
            self.state.attention = Some(format!("{SYNC_FAILURE}{message}"));
        }
    }

    fn reconcile_published(&mut self) -> Result<()> {
        // Only our final two trailers carry recovery metadata; do not interpret
        // arbitrary message text or AI output as session publication metadata.
        let history = self
            .git
            .text(&["log", "--format=%B%x00", &self.state.main])?;
        for session in self.state.manifest.sessions.values_mut() {
            if history.split('\0').any(|message| {
                let lines: Vec<_> = message.trim().lines().collect();
                lines.len() >= 2
                    && lines[lines.len() - 2] == format!("Mesh-Session: {}", session.id)
                    && lines[lines.len() - 1]
                        == format!("Mesh-Snapshot: {}", session.snapshot_hash())
            }) {
                session.published = true;
            }
        }
        Ok(())
    }

    pub fn render(&mut self) -> Result<()> {
        let mut files = self.git.files(&self.state.main)?;
        let mut sessions: Vec<_> = self
            .state
            .manifest
            .sessions
            .values()
            .filter(|s| !s.settled())
            .cloned()
            .collect();
        sessions.sort_by_key(|s| (s.created_at, s.id.clone()));
        let mut pending = sessions;
        let mut applied = BTreeSet::new();
        let mut sources = BTreeMap::<String, BTreeMap<String, String>>::new();
        while !pending.is_empty() {
            let index = pending
                .iter()
                .position(|s| {
                    s.dependencies.iter().all(|dep| {
                        applied.contains(dep)
                            || self
                                .state
                                .manifest
                                .sessions
                                .get(dep)
                                .is_some_and(|s| s.settled())
                    })
                })
                .context("Draft dependencies are missing or cyclic; local work retained")?;
            let session = pending.remove(index);
            let mut policy = None;
            for (path, change) in rules_first(&session) {
                if excluded_by(&mut policy, &self.root, &files, path)? {
                    // The removal wins over any edit to an excluded path (ADR-0013).
                    files.remove(path);
                    continue;
                }
                let accepted = files.get(path).cloned();
                let discarded = self
                    .matching_resolution(path, change, &session)
                    .is_some_and(|(_, _, keep)| !keep);
                let resolved =
                    self.merge_change(path, &accepted, change, &session, sources.get(path))?;
                if !discarded
                    && change.base != change.content
                    && (resolved != accepted || resolved == change.content)
                {
                    sources
                        .entry(path.clone())
                        .or_default()
                        .insert(session.id.clone(), change_fingerprint(change));
                }
                set_file(&mut files, path, resolved);
            }
            applied.insert(session.id);
        }
        let policy = Policy::from_files(&self.root, &files)?;
        files.retain(|path, _| !policy.ignores(path));
        self.settle_excluded_conflicts(&policy)?;
        self.install(files)
    }

    /// Recording that a path stopped synchronizing settles an open conflict on it,
    /// as a traceable resolution selecting "removed" (ADR-0003). Both inputs stay
    /// in the conflict record; the local file is never touched.
    fn settle_excluded_conflicts(&mut self, policy: &Policy) -> Result<()> {
        let ids: Vec<_> = self
            .unresolved()
            .filter(|conflict| policy.ignores(&conflict.path))
            .map(|conflict| conflict.id.clone())
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        for id in ids {
            self.state.manifest.resolutions.insert(
                id.clone(),
                Resolution {
                    conflict: id,
                    selected: None,
                    author: self.config.identity.clone(),
                    created_at: now(),
                },
            );
        }
        self.save()
    }

    fn merge_change(
        &mut self,
        path: &str,
        accepted: &Option<Vec<u8>>,
        change: &Change,
        session: &Session,
        sources: Option<&BTreeMap<String, String>>,
    ) -> Result<Option<Vec<u8>>> {
        let id = digest(serde_json::to_vec(&(
            path,
            &change.base,
            accepted,
            &change.content,
            &session.id,
        ))?);
        if let Some(sources) = sources
            && let Some(record) = self.state.conflicts.get_mut(&id)
        {
            record.accepted_sessions.extend(sources.clone());
        }
        let choice = self.matching_resolution(path, change, session);
        if choice.is_some_and(|(_, _, keep)| !keep) {
            // The user rejected this exact input revision. Applying it cleanly
            // against an older main tree must not resurrect that rejected input.
            return Ok(accepted.clone());
        }
        if let Some(merged) = self.git.merge(accepted, &change.base, &change.content)? {
            return Ok(merged);
        }
        if let Some((conflict, resolution, true)) = choice
            && [
                &conflict.base,
                &conflict.accepted,
                &conflict.incoming,
                &resolution.selected,
            ]
            .contains(&accepted)
        {
            // A different publication order may put either original input on
            // main first. The recorded choice still governs those same inputs.
            return Ok(resolution.selected.clone());
        }
        if let Some(resolution) = self.state.manifest.resolutions.get(&id) {
            return Ok(resolution.selected.clone());
        }
        let accepted_sessions = sources.cloned().unwrap_or_else(|| {
            self.state
                .manifest
                .sessions
                .values()
                .filter(|candidate| candidate.id != session.id && !candidate.settled())
                .filter_map(|candidate| {
                    candidate
                        .changes
                        .get(path)
                        .filter(|change| &change.content == accepted)
                        .map(|change| (candidate.id.clone(), change_fingerprint(change)))
                })
                .collect()
        });
        let record = self
            .state
            .conflicts
            .entry(id.clone())
            .or_insert_with(|| Conflict {
                id,
                path: path.to_string(),
                base: change.base.clone(),
                accepted: accepted.clone(),
                incoming: change.content.clone(),
                session: session.id.clone(),
                author: session.author.clone(),
                binary: binary(accepted) || binary(&change.content),
                created_at: now(),
                accepted_sessions: accepted_sessions.clone(),
            });
        record.accepted_sessions.extend(accepted_sessions);
        Ok(accepted.clone())
    }

    fn matching_resolution(
        &self,
        path: &str,
        change: &Change,
        session: &Session,
    ) -> Option<(&Conflict, &Resolution, bool)> {
        let fingerprint = change_fingerprint(change);
        self.state.conflicts.values().filter(|conflict| conflict.path == path).filter_map(|conflict| {
            let resolution = self.state.manifest.resolutions.get(&conflict.id)?;
            let incoming = conflict.session == session.id && conflict.base == change.base && conflict.incoming == change.content;
            let accepted = conflict.accepted_sessions.get(&session.id).is_some_and(|version| version == &fingerprint)
                // Compatibility for records made before contributor revisions
                // were stored: infer only an exact original base/version match.
                || (conflict.accepted_sessions.is_empty() && session.created_at <= conflict.created_at && conflict.session != session.id && conflict.base == change.base && conflict.accepted == change.content);
            if incoming { Some((conflict, resolution, resolution.selected == conflict.incoming)) }
            else if accepted { Some((conflict, resolution, resolution.selected == conflict.accepted)) }
            else { None }
        }).max_by(|(left, lr, _), (right, rr, _)| (lr.created_at, left.created_at, &left.id).cmp(&(rr.created_at, right.created_at, &right.id)))
    }

    fn install(&mut self, desired: Files) -> Result<()> {
        let mut folded = BTreeSet::new();
        for path in desired.keys() {
            ensure!(
                folded.insert(path.to_lowercase()),
                "Incoming files have case-colliding paths"
            );
        }
        if desired == self.state.observed {
            return Ok(());
        }
        // Capture saves that arrived during fetch/merge instead of overwriting them.
        if self.scan()? != self.state.observed {
            self.capture_at(now())?;
            bail!(
                "New local edits arrived during synchronization; captured safely, retrying next cycle"
            );
        }
        self.state.applying = Some(desired);
        self.save()?;
        self.recover_apply()
    }

    fn recover_apply(&mut self) -> Result<()> {
        let Some(desired) = self.state.applying.clone() else {
            return Ok(());
        };
        // Decided from the journal itself, so a replay after a crash leaves the same
        // files alone. A path the incoming rules exclude is neither written nor
        // removed, and leaves the observed view (ADR-0013).
        let policy = Policy::from_files(&self.root, &desired)?;
        let desired: Files = desired
            .into_iter()
            .filter(|(path, _)| !policy.ignores(path))
            .collect();
        let paths: BTreeSet<_> = desired
            .keys()
            .chain(self.state.observed.keys())
            .filter(|path| !policy.ignores(path))
            .cloned()
            .collect();
        let mut changed_by_user = false;
        let mut actual_after = desired.clone();
        let mut capture_baseline = desired.clone();
        for path in paths {
            let before = self.state.observed.get(&path);
            let after = desired.get(&path);
            if before == after {
                continue;
            }
            self.safe_target(&path)?;
            let target = self.root.join(&path);
            let current = match fs::read(&target) {
                Ok(bytes) => Some(bytes),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            };
            if current.as_ref() == after {
                continue;
            }
            let already_moved = self.state.apply_backups.get(&path).is_some_and(|backup| {
                self.root
                    .join(".mesh/recovery")
                    .join(backup)
                    .join("content")
                    .exists()
            });
            if current.as_ref() != before && !(current.is_none() && already_moved) {
                // Preserve a save made after the journal was written. Capture against the
                // pre-apply baseline before rendering again on the next cycle.
                set_file(&mut actual_after, &path, current.clone());
                set_file(&mut capture_baseline, &path, before.cloned());
                changed_by_user = true;
                continue;
            }
            // Preserve the actual displaced file, including a save racing the precheck.
            // A non-clobbering install then refuses to overwrite a newly created save.
            if current.is_some() {
                let backup_id = Uuid::new_v4().to_string();
                let recovery = self.root.join(".mesh/recovery").join(&backup_id);
                fs::create_dir_all(&recovery)?;
                write_json(
                    &recovery.join("record.json"),
                    &serde_json::json!({"path": path, "created_at": now()}),
                )?;
                let backup = recovery.join("content");
                self.state.apply_backups.insert(path.clone(), backup_id);
                self.save()?;
                fs::rename(&target, &backup)?;
                if fs::read(&backup)?.as_slice() != before.unwrap().as_slice() {
                    let bytes = fs::read(&backup)?;
                    let mut restored = tempfile::NamedTempFile::new_in(target.parent().unwrap())?;
                    use std::io::Write;
                    restored.write_all(&bytes)?;
                    let _ = restored.persist_noclobber(&target);
                    bail!(
                        "A save raced incoming changes; the displaced version is preserved in .mesh/recovery"
                    );
                }
            }
            if let Some(bytes) = after {
                let parent = target.parent().context("Missing parent directory")?;
                fs::create_dir_all(parent)?;
                self.safe_target(&path)?;
                use std::io::Write;
                let mut file = tempfile::NamedTempFile::new_in(parent)?;
                file.write_all(bytes)?;
                file.as_file().sync_all()?;
                file.persist_noclobber(&target).map_err(|e| e.error)?;
            }
        }
        self.state.applying = None;
        self.state.apply_backups.clear();
        self.state.observed = capture_baseline;
        self.save()?;
        if changed_by_user
            && let Err(error) = self.capture(actual_after, now())
            && self.state.pending_capture.is_none()
        {
            return Err(error);
        }
        Ok(())
    }

    fn safe_target(&self, path: &str) -> Result<()> {
        validate_path(path)?;
        let mut current = self.root.clone();
        for component in Path::new(path).components() {
            current.push(component);
            if let Ok(meta) = fs::symlink_metadata(&current) {
                ensure!(
                    !meta.file_type().is_symlink(),
                    "Refusing to write through a symbolic link"
                );
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    ensure!(
                        meta.file_attributes() & 0x400 == 0,
                        "Refusing to write through a reparse point"
                    );
                }
            }
        }
        Ok(())
    }

    pub fn publish_ready(&mut self) -> Result<()> {
        let ids: Vec<_> = self
            .state
            .manifest
            .sessions
            .values()
            .filter(|s| {
                s.device == self.config.device && s.closed && !s.settled() && s.message.is_some()
            })
            .map(|s| s.id.clone())
            .collect();
        for id in ids {
            let session = self.state.manifest.sessions[&id].clone();
            if !session.dependencies.iter().all(|dep| {
                self.state
                    .manifest
                    .sessions
                    .get(dep)
                    .is_some_and(|s| s.settled())
            }) {
                continue;
            }
            if self
                .unresolved()
                .any(|c| c.session == id || session.changes.contains_key(&c.path))
            {
                continue;
            }
            if session.message_base.as_deref() != Some(&self.state.main) {
                let session = self.state.manifest.sessions.get_mut(&id).unwrap();
                session.message = None;
                session.message_base = None;
                self.save()?;
                continue;
            }
            let main = self.git.files(&self.state.main)?;
            let mut files = main.clone();
            let conflicts_before = self.state.conflicts.len();
            let mut policy = None;
            for (path, change) in rules_first(&session) {
                if excluded_by(&mut policy, &self.root, &files, path)? {
                    files.remove(path);
                    continue;
                }
                let accepted = files.get(path).cloned();
                let merged = self.merge_change(path, &accepted, change, &session, None)?;
                set_file(&mut files, path, merged);
            }
            // The shared tree never holds a path its own rules exclude (ADR-0013).
            let policy = Policy::from_files(&self.root, &files)?;
            files.retain(|path, _| !policy.ignores(path));
            if self.state.conflicts.len() != conflicts_before {
                self.save()?;
                continue;
            }
            if files == main {
                self.discard_empty_projection(&id)?;
                continue;
            }
            let message = session.message.as_ref().unwrap();
            let text = format!(
                "{}\n\n{}\n\nMesh-Session: {}\nMesh-Snapshot: {}\n",
                message.title,
                message.description,
                id,
                session.snapshot_hash()
            );
            let commit = self
                .git
                .commit(&files, Some(&self.state.main), &session.author, &text)?;
            if self.config.remote.is_some() {
                // Ordinary fast-forward push detects a remote publication race. A retry
                // fetches main and recomputes the merge; shared history is never amended.
                let refspec = format!("{commit}:refs/heads/{}", self.config.branch);
                self.network_call(|git| git.run(&["push", "origin", &refspec], None))?;
            }
            self.state.main = commit;
            if self.config.remote.is_some() {
                self.state.remote_main = Some(self.state.main.clone());
            }
            self.state.manifest.sessions.get_mut(&id).unwrap().published = true;
            self.git.run(
                &["update-ref", "refs/heads/mesh-history", &self.state.main],
                None,
            )?;
            self.save()?;
        }
        Ok(())
    }

    pub fn unresolved(&self) -> impl Iterator<Item = &Conflict> {
        self.state
            .conflicts
            .values()
            .filter(|c| !self.state.manifest.resolutions.contains_key(&c.id))
    }

    fn discard_empty_projection(&mut self, id: &str) -> Result<()> {
        let session = self
            .state
            .manifest
            .sessions
            .get_mut(id)
            .context("Session not found")?;
        session.closed = true;
        session.discarded = true;
        // Keep its original changes, intent, and conflict inputs in the native
        // draft record, without adding an empty permanent history commit.
        session.message = Some(Message { title: "Discard session with no effective file changes".into(), description: "The session was already integrated or its changes were rejected by a recorded conflict resolution. Original snapshots and recorded intent remain recoverable in the session record.".into() });
        self.save()
    }

    pub fn summary_input(&mut self, id: &str) -> Result<Option<Session>> {
        let session = self
            .state
            .manifest
            .sessions
            .get(id)
            .context("Session not found")?
            .clone();
        if !session.dependencies.iter().all(|dep| {
            self.state
                .manifest
                .sessions
                .get(dep)
                .is_some_and(|s| s.settled())
        }) || self
            .unresolved()
            .any(|c| c.session == id || session.changes.contains_key(&c.path))
        {
            return Ok(None);
        }
        let main = self.git.files(&self.state.main)?;
        let mut prepared = session.clone();
        let mut tree = main.clone();
        let mut policy = None;
        for (path, change) in rules_first(&session) {
            let base = main.get(path).cloned();
            let content = if excluded_by(&mut policy, &self.root, &tree, path)? {
                None
            } else {
                self.merge_change(path, &base, change, &session, None)?
            };
            set_file(&mut tree, path, content.clone());
            let entry = prepared.changes.get_mut(path).unwrap();
            entry.base = base;
            entry.content = content;
        }
        // Describe what publication will do: every path of main that the
        // resulting rules exclude leaves the shared tree, and says so.
        let policy = Policy::from_files(&self.root, &tree)?;
        for (path, bytes) in &main {
            if policy.ignores(path) {
                prepared
                    .changes
                    .entry(path.clone())
                    .or_insert_with(|| Change {
                        base: Some(bytes.clone()),
                        content: None,
                        intent: None,
                        last_edit: session.last_edit,
                    })
                    .content = None;
                prepared.excluded.insert(path.clone());
            }
        }
        if self
            .unresolved()
            .any(|c| c.session == id || session.changes.contains_key(&c.path))
        {
            self.save()?;
            return Ok(None);
        }
        prepared
            .changes
            .retain(|_, change| change.base != change.content);
        if session.closed && prepared.changes.is_empty() {
            self.discard_empty_projection(id)?;
            return Ok(None);
        }
        prepared.message_base = Some(self.state.main.clone());
        Ok(Some(prepared))
    }

    pub fn resolve(&mut self, id: &str, incoming: bool) -> Result<()> {
        self.capture_at(now())?;
        let conflict = self
            .state
            .conflicts
            .get(id)
            .context("Conflict not found")?
            .clone();
        ensure!(
            !self.state.manifest.resolutions.contains_key(id),
            "Conflict is already resolved"
        );
        ensure!(
            self.state.observed.get(&conflict.path) == conflict.accepted.as_ref(),
            "File has newer edits; review them before resolving this conflict"
        );
        let selected = if incoming {
            conflict.incoming.clone()
        } else {
            conflict.accepted.clone()
        };
        self.state.manifest.resolutions.insert(
            id.to_string(),
            Resolution {
                conflict: id.to_string(),
                selected,
                author: self.config.identity.clone(),
                created_at: now(),
            },
        );
        self.save()?;
        self.render()
    }
}

/// Obsidian's per-device layout files. `init` writes them as visible, editable
/// rules; nothing is built into the engine and existing workspaces are not edited.
const OBSIDIAN_EXCLUDES: [&str; 2] = [
    ".obsidian/workspace.json",
    ".obsidian/workspace-mobile.json",
];

fn write_obsidian_excludes(root: &Path) -> Result<()> {
    if !root.join(".obsidian").is_dir() {
        return Ok(());
    }
    let path = root.join(".meshignore");
    // Check before reading or writing: a linked .meshignore must not let init
    // change a file outside the workspace, even though capture rejects it later.
    let mut text = match fs::symlink_metadata(&path) {
        Ok(_) => {
            ensure_regular_file(&path)?;
            String::from_utf8(fs::read(&path)?).context(".meshignore is not UTF-8 text")?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let missing: Vec<_> = OBSIDIAN_EXCLUDES
        .into_iter()
        .filter(|rule| !text.lines().any(|line| line.trim() == *rule))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    for rule in missing {
        text.push_str(rule);
        text.push('\n');
    }
    // Replace by rename: a link substituted after the check is replaced, not followed.
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    file.write_all(text.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(&path).map_err(|e| e.error)?;
    Ok(())
}

/// Refuses symbolic links and Windows reparse points without following them.
fn ensure_regular_file(file: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(file)?;
    ensure!(
        !meta.file_type().is_symlink(),
        "Symbolic links are unsupported in this preview: {}",
        file.display()
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            meta.file_attributes() & 0x400 == 0,
            "Reparse points are unsupported"
        );
    }
    Ok(())
}

/// A session's changes with the ignore-rule files first, so the rules a session
/// brings decide which of its other paths the resulting tree may hold.
fn rules_first(session: &Session) -> impl Iterator<Item = (&String, &Change)> {
    let (rules, others): (Vec<_>, Vec<_>) = session
        .changes
        .iter()
        .partition(|(path, _)| policy::is_rule_file(path));
    rules.into_iter().chain(others)
}

/// Whether `path` is excluded by the rules of `files`, computing that policy
/// once, on the first path that is not itself a rule file.
fn excluded_by(
    policy: &mut Option<Policy>,
    root: &Path,
    files: &Files,
    path: &str,
) -> Result<bool> {
    if policy::is_rule_file(path) {
        return Ok(false);
    }
    if policy.is_none() {
        *policy = Some(Policy::from_files(root, files)?);
    }
    Ok(policy.as_ref().is_some_and(|policy| policy.ignores(path)))
}

fn set_file(files: &mut Files, path: &str, content: Option<Vec<u8>>) {
    if let Some(content) = content {
        files.insert(path.to_string(), content);
    } else {
        files.remove(path);
    }
}
