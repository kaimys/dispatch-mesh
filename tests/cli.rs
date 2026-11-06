use dispatch_mesh::engine::Workspace;
use std::{
    fs,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Runner(Child);
impl Drop for Runner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn intent_begin_purpose_appears_in_final_commit_without_an_ai_provider() {
    let temp = tempfile::tempdir().unwrap();
    let mesh = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_dispatch-mesh"))
            .arg("--workspace")
            .arg(temp.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    mesh(&["init", "--name", "Alice", "--email", "alice@example.test"]);
    mesh(&[
        "intent",
        "begin",
        "--purpose",
        "Another little test",
        "--paths",
        "little.md",
    ]);
    fs::write(temp.path().join("little.md"), "test content").unwrap();
    fs::write(temp.path().join("unrelated.md"), "separate edit").unwrap();
    mesh(&["session", "finish"]);
    mesh(&["sync"]);
    let workspace = Workspace::inspect(temp.path()).unwrap();
    let message = workspace
        .git
        .text(&["log", "-1", "--format=%B", &workspace.state.main])
        .unwrap();
    assert!(message.contains("Files: little.md\nPurpose: Another little test"));
    assert!(!message.contains("Files: unrelated.md"));
    assert!(workspace.config.provider.is_none());
}

#[test]
fn editing_a_published_plan_is_reported_as_modification_in_metadata_only_mode() {
    let temp = tempfile::tempdir().unwrap();
    let mesh = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_dispatch-mesh"))
            .arg("--workspace")
            .arg(temp.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    mesh(&["init", "--name", "Alice", "--email", "alice@example.test"]);
    mesh(&[
        "provider",
        "set",
        "--base-url",
        "http://127.0.0.1:1/v1",
        "--model",
        "unused-metadata-mode",
    ]);
    fs::write(temp.path().join("plan.md"), "Existing milestones\n").unwrap();
    mesh(&["session", "finish"]);
    mesh(&["sync"]);
    fs::write(
        temp.path().join("plan.md"),
        "Existing milestones\n0.7 macOS beta\n",
    )
    .unwrap();
    mesh(&["session", "finish"]);
    mesh(&["sync"]);
    let workspace = Workspace::inspect(temp.path()).unwrap();
    let message = workspace
        .git
        .text(&["log", "-1", "--format=%B", &workspace.state.main])
        .unwrap();
    assert!(message.starts_with("Update plan.md\n"));
    assert!(message.contains("Modified: plan.md"));
    assert!(!message.contains("Added: plan.md"));
    assert_eq!(
        workspace
            .git
            .text(&[
                "diff-tree",
                "--no-commit-id",
                "--name-status",
                "-r",
                &workspace.state.main
            ])
            .unwrap(),
        "M\tplan.md"
    );
}

#[test]
fn running_process_captures_saved_edits_and_restart_recovers_them() {
    let temp = tempfile::tempdir().unwrap();
    let executable = env!("CARGO_BIN_EXE_dispatch-mesh");
    let output = Command::new(executable)
        .arg("--workspace")
        .arg(temp.path())
        .args(["init", "--name", "Alice", "--email", "alice@example.test"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut command = Command::new(executable);
    command
        .arg("--workspace")
        .arg(temp.path())
        .arg("run")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let runner = Runner(command.spawn().unwrap());
    fs::write(temp.path().join("note.md"), "saved while running").unwrap();
    let start = Instant::now();
    loop {
        if let Ok(workspace) = Workspace::open(temp.path())
            && workspace
                .state
                .observed
                .get("note.md")
                .is_some_and(|bytes| bytes == b"saved while running")
        {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "Watcher did not persist the saved file"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(runner); // Abrupt process stop exercises SQLite recovery, not a final sync.
    let workspace = Workspace::open(temp.path()).unwrap();
    assert_eq!(workspace.state.observed["note.md"], b"saved while running");
    assert!(workspace.active_session().is_some());
}

#[test]
fn runner_startup_survives_a_concurrent_workspace_operation() {
    use dispatch_mesh::model::Identity;
    let temp = tempfile::tempdir().unwrap();
    let workspace = Workspace::init(
        temp.path(),
        Identity {
            name: "Alice".into(),
            email: "alice@example.test".into(),
        },
        None,
        "main".into(),
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_dispatch-mesh"));
    command
        .arg("--workspace")
        .arg(temp.path())
        .arg("run")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut runner = Runner(command.spawn().unwrap());
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        runner.0.try_wait().unwrap().is_none(),
        "Runner exited because another operation briefly held the workspace lock"
    );
    fs::write(
        temp.path().join("after-startup.md"),
        "captured after contention",
    )
    .unwrap();
    drop(workspace);
    let started = Instant::now();
    loop {
        let state = Workspace::inspect(temp.path()).unwrap().state;
        if state
            .observed
            .get("after-startup.md")
            .is_some_and(|bytes| bytes == b"captured after contention")
        {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "Runner did not retry capture after startup contention"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn status_remains_readable_while_an_operation_holds_the_workspace_lock() {
    use dispatch_mesh::model::Identity;
    let temp = tempfile::tempdir().unwrap();
    let _workspace = Workspace::init(
        temp.path(),
        Identity {
            name: "Alice".into(),
            email: "alice@example.test".into(),
        },
        None,
        "main".into(),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dispatch-mesh"))
        .arg("--workspace")
        .arg(temp.path())
        .arg("status")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let state: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(state["capture_error"].is_null());
    assert_eq!(state["status"], "synced");
}

#[test]
fn slow_remote_does_not_block_capture_or_overwrite_concurrent_saved_state() {
    use dispatch_mesh::model::Identity;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
    };
    let temp = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/repo.git", listener.local_addr().unwrap());
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request);
        started_tx.send(()).unwrap();
        let _ = release_rx.recv_timeout(Duration::from_secs(12));
        let _ = stream.write_all(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
    });
    let mut workspace = Workspace::init(
        temp.path(),
        Identity {
            name: "Alice".into(),
            email: "alice@example.test".into(),
        },
        None,
        "main".into(),
    )
    .unwrap();
    // Test-only HTTP transport to a loopback fake remote; no public endpoint.
    workspace.config.remote = Some("test-remote".into());
    workspace.save_config().unwrap();
    workspace
        .git
        .run(&["config", "remote.origin.url", &url], None)
        .unwrap();
    drop(workspace);
    let mut command = Command::new(env!("CARGO_BIN_EXE_dispatch-mesh"));
    command
        .arg("--workspace")
        .arg(temp.path())
        .arg("run")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let runner = Runner(command.spawn().unwrap());
    started_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("Remote request did not start");
    fs::write(
        temp.path().join("during-network.md"),
        "saved while the remote is stalled",
    )
    .unwrap();
    let started = Instant::now();
    loop {
        let workspace = Workspace::inspect(temp.path()).unwrap();
        if workspace
            .state
            .observed
            .get("during-network.md")
            .is_some_and(|v| v == b"saved while the remote is stalled")
        {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "Capture waited for the stalled network request"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    release_tx.send(()).unwrap();
    server.join().unwrap();
    let started = Instant::now();
    loop {
        let workspace = Workspace::inspect(temp.path()).unwrap();
        if workspace.state.attention.is_some() {
            assert_eq!(
                workspace.state.observed["during-network.md"],
                b"saved while the remote is stalled"
            );
            assert!(
                workspace
                    .active_session()
                    .unwrap()
                    .changes
                    .contains_key("during-network.md")
            );
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "Network failure was not recorded"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(runner);
}

/// Run the binary with an isolated Git configuration, so no system or user
/// credential helper (e.g. Git Credential Manager) answers an authentication
/// challenge, plus any extra environment such as a non-English locale.
fn mesh_isolated(
    workspace: &std::path::Path,
    git_config: &std::path::Path,
    env: &[(&str, &str)],
    args: &[&str],
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dispatch-mesh"));
    command
        .arg("--workspace")
        .arg(workspace)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", git_config);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().unwrap()
}

/// An HTTP remote that refuses every request with 401, for as long as the test runs.
fn unauthorized_remote() -> String {
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/repo.git", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => request.extend_from_slice(&chunk[..count]),
                }
            }
            let _ = stream.write_all(
                b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"mesh-test\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    url
}

/// A URL on which nothing listens: connecting is refused immediately.
fn unreachable_remote() -> String {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    format!("http://127.0.0.1:{port}/repo.git")
}

/// Init a workspace on a loopback `remote`, save a file, sync (which must fail)
/// and return `status`.
fn status_after_failed_sync(remote: &str, env: &[(&str, &str)]) -> serde_json::Value {
    use dispatch_mesh::model::Identity;
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("work");
    let git_config = temp.path().join("empty.gitconfig");
    fs::write(&git_config, "").unwrap();
    let mut mesh = Workspace::init(
        &workspace,
        Identity {
            name: "Alice".into(),
            email: "alice@example.test".into(),
        },
        None,
        "main".into(),
    )
    .unwrap();
    // Test-only HTTP transport to a loopback fake remote, as in the slow-remote
    // test: Mesh accepts only HTTPS, SSH and local remotes from users.
    mesh.config.remote = Some("test-remote".into());
    mesh.save_config().unwrap();
    mesh.git
        .run(&["config", "remote.origin.url", remote], None)
        .unwrap();
    drop(mesh);
    fs::write(workspace.join("note.md"), "saved while the remote fails").unwrap();
    let sync = mesh_isolated(&workspace, &git_config, env, &["sync"]);
    assert!(!sync.status.success(), "sync must fail against {remote}");
    let status = mesh_isolated(&workspace, &git_config, env, &["status"]);
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let state: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    // Capture continues regardless of how the remote failed.
    assert!(
        state["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["files"] == serde_json::json!(["note.md"])),
        "{state:#}"
    );
    state
}

fn assert_offline(state: &serde_json::Value) {
    assert_eq!(state["status"], "offline", "{state:#}");
    assert!(state["attention"].is_null(), "{state:#}");
    let reason = state["reason"].as_str().unwrap();
    assert!(reason.contains("Remote unreachable"), "{reason}");
}

fn assert_access_needs_attention(state: &serde_json::Value) {
    assert_eq!(state["status"], "needs_attention", "{state:#}");
    assert!(state["offline"].is_null(), "{state:#}");
    let reason = state["reason"].as_str().unwrap();
    assert!(reason.contains("Remote refused access"), "{reason}");
}

const GERMAN: &[(&str, &str)] = &[
    ("LANG", "de_DE.UTF-8"),
    ("LC_ALL", "de_DE.UTF-8"),
    ("LC_MESSAGES", "de_DE.UTF-8"),
    ("LANGUAGE", "de"),
];

#[test]
fn an_unreachable_remote_reports_offline_and_keeps_capturing() {
    assert_offline(&status_after_failed_sync(&unreachable_remote(), &[]));
}

#[test]
fn a_refused_login_needs_attention_instead_of_offline() {
    assert_access_needs_attention(&status_after_failed_sync(&unauthorized_remote(), &[]));
}

#[test]
fn failure_classification_does_not_depend_on_the_user_locale() {
    assert_offline(&status_after_failed_sync(&unreachable_remote(), GERMAN));
    assert_access_needs_attention(&status_after_failed_sync(&unauthorized_remote(), GERMAN));
}

#[test]
fn init_in_an_obsidian_vault_excludes_only_the_per_device_layout() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".obsidian")).unwrap();
    fs::write(temp.path().join(".obsidian/workspace.json"), "{}").unwrap();
    fs::write(temp.path().join(".obsidian/app.json"), "{}").unwrap();
    fs::write(temp.path().join("note.md"), "note").unwrap();
    let mesh = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_dispatch-mesh"))
            .arg("--workspace")
            .arg(temp.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    mesh(&["init", "--name", "Alice", "--email", "alice@example.test"]);
    mesh(&["session", "finish"]);
    mesh(&["sync"]);
    let status: serde_json::Value = serde_json::from_slice(&mesh(&["status"]).stdout).unwrap();
    assert!(status["capture_error"].is_null(), "{status}");
    assert_eq!(status["status"], "synced", "{status}");
    assert_eq!(
        fs::read_to_string(temp.path().join(".meshignore")).unwrap(),
        ".obsidian/workspace.json\n.obsidian/workspace-mobile.json\n"
    );
    let workspace = Workspace::inspect(temp.path()).unwrap();
    let shared = workspace.git.files(&workspace.state.main).unwrap();
    assert!(shared.contains_key(".meshignore"));
    assert!(shared.contains_key(".obsidian/app.json"));
    assert!(!shared.contains_key(".obsidian/workspace.json"));
}
