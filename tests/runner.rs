//! The in-process engine a desktop host embeds (US00002, ADR-0009). These are
//! plain `#[test]`s: the caller needs no Tokio runtime.

use dispatch_mesh::{
    engine::{Workspace, WorkspaceBusy},
    model::{Identity, ProviderConfig},
    runner::{AlreadyRunning, Runner, RunnerSnapshot, StatusReceiver},
};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

fn init(root: &Path) -> PathBuf {
    let workspace = Workspace::init(
        root,
        Identity {
            name: "Alice".into(),
            email: "alice@example.test".into(),
        },
        None,
        "main".into(),
    )
    .unwrap();
    workspace.root.clone()
}

fn mesh(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dispatch-mesh"))
        .arg("--workspace")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}

fn mesh_ok(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = mesh(root, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Open the workspace from the test while the runner also opens it every tick.
fn open(root: &Path) -> Workspace {
    let started = Instant::now();
    loop {
        match Workspace::open(root) {
            Ok(workspace) => return workspace,
            Err(error)
                if error.is::<WorkspaceBusy>() && started.elapsed() < Duration::from_secs(10) =>
            {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(error) => panic!("{error:#}"),
        }
    }
}

/// Wait until the latest published snapshot satisfies `accept`.
fn wait_for(
    status: &mut StatusReceiver,
    within: Duration,
    accept: impl Fn(&RunnerSnapshot) -> bool,
) -> RunnerSnapshot {
    let started = Instant::now();
    loop {
        if let Some(snapshot) = status.borrow_and_update().clone()
            && accept(&snapshot)
        {
            return snapshot;
        }
        assert!(
            started.elapsed() < within,
            "no matching snapshot within {within:?}; last: {:#?}",
            *status.borrow()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn has_file(snapshot: &RunnerSnapshot, path: &str) -> bool {
    snapshot
        .status
        .sessions
        .iter()
        .any(|s| s.files.iter().any(|f| f == path))
}

fn captured(root: &Path, path: &str, content: &str) -> bool {
    open(root).state.manifest.sessions.values().any(|s| {
        s.changes
            .get(path)
            .is_some_and(|c| c.content.as_deref() == Some(content.as_bytes()))
    })
}

#[test]
fn snapshots_follow_changes_from_other_processes_and_stay_quiet_otherwise() {
    let temp = tempfile::tempdir().unwrap();
    let root = init(temp.path());

    let started = Instant::now();
    let handle = Runner::start(&root).unwrap();
    assert!(started.elapsed() < Duration::from_secs(1), "start blocked");
    let mut status = handle.status();
    wait_for(&mut status, Duration::from_secs(2), |_| true);

    std::fs::write(root.join("a.md"), "first").unwrap();
    let snapshot = wait_for(&mut status, Duration::from_secs(10), |s| {
        has_file(s, "a.md") && s.status.active_session.is_some()
    });
    let session = snapshot.status.active_session.clone().unwrap();

    // Nothing changes: nothing is published. Let the first sync settle first.
    std::thread::sleep(Duration::from_secs(5));
    status.borrow_and_update();
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !status.has_changed().unwrap(),
        "published without a change: {:#?}",
        *status.borrow()
    );

    // CLI commands in another process work while the handle runs the workspace.
    mesh_ok(&root, &["status"]);
    let intent: serde_json::Value = serde_json::from_slice(&mesh_ok(
        &root,
        &[
            "intent",
            "begin",
            "--purpose",
            "Tidy notes",
            "--paths",
            "a.md",
        ],
    ))
    .unwrap();
    mesh_ok(
        &root,
        &["intent", "end", intent["intent_id"].as_str().unwrap()],
    );
    mesh_ok(&root, &["session", "finish"]);
    let finished = Instant::now();
    wait_for(&mut status, Duration::from_secs(2), |s| {
        s.status
            .sessions
            .iter()
            .any(|x| x.id == session && x.closed)
    });
    assert!(finished.elapsed() <= Duration::from_secs(2));

    handle.stop();
    handle.join().unwrap();
}

#[test]
fn stop_captures_saved_changes_before_the_loop_ends() {
    let temp = tempfile::tempdir().unwrap();
    let root = init(temp.path());
    let handle = Runner::start(&root).unwrap();
    let mut status = handle.status();
    std::fs::write(root.join("a.md"), "first").unwrap();
    wait_for(&mut status, Duration::from_secs(10), |s| {
        has_file(s, "a.md")
    });

    // Saved, but not yet stable across two scans when stop arrives.
    std::fs::write(root.join("b.md"), "saved just before stop").unwrap();
    handle.stop();
    handle.join().unwrap();
    assert!(captured(&root, "a.md", "first"));
    assert!(captured(&root, "b.md", "saved just before stop"));
}

#[test]
fn shutdown_capture_rides_out_a_briefly_held_operation_lock() {
    let temp = tempfile::tempdir().unwrap();
    let root = init(temp.path());
    let handle = Runner::start(&root).unwrap();
    let mut status = handle.status();
    wait_for(&mut status, Duration::from_secs(2), |_| true);

    std::fs::write(root.join("held.md"), "saved while locked").unwrap();
    let held = open(&root);
    handle.stop();
    // Longer than the lock settle time, so a single attempt would fail.
    std::thread::sleep(Duration::from_millis(1500));
    drop(held);
    handle.join().unwrap();
    assert!(captured(&root, "held.md", "saved while locked"));
}

#[test]
fn stop_does_not_wait_for_an_in_flight_sync() {
    let temp = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/repo.git", listener.local_addr().unwrap());
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request);
        started_tx.send(()).unwrap();
        let _ = release_rx.recv_timeout(Duration::from_secs(20));
        let _ = stream.write_all(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
    });
    let root = init(temp.path());
    {
        let mut workspace = open(&root);
        // Test-only HTTP transport to a loopback fake remote; no public endpoint.
        workspace.config.remote = Some("test-remote".into());
        workspace.save_config().unwrap();
        workspace
            .git
            .run(&["config", "remote.origin.url", &url], None)
            .unwrap();
    }

    let handle = Runner::start(&root).unwrap();
    started_rx
        .recv_timeout(Duration::from_secs(15))
        .expect("the runner contacts the remote");
    std::fs::write(root.join("offline.md"), "saved during a stalled sync").unwrap();
    let stopping = Instant::now();
    handle.stop();
    handle.join().unwrap();
    assert!(
        stopping.elapsed() < Duration::from_secs(5),
        "stop waited {:?} for the network",
        stopping.elapsed()
    );
    assert!(captured(&root, "offline.md", "saved during a stalled sync"));
    let _ = release_tx.send(());
    server.join().unwrap();
}

#[test]
fn summary_failures_reach_the_snapshot_without_the_response_body() {
    let temp = tempfile::tempdir().unwrap();
    let root = init(temp.path());
    let secret = "sk-runner-sensitive-summary-body";
    let serve = |status: u16, body: String| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0_u8; 4096];
                let count = stream.read(&mut chunk).unwrap();
                request.extend_from_slice(&chunk[..count]);
                if let Some(end) = request.windows(4).position(|b| b == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .map_or(0, |v| v.trim().parse().unwrap());
                    if request.len() >= end + 4 + length || count == 0 {
                        break;
                    }
                }
            }
            write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        (url, thread)
    };
    let use_provider = |base_url: String| {
        let mut workspace = open(&root);
        workspace.config.provider = Some(ProviderConfig {
            base_url,
            model: "test".into(),
            api_key_env: None,
            json_mode: false,
            send_text: true,
        });
        workspace.save_config().unwrap();
    };

    let (url, server) = serve(
        401,
        serde_json::json!({"error": {"message": secret}}).to_string(),
    );
    use_provider(url);
    let handle = Runner::start(&root).unwrap();
    let mut status = handle.status();
    std::fs::write(root.join("first.md"), "first").unwrap();
    wait_for(&mut status, Duration::from_secs(10), |s| {
        has_file(s, "first.md")
    });
    mesh_ok(&root, &["session", "finish"]);
    let failed = wait_for(&mut status, Duration::from_secs(20), |s| {
        s.status.summary_failure.is_some()
    });
    server.join().unwrap();
    let failure = failed.status.summary_failure.clone().unwrap();
    assert!(failure.contains("HTTP 401"), "{failure}");
    assert!(!serde_json::to_string(&failed).unwrap().contains(secret));
    assert_eq!(failed.status.reason.as_deref(), Some(failure.as_str()));

    // The next successful summary clears it, as the runner observes.
    let body = serde_json::json!({"choices": [{"message": {"content":
        r#"{"title":"Add second note","description":"Adds second.md."}"#}}]});
    let (url, server) = serve(200, body.to_string());
    use_provider(url);
    std::fs::write(root.join("second.md"), "second").unwrap();
    wait_for(&mut status, Duration::from_secs(10), |s| {
        has_file(s, "second.md")
    });
    mesh_ok(&root, &["session", "finish"]);
    wait_for(&mut status, Duration::from_secs(20), |s| {
        s.status.summary_failure.is_none()
    });
    server.join().unwrap();
    drop(handle);
}

#[test]
fn a_held_runner_lock_is_refused_until_it_is_released() {
    let temp = tempfile::tempdir().unwrap();
    let root = init(temp.path());

    let first = Runner::start(&root).unwrap();
    let error = Runner::start(&root)
        .err()
        .expect("a second runner is refused");
    assert!(error.is::<AlreadyRunning>(), "{error:#}");
    let cli = mesh(&root, &["run"]);
    assert!(!cli.status.success());
    assert!(
        String::from_utf8_lossy(&cli.stderr).contains("already running"),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    first.stop();
    first.join().unwrap();
    drop(Runner::start(&root).unwrap());

    // A CLI runner holds the lock just as a handle does.
    let mut child = Command::new(env!("CARGO_BIN_EXE_dispatch-mesh"))
        .arg("--workspace")
        .arg(&root)
        .arg("run")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut banner = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut banner)
        .unwrap();
    assert!(banner.starts_with("Mesh is running"), "{banner}");
    let error = Runner::start(&root)
        .err()
        .expect("the CLI runner holds the lock");
    assert!(error.is::<AlreadyRunning>(), "{error:#}");
    let _ = child.kill();
    let _ = child.wait();
}
