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

// ---- Conflict review (US00006) ----------------------------------------

mod conflicts {
    use super::*;
    use dispatch_mesh::{git::Git, model::now};
    use dispatch_mesh_tray::model::conflicts::{Body, Choice};

    const BASE: &str = "one\ntwo\nthree\n";
    const PNG: [u8; 12] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];

    fn person(name: &str) -> Identity {
        Identity {
            name: name.into(),
            email: format!("{}@example.test", name.to_lowercase()),
        }
    }

    fn client(parent: &Path, name: &str, origin: &Path) -> Engine {
        std::fs::create_dir_all(parent.join(name)).unwrap();
        Engine::init(
            &parent.join(name),
            person(name),
            Some(origin.to_string_lossy().into_owned()),
            "main".into(),
        )
        .unwrap()
    }

    /// Publish the closed session, as the runner would over later cycles.
    fn finish(workspace: &mut Engine) {
        let id = workspace.finish().unwrap();
        workspace.sync().unwrap();
        if let Some(id) = id
            && let Some(input) = workspace.summary_input(&id).unwrap()
        {
            let session = workspace.state.manifest.sessions.get_mut(&id).unwrap();
            session.message = Some(dispatch_mesh::ai::fallback(&input));
            session.message_base = input.message_base;
            workspace.save().unwrap();
        }
        workspace.sync().unwrap();
    }

    fn origin(parent: &Path) -> PathBuf {
        let origin = parent.join("remote.git");
        Git {
            dir: origin.clone(),
        }
        .init()
        .unwrap();
        origin
    }

    /// Alice and Bob start from `files`, then each changes some of them
    /// (`None` deletes). Bob saves first, so his versions are the current
    /// ones. Returns Bob's folder, with the conflicts recorded and no engine
    /// running.
    fn collide(
        parent: &Path,
        files: &[(&str, &[u8])],
        alice_edits: &[(&str, Option<&[u8]>)],
        bob_edits: &[(&str, Option<&[u8]>)],
    ) -> PathBuf {
        let origin = origin(parent);
        let mut alice = client(parent, "Alice", &origin);
        for (path, bytes) in files {
            std::fs::write(alice.root.join(path), bytes).unwrap();
        }
        finish(&mut alice);
        let mut bob = client(parent, "Bob", &origin);
        bob.sync().unwrap();
        let edit = |engine: &mut Engine, edits: &[(&str, Option<&[u8]>)], at: u64| {
            for (path, bytes) in edits {
                match bytes {
                    Some(bytes) => std::fs::write(engine.root.join(path), bytes).unwrap(),
                    None => std::fs::remove_file(engine.root.join(path)).unwrap(),
                }
            }
            engine.capture_at(at).unwrap();
        };
        let at = now();
        edit(&mut bob, bob_edits, at);
        edit(&mut alice, alice_edits, at + 5);
        alice.sync().unwrap();
        bob.sync().unwrap();
        assert!(bob.unresolved().count() > 0, "Bob holds the conflicts");
        bob.root.clone()
    }

    fn text_conflict(parent: &Path) -> PathBuf {
        collide(
            parent,
            &[("note.md", BASE.as_bytes())],
            &[("note.md", Some(b"ALICE\ntwo\nthree\n"))],
            &[("note.md", Some(b"BOB\ntwo\nthree\n"))],
        )
    }

    /// Wait until the host reports `count` unresolved conflicts for workspace 0.
    fn listed(host: &Host, count: usize) -> dispatch_mesh::status::Status {
        let workspaces = wait_until(host, "the conflicts are listed", |w| {
            status(&w[0]).is_some_and(|s| s.unresolved_conflicts.len() == count)
        });
        status(&workspaces[0]).unwrap().clone()
    }

    fn id_of(status: &dispatch_mesh::status::Status, path: &str) -> String {
        status
            .unresolved_conflicts
            .iter()
            .find(|c| c.path == path)
            .unwrap_or_else(|| panic!("{path}: {:?}", status.unresolved_conflicts))
            .id
            .clone()
    }

    fn resolutions(root: &Path) -> Vec<Option<Vec<u8>>> {
        let engine = Engine::inspect(root).unwrap();
        engine
            .state
            .manifest
            .resolutions
            .values()
            .map(|r| r.selected.clone())
            .collect()
    }

    #[test]
    fn resolving_from_the_host_does_what_the_engine_call_does_while_the_runner_runs() {
        for choice in [Choice::Current, Choice::Other] {
            let parent = tempfile::tempdir().unwrap();
            let via_host = text_conflict(&parent.path().join("host"));
            let direct = text_conflict(&parent.path().join("direct"));
            {
                let mut engine = Engine::open(&direct).unwrap();
                let id = engine.unresolved().next().unwrap().id.clone();
                engine.resolve(&id, choice.incoming()).unwrap();
            }

            let (host, _) = host(std::slice::from_ref(&via_host));
            host.start_all();
            let status = listed(&host, 1);
            let id = id_of(&status, "note.md");
            let resolved = host.resolve_conflict(0, &id, choice).unwrap();
            assert!(resolved.confirmation.starts_with(match choice {
                Choice::Current => "note.md: kept the current version.",
                Choice::Other => "note.md: used the other version.",
            }));
            assert_eq!(resolved.next, None, "no other conflict is left");
            listed(&host, 0);
            host.quit();

            let expected = match choice {
                Choice::Current => "BOB\ntwo\nthree\n",
                Choice::Other => "ALICE\ntwo\nthree\n",
            };
            assert_eq!(
                std::fs::read_to_string(via_host.join("note.md")).unwrap(),
                expected
            );
            assert_eq!(
                std::fs::read(via_host.join("note.md")).unwrap(),
                std::fs::read(direct.join("note.md")).unwrap()
            );
            assert_eq!(resolutions(&via_host), resolutions(&direct));
            assert_eq!(resolutions(&via_host), [Some(expected.as_bytes().to_vec())]);
        }
    }

    #[test]
    fn resolving_names_the_next_conflict_in_list_order() {
        let parent = tempfile::tempdir().unwrap();
        let root = collide(
            parent.path(),
            &[("a.md", BASE.as_bytes()), ("b.md", BASE.as_bytes())],
            &[
                ("a.md", Some(b"ALICE\ntwo\nthree\n")),
                ("b.md", Some(b"ALICE\ntwo\nthree\n")),
            ],
            &[
                ("a.md", Some(b"BOB\ntwo\nthree\n")),
                ("b.md", Some(b"BOB\ntwo\nthree\n")),
            ],
        );
        let (host, _) = host(std::slice::from_ref(&root));
        host.start_all();
        let status = listed(&host, 2);
        let ordered = dispatch_mesh_tray::model::conflicts::ordered(&status.unresolved_conflicts);
        let (first, second) = (ordered[0].id.clone(), ordered[1].id.clone());
        let detail = host.conflict_detail(0, &first).unwrap();
        assert_eq!(detail.position, "1 of 2");
        let resolved = host.resolve_conflict(0, &first, Choice::Current).unwrap();
        assert_eq!(resolved.next.as_deref(), Some(second.as_str()));
        listed(&host, 1);
        assert_eq!(host.conflict_detail(0, &second).unwrap().position, "1 of 1");
        host.quit();
    }

    #[test]
    fn a_later_save_is_noted_and_the_engine_refuses_the_choice_without_recording_it() {
        let parent = tempfile::tempdir().unwrap();
        let root = text_conflict(parent.path());
        let (host, _) = host(std::slice::from_ref(&root));
        host.start_all();
        let id = id_of(&listed(&host, 1), "note.md");
        assert!(host.conflict_detail(0, &id).unwrap().note.is_none());
        assert_eq!(host.conflict_note(0, &id).unwrap(), None);

        std::fs::write(root.join("note.md"), "BOB again\ntwo\nthree\n").unwrap();
        let started = Instant::now();
        let view = loop {
            let view = host.conflict_detail(0, &id).unwrap();
            if view.note.is_some() {
                break view;
            }
            assert!(
                started.elapsed() < Duration::from_secs(15),
                "the save was not captured"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(!view.can_choose);
        assert_eq!(host.conflict_note(0, &id).unwrap(), view.note);
        let error = host.resolve_conflict(0, &id, Choice::Other).unwrap_err();
        assert!(format!("{error:#}").contains("newer edits"), "{error:#}");
        host.quit();
        assert!(resolutions(&root).is_empty(), "nothing was recorded");
        assert_eq!(
            std::fs::read_to_string(root.join("note.md")).unwrap(),
            "BOB again\ntwo\nthree\n",
            "the later edit is untouched"
        );
    }

    #[test]
    fn review_is_refused_where_the_engine_does_not_run_here_or_the_conflict_is_unknown() {
        let parent = tempfile::tempdir().unwrap();
        let root = text_conflict(parent.path());
        let id = Engine::inspect(&root)
            .unwrap()
            .unresolved()
            .next()
            .unwrap()
            .id
            .clone();
        {
            let lease = runner::acquire(&root).unwrap();
            let (host, _) = host(std::slice::from_ref(&root));
            host.start_all();
            wait_until(&host, "refused", |w| {
                w[0].state == WorkspaceState::AlreadyRunning
            });
            assert!(host.conflict_detail(0, &id).is_err());
            assert!(host.conflict_note(0, &id).is_err());
            assert!(host.conflict_preview(0, &id, "current").is_err());
            assert!(host.resolve_conflict(0, &id, Choice::Current).is_err());
            host.quit();
            drop(lease);
        }

        let (host, _) = host(std::slice::from_ref(&root));
        host.start_all();
        listed(&host, 1);
        assert!(host.conflict_detail(0, "no-such-conflict").is_err());
        assert!(host.conflict_note(0, "no-such-conflict").is_err());
        assert!(
            host.conflict_preview(0, "no-such-conflict", "current")
                .is_err()
        );
        assert!(
            host.resolve_conflict(0, "no-such-conflict", Choice::Current)
                .is_err()
        );
        assert!(host.conflict_detail(9, &id).is_err(), "unknown workspace");
        assert!(
            host.conflict_preview(0, &id, "../../note.md").is_err(),
            "unknown side"
        );
        host.quit();
        assert!(resolutions(&root).is_empty());
    }

    #[test]
    fn each_kind_of_conflict_gets_its_own_detail_view() {
        let parent = tempfile::tempdir().unwrap();
        let large_current = vec![b'a'; 1024 * 1024 + 1];
        let large_other = vec![b'b'; 1024 * 1024 + 1];
        let mut png_alice = PNG.to_vec();
        png_alice.push(1);
        let mut png_bob = PNG.to_vec();
        png_bob.push(2);
        let root = collide(
            parent.path(),
            &[
                ("note.md", BASE.as_bytes()),
                ("gone.md", BASE.as_bytes()),
                ("big.md", b"base\n"),
                ("logo.png", &PNG),
            ],
            &[
                ("note.md", Some(b"ALICE\ntwo\nthree\n")),
                ("gone.md", None),
                ("big.md", Some(&large_other)),
                ("logo.png", Some(&png_alice)),
            ],
            &[
                ("note.md", Some(b"BOB\ntwo\nthree\n")),
                ("gone.md", Some(b"BOB\ntwo\nthree\n")),
                ("big.md", Some(&large_current)),
                ("logo.png", Some(&png_bob)),
            ],
        );
        let (host, _) = host(std::slice::from_ref(&root));
        host.start_all();
        let status = listed(&host, 4);
        let view = |path: &str| host.conflict_detail(0, &id_of(&status, path)).unwrap();

        let text = view("note.md");
        assert!(matches!(text.body, Body::Text { .. }));
        assert_eq!(text.keep_label, "Keep current version (You)");
        assert_eq!(text.use_label, "Use other version (Alice)");
        assert!(text.base.is_some());

        let deleted = view("gone.md");
        assert!(deleted.other.placeholder.is_some());
        assert_eq!(deleted.use_label, "Use other version (deleted by Alice)");

        let big = view("big.md");
        assert_eq!(
            big.body,
            Body::Metadata {
                reason: "Too large to compare here"
            }
        );
        assert!(big.can_choose, "both choices remain");

        let image = view("logo.png");
        assert_eq!(image.body, Body::Binary);
        assert_eq!(
            image.current.preview.as_ref().map(|p| p.mime),
            Some("image/png")
        );
        assert!(!image.summary.contains("saved copy"));

        // Previews come back for images only, as bytes by id and side.
        let logo = id_of(&status, "logo.png");
        let (mime, bytes) = host.conflict_preview(0, &logo, "current").unwrap().unwrap();
        assert_eq!((mime, bytes), ("image/png", png_bob));
        let (_, other) = host.conflict_preview(0, &logo, "other").unwrap().unwrap();
        assert_eq!(other, png_alice);
        assert_eq!(
            host.conflict_preview(0, &id_of(&status, "note.md"), "current")
                .unwrap(),
            None
        );
        assert_eq!(
            host.conflict_preview(0, &id_of(&status, "gone.md"), "other")
                .unwrap(),
            None
        );
        host.quit();
    }

    #[test]
    fn a_non_utf8_file_gets_the_binary_view_with_whole_file_choice() {
        let parent = tempfile::tempdir().unwrap();
        let root = collide(
            parent.path(),
            &[("legacy.txt", b"base\n")],
            &[("legacy.txt", Some(&[b'a', 0xE9, b'\n']))],
            &[("legacy.txt", Some(&[b'b', 0xE9, b'\n']))],
        );
        let (host, _) = host(std::slice::from_ref(&root));
        host.start_all();
        let status = listed(&host, 1);
        assert!(status.unresolved_conflicts[0].binary);
        let view = host
            .conflict_detail(0, &id_of(&status, "legacy.txt"))
            .unwrap();
        assert_eq!(view.body, Body::Binary);
        assert!(view.current.preview.is_none());
        assert_eq!(
            view.footer,
            "Whole-file choice only. Both versions stay in history."
        );
        host.quit();
    }

    #[test]
    fn a_current_version_from_shared_history_is_named_by_the_commit_that_made_it() {
        let parent = tempfile::tempdir().unwrap();
        let origin = origin(parent.path());
        let mut alice = client(parent.path(), "Alice", &origin);
        std::fs::write(alice.root.join("note.md"), BASE).unwrap();
        finish(&mut alice);
        let mut bob = client(parent.path(), "Bob", &origin);
        let mut carol = client(parent.path(), "Carol", &origin);
        bob.sync().unwrap();
        carol.sync().unwrap();
        std::fs::write(alice.root.join("note.md"), "ALICE\ntwo\nthree\n").unwrap();
        finish(&mut alice);
        std::fs::write(carol.root.join("note.md"), "CAROL\ntwo\nthree\n").unwrap();
        carol.capture_at(now()).unwrap();
        bob.sync().unwrap();
        carol.sync().unwrap();
        bob.sync().unwrap();
        let root = bob.root.clone();
        drop((alice, bob, carol));

        let (host, _) = host(std::slice::from_ref(&root));
        host.start_all();
        let status = listed(&host, 1);
        assert!(status.unresolved_conflicts[0].current.shared_history);
        let view = host.conflict_detail(0, &id_of(&status, "note.md")).unwrap();
        assert_eq!(view.keep_label, "Keep current version (shared history)");
        assert!(
            view.current
                .byline
                .text
                .starts_with("Alice · shared history · saved"),
            "{:?}",
            view.current.byline
        );
        assert_eq!(view.use_label, "Use other version (Carol)");
        host.quit();
    }
}
