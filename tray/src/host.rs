//! Runs one embedded engine per workspace and reports their states
//! (US00001; ADR-0009, ADR-0012).
//!
//! Free of Tauri types: the window and tray subscribe through a listener, so
//! starting, refusing, retrying and quitting are tested with real workspaces.

use crate::model::{Workspace, WorkspaceState};
use anyhow::{Context, Result, bail};
use dispatch_mesh::{
    engine::{Workspace as Engine, WorkspaceBusy},
    runner::{AlreadyRunning, Runner, RunnerHandle, StatusReceiver},
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

type Listener = dyn Fn(Vec<Workspace>) + Send + Sync;

struct Entry {
    workspace: Workspace,
    handle: Option<RunnerHandle>,
    /// Bumped on every start, so a listener from an earlier run is ignored.
    generation: u64,
}

pub struct Host {
    entries: Mutex<Vec<Entry>>,
    listener: Box<Listener>,
    runtime: Option<tokio::runtime::Runtime>,
    /// Set once Quit begins; read without a lock where a stale `false` is
    /// harmless. It only changes while `starts` is locked.
    quitting: AtomicBool,
    /// Starts in flight. Quit refuses new starts and waits for these, because
    /// an engine that is still starting holds saved edits it must capture.
    starts: Mutex<Starts>,
    starts_done: Condvar,
}

#[derive(Default)]
struct Starts {
    quitting: bool,
    in_flight: usize,
}

/// Counts one start as in flight until it is dropped, even on a panic.
struct StartGuard<'a>(&'a Host);

impl Drop for StartGuard<'_> {
    fn drop(&mut self) {
        let mut starts = self.0.starts.lock().unwrap_or_else(|e| e.into_inner());
        starts.in_flight -= 1;
        self.0.starts_done.notify_all();
    }
}

/// A friendly name for a workspace: its folder name.
fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn resolve(path: PathBuf) -> PathBuf {
    dunce::canonicalize(&path)
        .or_else(|_| std::path::absolute(&path))
        .unwrap_or(path)
}

impl Host {
    /// A host for `paths`, in the given order with duplicates removed.
    /// Nothing starts until [`Host::start_all`]. `listener` receives every
    /// workspace's state after each change, on a host thread.
    pub fn new(
        paths: impl IntoIterator<Item = PathBuf>,
        listener: impl Fn(Vec<Workspace>) + Send + Sync + 'static,
    ) -> Result<Arc<Self>> {
        let mut seen: Vec<PathBuf> = Vec::new();
        for path in paths.into_iter().map(resolve) {
            if !seen.contains(&path) {
                seen.push(path);
            }
        }
        let entries = seen
            .into_iter()
            .enumerate()
            .map(|(id, path)| Entry {
                workspace: Workspace {
                    id,
                    name: display_name(&path),
                    path,
                    state: WorkspaceState::Starting,
                },
                handle: None,
                generation: 0,
            })
            .collect();
        // Status channels need no timers or I/O; one worker is plenty.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("mesh-host")
            .build()
            .context("Cannot start the tray host runtime")?;
        Ok(Arc::new(Self {
            entries: Mutex::new(entries),
            listener: Box::new(listener),
            runtime: Some(runtime),
            quitting: AtomicBool::new(false),
            starts: Mutex::new(Starts::default()),
            starts_done: Condvar::new(),
        }))
    }

