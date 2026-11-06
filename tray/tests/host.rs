//! The tray host with real workspaces and no window (US00001).

use dispatch_mesh::{
    engine::{Workspace as Engine, WorkspaceBusy},
    model::Identity,
    runner,
};
use dispatch_mesh_tray::{
    host::Host,
    model::{Workspace, WorkspaceState},
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, mpsc},
    time::{Duration, Instant},
};

fn init(root: &Path) -> PathBuf {
    Engine::init(
        root,
        Identity {
            name: "Alice".into(),
            email: "alice@example.test".into(),
        },
        None,
        "main".into(),
    )
    .unwrap()
    .root
    .clone()
}

fn workspace(parent: &tempfile::TempDir, name: &str) -> PathBuf {
    let root = parent.path().join(name);
    std::fs::create_dir(&root).unwrap();
    init(&root)
}

/// Every list of workspaces the listener received.
type Heard = Arc<Mutex<Vec<Vec<Workspace>>>>;

/// A host whose listener keeps every notification.
fn host(paths: &[PathBuf]) -> (Arc<Host>, Heard) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let host = Host::new(paths.iter().cloned(), move |workspaces| {
        sink.lock().unwrap().push(workspaces)
    })
    .unwrap();
    (host, seen)
}

fn wait_until(host: &Host, what: &str, accept: impl Fn(&[Workspace]) -> bool) -> Vec<Workspace> {
    let started = Instant::now();
    loop {
        let workspaces = host.workspaces();
        if accept(&workspaces) {
            return workspaces;
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "{what}: {workspaces:#?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn published(workspace: &Workspace) -> bool {
    matches!(workspace.state, WorkspaceState::Running(Some(_)))
}

fn captured(root: &Path, path: &str, content: &str) -> bool {
    let engine = loop {
        match Engine::open(root) {
            Ok(engine) => break engine,
            Err(error) if error.is::<WorkspaceBusy>() => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(error) => panic!("{error:#}"),
        }
    };
    engine.state.manifest.sessions.values().any(|session| {
        session
            .changes
            .get(path)
            .is_some_and(|change| change.content.as_deref() == Some(content.as_bytes()))
    })
}

#[test]
fn several_workspaces_run_side_by_side_and_publish() {
    let parent = tempfile::tempdir().unwrap();
    let wiki = workspace(&parent, "Team Wiki");
    let notes = workspace(&parent, "Research notes");
    // The same folder twice is one workspace, not a refused second engine.
    let (host, seen) = host(&[wiki.clone(), notes.clone(), wiki.clone()]);
    host.start_all();
    let workspaces = wait_until(&host, "both publish", |w| w.iter().all(published));
    let names: Vec<_> = workspaces.iter().map(|w| w.name.as_str()).collect();
    assert_eq!(names, ["Team Wiki", "Research notes"]);
    assert_eq!(workspaces[0].path, wiki);
    assert!(
        !seen.lock().unwrap().is_empty(),
        "the listener heard the changes"
    );

    std::fs::write(notes.join("note.md"), "hello").unwrap();
    wait_until(&host, "the edit reaches the snapshot", |w| {
        matches!(&w[1].state, WorkspaceState::Running(Some(s))
            if s.status.sessions.iter().any(|session| session.files.iter().any(|f| f == "note.md")))
    });
    host.quit();
}

#[test]
fn a_held_workspace_is_refused_while_the_others_run_and_retry_starts_it_later() {
    let parent = tempfile::tempdir().unwrap();
    let held = workspace(&parent, "Held");
    let free = workspace(&parent, "Free");
    let lease = runner::acquire(&held).unwrap();
    let (host, _) = host(&[held.clone(), free]);
    host.start_all();
    let workspaces = wait_until(&host, "held refused, free runs", |w| {
        w[0].state == WorkspaceState::AlreadyRunning && published(&w[1])
    });
    assert_eq!(workspaces[0].name, "Held");

    host.retry(0);
    assert_eq!(
        host.workspaces()[0].state,
        WorkspaceState::AlreadyRunning,
        "still held"
    );

    drop(lease);
    host.retry(0);
    wait_until(&host, "retry starts it once released", |w| published(&w[0]));
    // Retrying a running workspace is ignored: no second engine, no refusal.
    host.retry(0);
    assert!(published(&host.workspaces()[0]));
    host.quit();
}

#[test]
fn a_second_host_is_refused_per_workspace() {
    let parent = tempfile::tempdir().unwrap();
    let wiki = workspace(&parent, "Team Wiki");
    let (first, _) = host(std::slice::from_ref(&wiki));
    first.start_all();
    wait_until(&first, "first host runs", |w| published(&w[0]));
    let (second, _) = host(&[wiki]);
    second.start_all();
    wait_until(&second, "second host refused", |w| {
        w[0].state == WorkspaceState::AlreadyRunning
    });
    first.quit();
    second.retry(0);
    wait_until(
        &second,
        "second host takes over after the first quits",
        |w| published(&w[0]),
    );
    second.quit();
}

#[test]
fn quit_captures_an_edit_saved_just_before() {
    let parent = tempfile::tempdir().unwrap();
    let wiki = workspace(&parent, "Team Wiki");
    let (host, _) = host(std::slice::from_ref(&wiki));
    host.start_all();
    wait_until(&host, "runs", |w| published(&w[0]));
    std::fs::write(wiki.join("last.md"), "saved before quit").unwrap();
    host.quit();
    assert!(captured(&wiki, "last.md", "saved before quit"));
    // The runner lock is released: a CLI `run` or another host could start now.
    drop(runner::acquire(&wiki).unwrap());
}

#[test]
fn a_folder_that_is_not_a_workspace_is_stopped_with_a_reason_and_can_be_retried() {
    let parent = tempfile::tempdir().unwrap();
    let plain = parent.path().join("Plain");
    std::fs::create_dir(&plain).unwrap();
    let (host, _) = host(std::slice::from_ref(&plain));
    host.start_all();
    let workspaces = wait_until(&host, "stopped", |w| {
        matches!(w[0].state, WorkspaceState::Stopped(_))
    });
    let WorkspaceState::Stopped(reason) = &workspaces[0].state else {
        unreachable!()
    };
    assert!(
        reason.contains("not a Mesh workspace") && reason.contains("init"),
        "{reason}"
    );

    init(&plain);
    host.retry(0);
    wait_until(&host, "runs after init", |w| published(&w[0]));
    host.quit();
}

#[test]
fn a_host_without_workspaces_still_reports_once() {
    let (host, seen) = host(&[]);
    host.start_all();
    assert_eq!(seen.lock().unwrap().as_slice(), [Vec::<Workspace>::new()]);
}

#[test]
fn quit_waits_for_a_start_in_flight_and_its_final_capture() {
    let parent = tempfile::tempdir().unwrap();
    let wiki = workspace(&parent, "Team Wiki");
    std::fs::write(wiki.join("early.md"), "saved while starting").unwrap();

    // Hold the start in flight: the listener blocks on the "Starting" report
    // until the test opens the gate, like a slow UI callback.
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (reached_tx, reached) = mpsc::channel();
    let reached_tx = Mutex::new(Some(reached_tx));
    let listener_gate = Arc::clone(&gate);
    let host = Host::new([wiki.clone()], move |workspaces| {
        if workspaces
            .iter()
            .any(|w| w.state == WorkspaceState::Starting)
        {
            if let Some(tx) = reached_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            let (open, opened) = &*listener_gate;
            let _open = opened
                .wait_while(open.lock().unwrap(), |open| !*open)
                .unwrap();
        }
    })
    .unwrap();
    let starter = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || host.start_all())
    };
    reached.recv_timeout(Duration::from_secs(10)).unwrap();

    let (done_tx, done) = mpsc::channel();
    let quitter = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || {
            host.quit();
            done_tx.send(()).unwrap();
        })
    };
    assert!(
        done.recv_timeout(Duration::from_millis(500)).is_err(),
        "Quit returned while a workspace was still starting"
    );

    let (open, opened) = &*gate;
    *open.lock().unwrap() = true;
    opened.notify_all();
    done.recv_timeout(Duration::from_secs(30))
        .expect("Quit returns once the start has finished");
    // By then the late engine has captured the edit and released the workspace.
    assert!(captured(&wiki, "early.md", "saved while starting"));
    drop(runner::acquire(&wiki).unwrap());
    quitter.join().unwrap();
    starter.join().unwrap();

    // After Quit, no new engine starts.
    host.start_all();
    drop(runner::acquire(&wiki).unwrap());
}

