use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use dispatch_mesh::{
    ai,
    engine::{Workspace, WorkspaceBusy},
    model::*,
};
use notify::Watcher;
use serde_json::json;
use std::{path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(
    version,
    about = "Dispatch Mesh — console preview: saved drafts, sessions, and recoverable conflicts"
)]
struct Cli {
    #[arg(long, global = true, default_value = ".")]
    workspace: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage a folder, optionally connecting an existing remote repository.
    Init {
        #[arg(long)]
        name: String,
        #[arg(long)]
        email: String,
        #[arg(long)]
        remote: Option<String>,
        #[arg(long, default_value = "main")]
        branch: String,
    },
    /// Capture continuously; synchronize drafts every few seconds.
    Run,
    /// Capture and synchronize once, then finalize closed sessions.
    Sync,
    /// Show sessions, recent-save presence, and unresolved conflicts as JSON.
    Status,
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    Intent {
        #[command(subcommand)]
        command: IntentCommand,
    },
    Conflicts {
        #[command(subcommand)]
        command: ConflictCommand,
    },
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
}

#[derive(Subcommand)]
enum SessionCommand {
    Finish,
}

#[derive(Subcommand)]
enum IntentCommand {
    /// Begin an exclusive agent intent scope. Exact paths limit attribution.
    Begin {
        #[arg(long)]
        purpose: String,
        #[arg(long, required = true, num_args = 1..)]
        paths: Vec<String>,
    },
    /// Capture edits and end the intent scope without closing the session.
    End { id: String },
}

#[derive(Subcommand)]
enum ConflictCommand {
    List,
    Show {
        id: String,
    },
    /// Export base/accepted/incoming versions to a NEW directory outside the workspace.
    Export {
        id: String,
        #[arg(long)]
        output: PathBuf,
    },
    Resolve {
        id: String,
        #[arg(long, value_parser = ["accepted", "incoming"])]
        choose: String,
    },
}

#[derive(Subcommand)]
enum ProviderCommand {
    /// Configure an OpenAI-compatible endpoint. Contents are withheld unless opted in.
    Set {
        #[arg(long)]
        base_url: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        api_key_env: Option<String>,
        #[arg(long)]
        json_mode: bool,
        /// Allow sending text snapshots and recorded intent to this endpoint.
        #[arg(long)]
        send_text: bool,
    },
    Off,
    /// Send a synthetic change, without workspace content, to test the configured provider.
    Test,
}

#[tokio::main]
async fn main() {
    if let Err(error) = execute(Cli::parse()).await {
        eprintln!("Mesh: {error:#}");
        std::process::exit(1);
    }
}

