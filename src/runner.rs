//! The run loop shared by CLI `run` and desktop hosts (ADR-0009).
//!
//! A host embeds the engine in-process: [`Runner::start`] takes the workspace's
//! runner lock on the calling thread, then drives [`run`] on a background thread
//! with its own Tokio runtime. CLI commands keep working against the same
//! workspace through the on-disk operation locks, and the loop re-reads durable
//! state on every tick, so their effects reach the published snapshot.

use crate::{
    ai,
    engine::{DependentEditsPreserved, Workspace, WorkspaceBusy},
    model::{Files, now},
    status::{LocalObservation, Status},
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::{
    fs::File,
    future::Future,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};
use tokio::sync::watch;

/// Another runner (CLI `run` or a [`RunnerHandle`]) owns this workspace.
#[derive(Debug)]
pub struct AlreadyRunning;

impl std::fmt::Display for AlreadyRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Mesh is already running for this workspace")
    }
}

impl std::error::Error for AlreadyRunning {}

/// Exclusive ownership of a workspace's run loop: the held `.mesh/runner.lock`
/// and the file watcher. Dropping it releases the lock.
pub struct RunnerLease {
    root: PathBuf,
    branch: String,
    _lock: File,
    _watcher: notify::RecommendedWatcher,
    events: mpsc::Receiver<notify::Result<notify::Event>>,
}

impl RunnerLease {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn branch(&self) -> &str {
        &self.branch
    }
}

/// Take the runner lock and start watching the folder. Fails with
/// [`AlreadyRunning`] when another runner holds the lock; nothing is started.
pub fn acquire(root: &Path) -> Result<RunnerLease> {
    // Startup only needs committed configuration. A simultaneous intent/finish
    // command may hold the operation lock; the capture loop retries that lock.
    let workspace = Workspace::inspect(root)?;
    let root = workspace.root.clone();
    let branch = workspace.config.branch.clone();
    drop(workspace);
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(".mesh/runner.lock"))?;
    match fs2::FileExt::try_lock_exclusive(&lock) {
        Ok(()) => {}
        Err(error)
            if error.kind() == std::io::ErrorKind::WouldBlock
                || [Some(33), Some(11)].contains(&error.raw_os_error()) =>
        {
            return Err(AlreadyRunning.into());
        }
        Err(error) => return Err(error).context("Cannot lock the workspace runner"),
    }
    let (sender, events) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let _ = sender.send(event);
    })?;
    notify::Watcher::watch(&mut watcher, &root, notify::RecursiveMode::Recursive)?;
    Ok(RunnerLease {
        root,
        branch,
        _lock: lock,
        _watcher: watcher,
        events,
    })
}

/// What a running engine publishes: the workspace status plus the loop's own
/// current failures. Messages never contain credentials or response bodies.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunnerSnapshot {
    pub status: Status,
    pub capture_error: Option<String>,
    pub sync_error: Option<String>,
    pub summary_error: Option<String>,
}

/// Status channel of a runner. `None` until the first tick, which follows
/// startup immediately.
pub type StatusReceiver = watch::Receiver<Option<RunnerSnapshot>>;
pub type StatusSender = watch::Sender<Option<RunnerSnapshot>>;

/// Summarize this device's closed sessions that have no message yet and
/// persist the latest provider outcome (ADR-0005, ADR-0010).
pub async fn summarize_ready(root: &Path) -> Result<()> {
    let mut workspace = Workspace::open(root)?;
    let config = workspace.config.clone();
    let ids: Vec<_> = workspace
        .state
        .manifest
        .sessions
        .values()
        .filter(|s| s.device == config.device && s.closed && !s.settled() && s.message.is_none())
        .map(|s| s.id.clone())
        .collect();
    let mut sessions = Vec::new();
    for id in ids {
        let hash = workspace.state.manifest.sessions[&id].snapshot_hash();
        if let Some(prepared) = workspace.summary_input(&id)? {
            sessions.push((prepared, hash));
        }
    }
    drop(workspace);
    for (session, hash) in sessions {
        let (message, failure) = ai::generate_with_failure(&config, &session).await;
        let mut workspace = Workspace::open(root)?;
        // Persist the latest provider outcome so `status` can explain it (ADR-0010).
        if workspace.state.summary_failure != failure {
            workspace.state.summary_failure = failure;
            workspace.save()?;
        }
        if let Some(current) = workspace.state.manifest.sessions.get_mut(&session.id)
            && current.snapshot_hash() == hash
            && current.closed
            && current.message.is_none()
            && session.message_base.as_deref() == Some(&workspace.state.main)
        {
            current.message = Some(message);
            current.message_base = session.message_base;
            workspace.save()?;
        }
    }
    Ok(())
}

