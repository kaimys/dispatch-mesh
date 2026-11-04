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