async fn execute(cli: Cli) -> Result<()> {
    if let Command::Init {
        name,
        email,
        remote,
        branch,
    } = cli.command
    {
        let workspace = Workspace::init(&cli.workspace, Identity { name, email }, remote, branch)?;
        println!(
            "Workspace ready: {}\nRun `dispatch-mesh --workspace <folder> run` to synchronize. This console preview requires Git; guided desktop setup is planned.",
            workspace.root.display()
        );
        return Ok(());
    }
    if matches!(cli.command, Command::Run) {
        return watch(cli.workspace).await;
    }
    if matches!(cli.command, Command::Status) {
        return show_status(&Workspace::inspect(&cli.workspace)?);
    }
    let mut workspace = Workspace::open(&cli.workspace)?;
    match cli.command {
        Command::Sync => {
            workspace.sync()?;
            drop(workspace);
            summarize_ready(&cli.workspace).await?;
            let mut workspace = Workspace::open(&cli.workspace)?;
            workspace.sync()?;
            show_status(&workspace)?;
        }
        Command::Status => show_status(&workspace)?,
        Command::Session { command: SessionCommand::Finish } => {
            let id = workspace.finish()?;
            println!("{}", json!({"session_id": id, "status": if id.is_some() { "queued" } else { "no_active_session" }, "note": "The run process or next sync performs summarization and publication."}));
        }
        Command::Intent { command } => match command {
            IntentCommand::Begin { purpose, paths } => println!("{}", json!({"intent_id": workspace.register_intent(purpose, paths.into_iter().collect())?})),
            IntentCommand::End { id } => { workspace.end_intent(&id)?; println!("Intent scope ended; session remains open."); },
        },
        Command::Conflicts { command } => match command {
            ConflictCommand::List => println!("{}", serde_json::to_string_pretty(&workspace.unresolved().map(|c| json!({"id": c.id, "path": c.path, "binary": c.binary, "incoming_author": c.author})).collect::<Vec<_>>())?),
            ConflictCommand::Show { id } => println!("{}", serde_json::to_string_pretty(&conflict_details(workspace.state.conflicts.get(&id).context("Conflict not found")?))?),
            ConflictCommand::Resolve { id, choose } => { workspace.resolve(&id, choose == "incoming")?; println!("Resolution recorded. Run sync or keep Mesh running to share it."); },
            ConflictCommand::Export { id, output } => {
                let conflict = workspace.state.conflicts.get(&id).context("Conflict not found")?;
                ensure!(!output.exists(), "Export destination must not already exist");
                std::fs::create_dir_all(&output)?;
                let output = std::fs::canonicalize(output)?;
                ensure!(!output.starts_with(&workspace.root), "Export outside the managed folder to avoid syncing conflict copies");
                for (name, content) in [("base", &conflict.base), ("accepted", &conflict.accepted), ("incoming", &conflict.incoming)] {
                    if let Some(bytes) = content { std::fs::write(output.join(name), bytes)?; }
                }
                std::fs::write(output.join("conflict.json"), serde_json::to_vec_pretty(&conflict_details(conflict))?)?;
                println!("Exported to {}", output.display());
            }
        },
        Command::Provider { command } => match command {
            ProviderCommand::Set { base_url, model, api_key_env, json_mode, send_text } => {
                let provider = ProviderConfig { base_url, model, api_key_env, json_mode, send_text };
                ai::validate_config(&provider)?;
                workspace.config.provider = Some(provider);
                workspace.save_config()?;
                println!("Provider configured. Only this endpoint will be contacted. Text and intent sharing: {send_text}.");
            }
            ProviderCommand::Off => { workspace.config.provider = None; workspace.save_config()?; println!("AI disabled; factual local commit messages will be used."); },
            ProviderCommand::Test => {
                use ai::Summarizer;
                let config = workspace.config.provider.clone().context("Configure a provider first")?;
                let mut sample = Session { id: "connection-test".into(), device: "test".into(), author: workspace.config.identity.clone(), created_at: 0, last_edit: 0, revision: 1, closed: true, published: false,
            discarded: false, changes: Default::default(), dependencies: Default::default(), intents: Default::default(), message: None, message_base: None };
                sample.changes.insert("example.md".into(), Change { base: Some(b"Hello".to_vec()), content: Some(b"Hello, Mesh".to_vec()), intent: None, last_edit: now() });
                drop(workspace);
                let message = (ai::ChatCompletions { config }).summarize(&sample).await?;
                println!("{}", serde_json::to_string_pretty(&message)?);
            }
        },
        Command::Init { .. } | Command::Run => unreachable!(),
    }
    Ok(())
}

fn conflict_details(conflict: &Conflict) -> serde_json::Value {
    let version = |name: &str, bytes: &Option<Vec<u8>>| {
        bytes
            .as_ref()
            .map(|bytes| json!({"file": name, "bytes": bytes.len(), "sha256": digest(bytes)}))
    };
    json!({"format": 2, "id": conflict.id, "path": conflict.path, "session": conflict.session, "author": conflict.author,
        "binary": conflict.binary, "created_at": conflict.created_at,
        "base": version("base", &conflict.base), "accepted": version("accepted", &conflict.accepted), "incoming": version("incoming", &conflict.incoming)})
}

fn show_status(workspace: &Workspace) -> Result<()> {
    let scan = workspace.scan();
    let capture_error = scan.as_ref().err().map(|error| format!("{error:#}"));
    let local_changes_waiting = scan
        .as_ref()
        .is_ok_and(|files| files != &workspace.state.observed);
    let timestamp = now();
    let presence: Vec<_> = workspace.state.manifest.sessions.values().filter(|s| !s.closed && !s.settled()).flat_map(|s| s.changes.iter().map(move |(path, change)| (s, path, change)))
        .filter(|(_, _, change)| timestamp.saturating_sub(change.last_edit) < 300)
        .map(|(s, path, change)| json!({"path": path, "author": s.author, "device": s.device, "last_saved": change.last_edit, "signal": "recent_save"})).collect();
    let status = if workspace.state.attention.is_some()
        || capture_error.is_some()
        || workspace.unresolved().next().is_some()
    {
        "needs_attention"
    } else if local_changes_waiting
        || workspace
            .state
            .manifest
            .sessions
            .values()
            .any(|s| s.device == workspace.config.device && !s.settled())
    {
        "waiting_to_sync"
    } else {
        "synced"
    };
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"status": status, "device": workspace.config.device, "main": workspace.state.main,
                    "commit_message_mode": match &workspace.config.provider { None => "local_fallback", Some(provider) if provider.send_text => "ai_with_text", Some(_) => "local_metadata" },
                    "attention": workspace.state.attention, "capture_error": capture_error, "local_changes_waiting": local_changes_waiting, "recovery_pending": workspace.state.applying.is_some(), "active_session": workspace.active_session().map(|s| &s.id),
            "sessions": workspace.state.manifest.sessions.values().map(|s| json!({"id": s.id, "author": s.author, "closed": s.closed, "published": s.published, "discarded": s.discarded, "files": s.changes.keys().collect::<Vec<_>>(), "dependencies": s.dependencies})).collect::<Vec<_>>(),
                    "unresolved_conflicts": workspace.unresolved().map(|c| json!({"id": c.id, "path": c.path, "binary": c.binary})).collect::<Vec<_>>(), "presence": presence
                })
        )?
    );
    Ok(())
}