fn report_failure(label: &str, error: &anyhow::Error, previous: &mut Option<(String, Instant)>) {
    if error.is::<WorkspaceBusy>() {
        return;
    }
    let message = format!("{error:#}");
    if previous
        .as_ref()
        .is_none_or(|(old, time)| old != &message || time.elapsed() >= Duration::from_secs(60))
    {
        eprintln!("{label}: {message}. Prior snapshots are retained.");
        *previous = Some((message, Instant::now()));
    }
}

/// How long shutdown waits for a busy operation lock before giving up. The
/// lock is only held for local work (Git runs without it), so a longer wait
/// means something is wrong; captured work is already durable (ADR-0007).
const SHUTDOWN_LOCK_WAIT: Duration = Duration::from_secs(10);

/// Capture saved changes for shutdown, riding out a briefly held operation lock.
fn capture_for_shutdown(root: &Path) -> Result<()> {
    let started = Instant::now();
    let mut workspace = loop {
        match Workspace::open(root) {
            Ok(workspace) => break workspace,
            Err(error) if error.is::<WorkspaceBusy>() && started.elapsed() < SHUTDOWN_LOCK_WAIT => {
                std::thread::sleep(Duration::from_millis(25))
            }
            Err(error) => return Err(error),
        }
    };
    workspace.capture_at(now())?;
    Ok(())
}

/// Capture continuously and synchronize every few seconds until `shutdown`
/// completes, then capture saved changes and return. In-flight network and AI
/// work is not awaited. When `status` is given, a [`RunnerSnapshot`] is sent
/// after each tick whose snapshot differs from the last one.
pub async fn run(
    lease: RunnerLease,
    shutdown: impl Future<Output = ()>,
    status: Option<StatusSender>,
) -> Result<()> {
    let root = lease.root.clone();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut candidate: Option<Files> = None;
    let mut pending_scan = true;
    let mut summary: Option<tokio::task::JoinHandle<Result<()>>> = None;
    let mut synchronization: Option<tokio::task::JoinHandle<Result<()>>> = None;
    let mut next_sync = Instant::now();
    let mut capture_error = None;
    let mut sync_error = None;
    let mut summary_error = None;
    let mut cycles = 0_u64;
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => {
                // A short local operation never waits for an HTTP response.
                let root = root.clone();
                return tokio::task::spawn_blocking(move || capture_for_shutdown(&root)).await?;
            }
            _ = tick.tick() => {
                if synchronization.as_ref().is_some_and(|job| job.is_finished())
                    && let Some(job) = synchronization.take() {
                    match job.await? {
                        Ok(()) => { if sync_error.take().is_some() { println!("Synchronization resumed."); } },
                        Err(error) => report_failure("Synchronization waiting", &error, &mut sync_error),
                    }
                    next_sync = Instant::now() + Duration::from_secs(3);
                }
                if summary.as_ref().is_some_and(|job| job.is_finished())
                    && let Some(job) = summary.take() {
                    match job.await? {
                        Ok(()) => summary_error = None,
                        Err(error) => report_failure("Summary deferred", &error, &mut summary_error),
                    }
                }
                pending_scan |= cycles.is_multiple_of(30);
                for event in lease.events.try_iter() {
                    match event {
                        Ok(event) => pending_scan |= event.paths.iter().any(|p| !p.starts_with(root.join(".mesh")) && !p.starts_with(root.join(".git"))),
                        Err(_) => pending_scan = true,
                    }
                }
                let capture_root = root.clone();
                // Retain both the candidate and dirty flag if another operation briefly
                // owns the lock. Dropping them could postpone capture until the next scan.
                let previous = candidate.clone();
                let dirty = pending_scan;
                let publish = status.is_some();
                let capture = tokio::task::spawn_blocking(move || -> Result<(Option<Files>, bool, Option<Status>)> {
                    let mut workspace = Workspace::open(&capture_root)?;
                    let mut next = None;
                    if dirty || previous.is_some() {
                        let files = workspace.scan()?;
                        if previous.as_ref() == Some(&files) { workspace.capture(files, now())?; }
                        else if files != workspace.state.observed { next = Some(files); }
                    }
                    workspace.close_idle(now())?;
                    let needs_summary = workspace.state.manifest.sessions.values().any(|s| s.device == workspace.config.device && s.closed && !s.settled() && s.message.is_none());
                    // The state just loaded includes changes made by other processes.
                    let snapshot = publish.then(|| workspace.status_from(LocalObservation { changes_waiting: next.is_some(), capture_error: None }));
                    Ok((next, needs_summary, snapshot))
                }).await?;
                let mut needs_summary = false;
                let mut snapshot = None;
                match capture {
                    Ok((next, ready, status)) => {
                        candidate = next;
                        pending_scan = false;
                        needs_summary = ready;
                        snapshot = status;
                        if capture_error.take().is_some() { println!("Local capture resumed."); }
                    },
                    // Capture recorded the reason in workspace attention, where
                    // status shows it with what resolves it; it is no capture fault.
                    Err(error) if error.is::<DependentEditsPreserved>() => { pending_scan = true; capture_error = None; },
                    Err(error) => { pending_scan = true; report_failure("Capture waiting", &error, &mut capture_error); },
                }
                if let Some(sender) = &status {
                    if snapshot.is_none() {
                        // Capture could not open the workspace (for example, a CLI command
                        // holds the lock). Committed state is readable without the lock.
                        let fallback_root = root.clone();
                        let local = LocalObservation {
                            changes_waiting: candidate.is_some(),
                            capture_error: capture_error.as_ref().map(|(message, _)| message.clone()),
                        };
                        snapshot = tokio::task::spawn_blocking(move || {
                            Workspace::inspect(&fallback_root).ok().map(|workspace| workspace.status_from(local))
                        }).await?;
                    }
                    if let Some(status) = snapshot {
                        let next = RunnerSnapshot {
                            status,
                            capture_error: capture_error.as_ref().map(|(message, _)| message.clone()),
                            sync_error: sync_error.as_ref().map(|(message, _)| message.clone()),
                            summary_error: summary_error.as_ref().map(|(message, _)| message.clone()),
                        };
                        sender.send_if_modified(|current| {
                            if current.as_ref() == Some(&next) { false } else { *current = Some(next); true }
                        });
                    }
                }
                if synchronization.is_none() && candidate.is_none() && !pending_scan && Instant::now() >= next_sync {
                    let sync_root = root.clone();
                    synchronization = Some(tokio::task::spawn_blocking(move || -> Result<()> {
                        Workspace::sync_and_record(&sync_root)
                    }));
                }
                if summary.is_none() && needs_summary {
                    let root = root.clone();
                    summary = Some(tokio::spawn(async move { summarize_ready(&root).await }));
                }
                cycles += 1;
            }
        }
    }
}