fn status(workspace: &Workspace) -> Option<&dispatch_mesh::status::Status> {
    match &workspace.state {
        WorkspaceState::Running(Some(snapshot)) => Some(&snapshot.status),
        _ => None,
    }
}

#[test]
fn finish_closes_the_open_session_like_the_cli() {
    use dispatch_mesh::status::Stage;
    let parent = tempfile::tempdir().unwrap();
    let wiki = workspace(&parent, "Team Wiki");
    let (host, _) = host(std::slice::from_ref(&wiki));
    host.start_all();
    wait_until(&host, "runs", |w| published(&w[0]));
    assert_eq!(host.finish(0).unwrap(), None, "no session yet");

    std::fs::write(wiki.join("note.md"), "draft").unwrap();
    let workspaces = wait_until(&host, "a session opens", |w| {
        status(&w[0]).is_some_and(|s| s.active_session.is_some())
    });
    let open = status(&workspaces[0]).unwrap().active_session.clone();
    let finished = host.finish(0).unwrap();
    assert_eq!(finished, open);
    let id = finished.unwrap();
    wait_until(&host, "the snapshot shows it finishing up", |w| {
        status(&w[0]).is_some_and(|s| {
            s.active_session.is_none()
                && s.sessions.iter().any(|session| {
                    session.id == id
                        && session.closed
                        && matches!(
                            session.stage,
                            Stage::WaitingForFinalization | Stage::Published
                        )
                })
        })
    });
    host.quit();
}