async fn summarize_ready(root: &std::path::Path) -> Result<()> {
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
        let message = ai::generate(&config, &session).await;
        let mut workspace = Workspace::open(root)?;
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

fn report_failure(
    label: &str,
    error: &anyhow::Error,
    previous: &mut Option<(String, std::time::Instant)>,
) {
    if error.is::<WorkspaceBusy>() {
        return;
    }
    let message = format!("{error:#}");
    if previous
        .as_ref()
        .is_none_or(|(old, time)| old != &message || time.elapsed() >= Duration::from_secs(60))
    {
        eprintln!("{label}: {message}. Prior snapshots are retained.");
        *previous = Some((message, std::time::Instant::now()));
    }
}

async fn watch(root: PathBuf) -> Result<()> {
    // Startup only needs committed configuration. A simultaneous intent/finish
    // command may hold the operation lock; the capture loop retries that lock.
    let workspace = Workspace::inspect(&root)?;
    let root = workspace.root.clone();
    let branch = workspace.config.branch.clone();
    drop(workspace);
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(".mesh/runner.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock)
        .context("Mesh is already running for this workspace")?;
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let _ = sender.send(event);
    })?;
    watcher.watch(&root, notify::RecursiveMode::Recursive)?;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut candidate: Option<Files> = None;
    let mut pending_scan = true;
    let mut summary: Option<tokio::task::JoinHandle<Result<()>>> = None;
    let mut synchronization: Option<tokio::task::JoinHandle<Result<()>>> = None;
    let mut next_sync = std::time::Instant::now();
    let mut capture_error = None;
    let mut sync_error = None;
    let mut summary_error = None;
    let mut cycles = 0_u64;
    println!(
        "Mesh is running for {}. Ctrl+C saves local work and stops. Sessions close automatically after five minutes.\nDrafts synchronize continuously as internal snapshots. Files appear on GitHub's {branch} branch after session finalization; use session finish to finalize sooner.",
        root.display()
    );
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                // A short local operation never waits for an HTTP response.
                let mut workspace = Workspace::open(&root)?;
                workspace.capture_at(now())?;
                println!("Local changes saved. Mesh stopped.");
                return Ok(());
            }
            _ = tick.tick() => {
                if synchronization.as_ref().is_some_and(|job| job.is_finished())
                    && let Some(job) = synchronization.take() {
                    match job.await? {
                        Ok(()) => { if sync_error.take().is_some() { println!("Synchronization resumed."); } },
                        Err(error) => report_failure("Synchronization waiting", &error, &mut sync_error),
                    }
                    next_sync = std::time::Instant::now() + Duration::from_secs(3);
                }
                if summary.as_ref().is_some_and(|job| job.is_finished())
                    && let Some(job) = summary.take() {
                    match job.await? {
                        Ok(()) => summary_error = None,
                        Err(error) => report_failure("Summary deferred", &error, &mut summary_error),
                    }
                }
                pending_scan |= cycles.is_multiple_of(30);
                for event in receiver.try_iter() {
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
                let capture = tokio::task::spawn_blocking(move || -> Result<(Option<Files>, bool)> {
                    let mut workspace = Workspace::open(&capture_root)?;
                    let mut next = None;
                    if dirty || previous.is_some() {
                        let files = workspace.scan()?;
                        if previous.as_ref() == Some(&files) { workspace.capture(files, now())?; }
                        else if files != workspace.state.observed { next = Some(files); }
                    }
                    workspace.close_idle(now())?;
                    let needs_summary = workspace.state.manifest.sessions.values().any(|s| s.device == workspace.config.device && s.closed && !s.settled() && s.message.is_none());
                    Ok((next, needs_summary))
                }).await?;
                let mut needs_summary = false;
                match capture {
                    Ok((next, ready)) => {
                        candidate = next;
                        pending_scan = false;
                        needs_summary = ready;
                        if capture_error.take().is_some() { println!("Local capture resumed."); }
                    },
                    Err(error) => { pending_scan = true; report_failure("Capture waiting", &error, &mut capture_error); },
                }
                if synchronization.is_none() && candidate.is_none() && !pending_scan && std::time::Instant::now() >= next_sync {
                    let sync_root = root.clone();
                    synchronization = Some(tokio::task::spawn_blocking(move || -> Result<()> {
                        let mut workspace = Workspace::open(&sync_root)?;
                        let result = workspace.sync();
                        drop(workspace);
                        if let Err(error) = &result && !error.is::<WorkspaceBusy>()
                            && let Ok(mut workspace) = Workspace::open(&sync_root) {
                            workspace.state.attention = Some(format!("Synchronization waiting: {error:#}"));
                            workspace.save()?;
                        }
                        result
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
