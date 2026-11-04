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
