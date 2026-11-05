use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use dispatch_mesh::{ai, engine::Workspace, model::*, runner};
use serde_json::json;
use std::path::PathBuf;

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
        let lease = runner::acquire(&cli.workspace)?;
        println!(
            "Mesh is running for {}. Ctrl+C saves local work and stops. Sessions close automatically after five minutes.
Drafts synchronize continuously as internal snapshots. Files appear on GitHub's {} branch after session finalization; use session finish to finalize sooner.",
            lease.root().display(),
            lease.branch()
        );
        let ctrl_c = async {
            let _ = tokio::signal::ctrl_c().await;
        };
        runner::run(lease, ctrl_c, None).await?;
        println!("Local changes saved. Mesh stopped.");
        return Ok(());
    }
    if matches!(cli.command, Command::Status) {
        return show_status(&Workspace::inspect(&cli.workspace)?);
    }
    let mut workspace = Workspace::open(&cli.workspace)?;
    match cli.command {
        Command::Sync => {
            drop(workspace);
            Workspace::sync_and_record(&cli.workspace)?;
            runner::summarize_ready(&cli.workspace).await?;
            Workspace::sync_and_record(&cli.workspace)?;
            show_status(&Workspace::open(&cli.workspace)?)?;
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
    println!(
        "{}",
        serde_json::to_string_pretty(&workspace.status_snapshot())?
    );
    Ok(())
}