    pub fn workspaces(&self) -> Vec<Workspace> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .map(|entry| entry.workspace.clone())
            .collect()
    }

    fn notify(&self) {
        (self.listener)(self.workspaces());
    }

    pub fn start_all(self: &Arc<Self>) {
        let count = self.entries.lock().unwrap().len();
        for id in 0..count {
            self.start(id);
        }
        if count == 0 {
            self.notify();
        }
    }

    /// Start a workspace again after "already running" or a stop. Other
    /// states are left alone, so a double click cannot start a second engine.
    pub fn retry(self: &Arc<Self>, id: usize) {
        let retryable = self.entries.lock().unwrap().get(id).is_some_and(|entry| {
            matches!(
                entry.workspace.state,
                WorkspaceState::AlreadyRunning | WorkspaceState::Stopped(_)
            )
        });
        if retryable {
            self.start(id);
        }
    }

    fn start(self: &Arc<Self>, id: usize) {
        {
            let mut starts = self.starts.lock().unwrap();
            if starts.quitting {
                return;
            }
            starts.in_flight += 1;
        }
        // Released after the last notification below, so Quit returns only
        // once this start has finished, including a late engine's final capture.
        let _in_flight = StartGuard(self);
        let (path, generation) = {
            let mut entries = self.entries.lock().unwrap();
            let entry = &mut entries[id];
            entry.generation += 1;
            entry.workspace.state = WorkspaceState::Starting;
            (entry.workspace.path.clone(), entry.generation)
        };
        self.notify();
        let result = if path.join(".mesh").join("config.json").is_file() {
            Runner::start(&path)
        } else {
            Err(anyhow::anyhow!(
                "This folder is not a Mesh workspace yet. Set it up with dispatch-mesh init, then retry."
            ))
        };
        let mut late = None;
        {
            let mut entries = self.entries.lock().unwrap();
            let entry = &mut entries[id];
            match result {
                Ok(handle) if self.quitting.load(Ordering::SeqCst) => late = Some(handle),
                Ok(handle) => {
                    self.listen(id, generation, handle.status());
                    entry.handle = Some(handle);
                    entry.workspace.state = WorkspaceState::Running(None);
                }
                Err(error) if error.is::<AlreadyRunning>() => {
                    entry.workspace.state = WorkspaceState::AlreadyRunning;
                }
                Err(error) => entry.workspace.state = WorkspaceState::Stopped(format!("{error:#}")),
            }
        }
        // Quit began while this engine started: dropping it stops it and waits
        // for its final capture, before this start counts as finished.
        drop(late);
        self.notify();
    }

    fn listen(self: &Arc<Self>, id: usize, generation: u64, mut status: StatusReceiver) {
        let host: Weak<Self> = Arc::downgrade(self);
        let Some(runtime) = &self.runtime else { return };
        runtime.spawn(async move {
            loop {
                let snapshot = status.borrow_and_update().clone();
                if let Some(snapshot) = snapshot {
                    let Some(host) = host.upgrade() else { return };
                    host.update(
                        id,
                        generation,
                        WorkspaceState::Running(Some(Box::new(snapshot))),
                    );
                }
                if status.changed().await.is_err() {
                    break;
                }
            }
            // The engine's loop ended without being asked to.
            if let Some(host) = host.upgrade() {
                host.ended(id, generation);
            }
        });
    }

    fn update(&self, id: usize, generation: u64, state: WorkspaceState) {
        {
            let mut entries = self.entries.lock().unwrap();
            let entry = &mut entries[id];
            if entry.generation != generation || entry.workspace.state == state {
                return;
            }
            entry.workspace.state = state;
        }
        self.notify();
    }

    fn ended(&self, id: usize, generation: u64) {
        if self.quitting.load(Ordering::SeqCst) {
            return;
        }
        let handle = {
            let mut entries = self.entries.lock().unwrap();
            let entry = &mut entries[id];
            if entry.generation != generation {
                return;
            }
            entry.handle.take()
        };
        let reason = match handle.map(RunnerHandle::join) {
            Some(Err(error)) => format!("The Mesh engine stopped: {error:#}"),
            _ => "The Mesh engine stopped.".to_owned(),
        };
        self.update(id, generation, WorkspaceState::Stopped(reason));
    }

    /// Close the workspace's open session, exactly as CLI `session finish`
    /// does next to a running engine; that engine then summarizes and
    /// publishes it, and its next snapshot shows the session finishing up.
    /// Refused unless the engine runs here, because then this host does not
    /// look after the workspace. Waits for the engine's current operation, so
    /// call it off the UI thread. Returns the finished session's id, if any.
    pub fn finish(&self, id: usize) -> Result<Option<String>> {
        let path = {
            let entries = self.entries.lock().unwrap();
            let entry = entries.get(id).context("No such workspace")?;
            if entry.handle.is_none()
                || !matches!(entry.workspace.state, WorkspaceState::Running(_))
            {
                bail!("Mesh is not running this workspace here");
            }
            entry.workspace.path.clone()
        };
        let started = std::time::Instant::now();
        let mut workspace = loop {
            match Engine::open(&path) {
                Ok(workspace) => break workspace,
                Err(error)
                    if error.is::<WorkspaceBusy>()
                        && started.elapsed() < std::time::Duration::from_secs(30) =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(50))
                }
                Err(error) => return Err(error),
            }
        };
        workspace.finish()
    }

    /// Where a saved copy listed in the workspace's latest status is kept, or
    /// `None` for an id that status does not list. The window sends only ids,
    /// never paths, so it cannot point at anything else on disk.
    pub fn saved_copy_location(&self, id: usize, copy: &str) -> Option<PathBuf> {
        uuid::Uuid::try_parse(copy).ok()?;
        let entries = self.entries.lock().unwrap();
        let workspace = &entries.get(id)?.workspace;
        workspace
            .status()?
            .saved_copies
            .iter()
            .any(|saved| saved.id == copy)
            .then(|| {
                workspace
                    .path
                    .join(".mesh")
                    .join("recovery")
                    .join(copy)
                    .join("content")
            })
    }

    /// Stop every engine and wait for each to capture saved changes. Engines
    /// stop in parallel; none waits for network or AI work (ADR-0009). Starts
    /// already under way finish and stop their engine before this returns;
    /// later starts are refused. Holds no lock while listeners run.
    pub fn quit(&self) {
        {
            let mut starts = self.starts.lock().unwrap();
            starts.quitting = true;
            self.quitting.store(true, Ordering::SeqCst);
        }
        let handles: Vec<RunnerHandle> = self
            .entries
            .lock()
            .unwrap()
            .iter_mut()
            .filter_map(|entry| entry.handle.take())
            .collect();
        for handle in &handles {
            handle.stop();
        }
        for handle in handles {
            let _ = handle.join();
        }
        let starts = self.starts.lock().unwrap();
        let _finished = self
            .starts_done
            .wait_while(starts, |starts| starts.in_flight > 0)
            .unwrap();
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.quit();
        // The last reference may be dropped on a host thread, where dropping a
        // runtime would panic.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}