#[test]
fn finish_is_refused_where_the_engine_does_not_run_here() {
    let parent = tempfile::tempdir().unwrap();
    let held = workspace(&parent, "Held");
    let _lease = runner::acquire(&held).unwrap();
    let (host, _) = host(std::slice::from_ref(&held));
    host.start_all();
    wait_until(&host, "refused", |w| {
        w[0].state == WorkspaceState::AlreadyRunning
    });
    assert!(host.finish(0).is_err());
    assert!(host.finish(7).is_err(), "unknown workspace");
    host.quit();
}

#[test]
fn saved_copies_resolve_only_from_ids_the_status_lists() {
    let parent = tempfile::tempdir().unwrap();
    let wiki = workspace(&parent, "Team Wiki");
    {
        // The next open displaces note.md to write the incoming version.
        let mut engine = Engine::open(&wiki).unwrap();
        std::fs::write(wiki.join("note.md"), "before").unwrap();
        engine.capture_at(dispatch_mesh::model::now()).unwrap();
        engine.state.applying = Some(
            [("note.md".to_owned(), b"incoming".to_vec())]
                .into_iter()
                .collect(),
        );
        engine.save().unwrap();
    }
    let (host, _) = host(std::slice::from_ref(&wiki));
    host.start_all();
    let workspaces = wait_until(&host, "the copy is listed", |w| {
        status(&w[0]).is_some_and(|s| !s.saved_copies.is_empty())
    });
    let copy = status(&workspaces[0]).unwrap().saved_copies[0].clone();
    assert_eq!(copy.path, "note.md");

    let location = host.saved_copy_location(0, &copy.id).unwrap();
    assert_eq!(std::fs::read(&location).unwrap(), b"before");
    assert!(location.starts_with(wiki.join(".mesh").join("recovery")));

    let unknown = "00000000-0000-4000-8000-000000000000";
    for refused in [
        unknown,
        "../../note.md",
        r"C:\Windows",
        &format!("{}/../x", copy.id),
        "",
    ] {
        assert_eq!(host.saved_copy_location(0, refused), None, "{refused}");
    }
    assert_eq!(
        host.saved_copy_location(3, &copy.id),
        None,
        "unknown workspace"
    );
    host.quit();
}