/// Entry point for desktop hosts.
pub struct Runner;

impl Runner {
    /// Start the engine for `root` on a background thread with its own Tokio
    /// runtime; the caller needs none. Fails with [`AlreadyRunning`] when the
    /// workspace already has a runner, and then starts nothing; a later call
    /// succeeds once that runner stops.
    pub fn start(root: &Path) -> Result<RunnerHandle> {
        let lease = acquire(root)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("Cannot start the Mesh engine runtime")?;
        let (stop, mut stop_requested) = watch::channel(false);
        let (status_tx, status) = watch::channel(None);
        let thread = std::thread::Builder::new()
            .name("mesh-runner".into())
            .spawn(move || {
                let shutdown = async move {
                    let _ = stop_requested.wait_for(|stop| *stop).await;
                };
                let result = runtime.block_on(run(lease, shutdown, Some(status_tx)));
                // Detached Git or HTTP work is not awaited; the locks serialize it
                // against a restarted runner.
                runtime.shutdown_background();
                result
            })
            .context("Cannot start the Mesh engine thread")?;
        Ok(RunnerHandle {
            stop,
            status,
            thread: Some(thread),
        })
    }
}

/// A running, in-process engine. Dropping it stops the engine and waits for
/// the final capture.
pub struct RunnerHandle {
    stop: watch::Sender<bool>,
    status: StatusReceiver,
    thread: Option<std::thread::JoinHandle<Result<()>>>,
}

impl RunnerHandle {
    /// Snapshots, replaced only when they change. `has_changed` and
    /// `borrow_and_update` need no async runtime.
    pub fn status(&self) -> StatusReceiver {
        self.status.clone()
    }

    /// Ask the engine to capture saved changes and stop. Idempotent.
    pub fn stop(&self) {
        self.stop.send_replace(true);
    }

    /// Wait for the engine to finish and return how its loop ended.
    pub fn join(mut self) -> Result<()> {
        self.wait()
    }

    fn wait(&mut self) -> Result<()> {
        match self.thread.take() {
            Some(thread) => thread
                .join()
                .unwrap_or_else(|_| Err(anyhow::anyhow!("The Mesh engine thread panicked"))),
            None => Ok(()),
        }
    }
}

impl Drop for RunnerHandle {
    fn drop(&mut self) {
        self.stop();
        let _ = self.wait();
    }
}
