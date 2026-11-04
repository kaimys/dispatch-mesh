use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use dispatch_mesh::{ai, engine::Workspace, model::*};
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
            ConflictCommand::Show { id } => println!("{}", serde_json::to_string_pretty(workspace.state.conflicts.get(&id).context("Conflict not found")?)?),
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
                std::fs::write(output.join("conflict.json"), serde_json::to_vec_pretty(conflict)?)?;
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

fn show_status(workspace: &Workspace) -> Result<()> {
    let timestamp = now();
    let presence: Vec<_> = workspace.state.manifest.sessions.values().filter(|s| !s.closed && !s.settled()).flat_map(|s| s.changes.iter().map(move |(path, change)| (s, path, change)))
        .filter(|(_, _, change)| timestamp.saturating_sub(change.last_edit) < 300)
        .map(|(s, path, change)| json!({"path": path, "author": s.author, "device": s.device, "last_saved": change.last_edit, "signal": "recent_save"})).collect();
    let status = if workspace.state.attention.is_some() || workspace.unresolved().next().is_some() {
        "needs_attention"
    } else if workspace
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
                    "attention": workspace.state.attention, "active_session": workspace.active_session().map(|s| &s.id),
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

async fn watch(root: PathBuf) -> Result<()> {
    let workspace = Workspace::open(&root)?;
    let root = workspace.root.clone();
    drop(workspace);
    // One watcher process per workspace, while short operation locks allow the CLI
    // to register intent and finish sessions while the process is running.
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
    let mut candidate: Option<Files> = None;
    let mut summary: Option<tokio::task::JoinHandle<Result<()>>> = None;
    let mut cycles = 0_u64;
    println!(
        "Mesh is running for {}. Ctrl+C saves local work and stops. Sessions close automatically after five minutes.",
        root.display()
    );
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                let mut workspace = Workspace::open(&root)?;
                workspace.capture_at(now())?;
                println!("Local changes saved. Mesh stopped.");
                return Ok(());
            }
            _ = tick.tick() => {
                // Events are hints; periodic full scans recover missed notifications.
                let mut dirty = cycles == 0 || cycles.is_multiple_of(30);
                for event in receiver.try_iter() {
                    match event {
                        Ok(event) => dirty |= event.paths.iter().any(|p| !p.starts_with(root.join(".mesh")) && !p.starts_with(root.join(".git"))),
                        Err(_) => dirty = true,
                    }
                }
                let capture_root = root.clone();
                let previous = candidate.take();
                let capture = tokio::task::spawn_blocking(move || -> Result<Option<Files>> {
                    let mut workspace = Workspace::open(&capture_root)?;
                    let mut next = None;
                    if dirty || previous.is_some() {
                        let files = workspace.scan()?;
                        if previous.as_ref() == Some(&files) { workspace.capture(files, now())?; }
                        else if files != workspace.state.observed { next = Some(files); }
                    }
                    workspace.close_idle(now())?;
                    Ok(next)
                }).await?;
                match capture {
                    Ok(next) => candidate = next,
                    Err(_) => eprintln!("Capture waiting: workspace busy or a file is unreadable/changing. Prior snapshots are retained."),
                }
                if cycles.is_multiple_of(3) && candidate.is_none() {
                    let sync_root = root.clone();
                    let result = tokio::task::spawn_blocking(move || -> Result<()> {
                        let mut workspace = Workspace::open(&sync_root)?;
                        if let Err(error) = workspace.sync() {
                            workspace.state.attention = Some("Synchronization paused or offline; local snapshots retained. Retry sync after checking repository access and conflicts.".into());
                            workspace.save()?;
                            return Err(error);
                        }
                        Ok(())
                    }).await?;
                    if result.is_err() { eprintln!("Synchronization waiting; local capture continues. See status for details."); }
                }
                if summary.as_ref().is_some_and(|job| job.is_finished())
                    && let Some(job) = summary.take() {
                        match job.await { Ok(Ok(())) => {}, _ => eprintln!("Summary deferred; next cycle will retry.") }
                }
                if summary.is_none() {
                    let root = root.clone();
                    summary = Some(tokio::spawn(async move { summarize_ready(&root).await }));
                }
                cycles += 1;
            }
        }
    }
}
