use dispatch_mesh::{
    ai,
    engine::Workspace,
    git::{Git, validate_path},
    model::*,
    policy::Policy,
};
use std::{collections::BTreeSet, fs, path::Path};
use tempfile::TempDir;

fn identity(name: &str) -> Identity {
    Identity {
        name: name.into(),
        email: format!("{}@example.test", name.to_lowercase()),
    }
}

fn init(path: &Path, name: &str, remote: Option<&Path>) -> Workspace {
    Workspace::init(
        path,
        identity(name),
        remote.map(|p| p.to_string_lossy().into_owned()),
        "main".into(),
    )
    .unwrap()
}

fn finish(workspace: &mut Workspace) {
    let id = workspace.finish().unwrap();
    workspace.sync().unwrap();
    if let Some(id) = id
        && let Some(input) = workspace.summary_input(&id).unwrap()
    {
        let session = workspace.state.manifest.sessions.get_mut(&id).unwrap();
        session.message = Some(ai::fallback(&input));
        session.message_base = input.message_base;
        workspace.save().unwrap();
    }
    workspace.sync().unwrap();
}

fn remote(path: &Path) {
    Git {
        dir: path.to_path_buf(),
    }
    .init()
    .unwrap();
}

#[test]
fn sessions_span_files_and_survive_restart() {
    let temp = TempDir::new().unwrap();
    let mut workspace = init(temp.path(), "Alice", None);
    fs::write(temp.path().join("one.md"), "one").unwrap();
    workspace.capture_at(100).unwrap();
    let id = workspace.active_session().unwrap().id.clone();
    fs::write(temp.path().join("two.md"), "two").unwrap();
    workspace.capture_at(200).unwrap();
    assert_eq!(workspace.active_session().unwrap().id, id);
    assert_eq!(workspace.active_session().unwrap().changes.len(), 2);
    workspace.close_idle(499).unwrap();
    assert!(!workspace.state.manifest.sessions[&id].closed);
    workspace.close_idle(500).unwrap();
    assert!(workspace.state.manifest.sessions[&id].closed);
    drop(workspace);
    let workspace = Workspace::open(temp.path()).unwrap();
    assert_eq!(workspace.state.observed.len(), 2);
    assert!(workspace.state.manifest.sessions[&id].closed);
}

#[test]
fn published_history_is_immutable_and_binary_bytes_round_trip() {
    let temp = TempDir::new().unwrap();
    let mut workspace = init(temp.path(), "Alice", None);
    fs::write(temp.path().join("photo.bin"), [0, 255, 1, 2]).unwrap();
    finish(&mut workspace);
    let first = workspace.state.main.clone();
    assert_eq!(
        workspace.git.files(&first).unwrap()["photo.bin"],
        vec![0, 255, 1, 2]
    );
    fs::write(temp.path().join("note.md"), "new").unwrap();
    finish(&mut workspace);
    assert_ne!(workspace.state.main, first);
    assert_eq!(
        workspace
            .git
            .text(&["rev-parse", &format!("{}^", workspace.state.main)])
            .unwrap(),
        first
    );
}

#[test]
fn draft_reaches_another_client_before_session_closes() {
    let temp = TempDir::new().unwrap();
    let origin = temp.path().join("remote.git");
    remote(&origin);
    let mut alice = init(&temp.path().join("alice"), "Alice", Some(&origin));
    let mut bob = init(&temp.path().join("bob"), "Bob", Some(&origin));
    fs::write(alice.root.join("note.md"), "still editing").unwrap();
    alice.sync().unwrap();
    let session = alice.active_session().unwrap().id.clone();
    bob.sync().unwrap();
    assert_eq!(
        fs::read_to_string(bob.root.join("note.md")).unwrap(),
        "still editing"
    );
    assert!(!bob.state.manifest.sessions[&session].closed);
    assert!(
        bob.active_session().is_none(),
        "incoming saves must not become Bob's edits"
    );
    fs::write(alice.root.join("note.md"), "another draft").unwrap();
    alice.sync().unwrap();
    bob.sync().unwrap();
    assert_eq!(
        fs::read_to_string(bob.root.join("note.md")).unwrap(),
        "another draft"
    );
    finish(&mut alice);
    bob.sync().unwrap();
    assert!(bob.state.manifest.sessions[&session].published);
    assert_eq!(
        fs::read_to_string(bob.root.join("note.md")).unwrap(),
        "another draft"
    );
}

#[test]
fn binary_conflict_preserves_both_inputs_and_can_select_incoming() {
    let temp = TempDir::new().unwrap();
    let origin = temp.path().join("remote.git");
    remote(&origin);
    let mut alice = init(&temp.path().join("alice"), "Alice", Some(&origin));
    fs::write(alice.root.join("image.bin"), [0, 1]).unwrap();
    finish(&mut alice);
    let mut bob = init(&temp.path().join("bob"), "Bob", Some(&origin));
    bob.sync().unwrap();
    fs::write(alice.root.join("image.bin"), [0, 2]).unwrap();
    fs::write(bob.root.join("image.bin"), [0, 3]).unwrap();
    alice.sync().unwrap();
    bob.sync().unwrap();
    let conflict = bob.unresolved().next().unwrap().clone();
    assert!(conflict.binary);
    assert_eq!(conflict.base, Some(vec![0, 1]));
    assert_ne!(conflict.accepted, conflict.incoming);
    let expected = conflict.incoming.clone().unwrap();
    bob.resolve(&conflict.id, true).unwrap();
    assert_eq!(fs::read(bob.root.join("image.bin")).unwrap(), expected);
    assert!(bob.unresolved().next().is_none());
    bob.sync().unwrap();
    alice.sync().unwrap();
    assert_eq!(fs::read(alice.root.join("image.bin")).unwrap(), expected);
    finish(&mut alice);
    finish(&mut bob);
    alice.sync().unwrap();
    assert_eq!(fs::read(alice.root.join("image.bin")).unwrap(), expected);
}

#[test]
fn three_way_text_merge_preserves_independent_edits() {
    let temp = TempDir::new().unwrap();
    let workspace = init(temp.path(), "Alice", None);
    let original = Some(b"one\ntwo\nthree\nfour\nfive\n".to_vec());
    let ours = Some(b"ONE\ntwo\nthree\nfour\nfive\n".to_vec());
    let theirs = Some(b"one\ntwo\nthree\nfour\nFIVE\n".to_vec());
    let merged = workspace
        .git
        .merge(&ours, &original, &theirs)
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(merged, b"ONE\ntwo\nthree\nfour\nFIVE\n");
}

#[test]
fn agent_intent_is_scoped_to_exact_paths() {
    let temp = TempDir::new().unwrap();
    let mut workspace = init(temp.path(), "Agent", None);
    let intent = workspace
        .register_intent("Explain setup".into(), BTreeSet::from(["guide.md".into()]))
        .unwrap();
    fs::write(workspace.root.join("guide.md"), "setup steps").unwrap();
    fs::write(workspace.root.join("other.md"), "unrelated").unwrap();
    workspace.end_intent(&intent).unwrap();
    let session = workspace.active_session().unwrap();
    assert_eq!(session.changes["guide.md"].intent, Some(intent));
    assert!(session.changes["other.md"].intent.is_none());
    assert!(!session.closed);
}

#[test]
fn ignores_private_files_and_never_captures_mesh_storage() {
    let temp = TempDir::new().unwrap();
    let mut workspace = init(temp.path(), "Alice", None);
    fs::write(workspace.root.join(".env"), "TOKEN=secret").unwrap();
    fs::write(workspace.root.join(".env.local"), "TOKEN=secret").unwrap();
    fs::write(workspace.root.join("private.key"), "secret").unwrap();
    fs::write(workspace.root.join(".meshignore"), "private/\n").unwrap();
    fs::create_dir(workspace.root.join("private")).unwrap();
    fs::write(workspace.root.join("private/note.md"), "secret").unwrap();
    fs::write(workspace.root.join("public.md"), "hello").unwrap();
    workspace.capture_at(100).unwrap();
    assert_eq!(
        workspace.state.observed.keys().cloned().collect::<Vec<_>>(),
        vec![".meshignore", "public.md"]
    );
}

#[test]
fn apply_journal_preserves_newer_local_save() {
    let temp = TempDir::new().unwrap();
    let mut workspace = init(temp.path(), "Alice", None);
    fs::write(workspace.root.join("note.md"), "before").unwrap();
    workspace.capture_at(now()).unwrap();
    workspace.state.applying = Some(Files::from([("note.md".into(), b"incoming".to_vec())]));
    workspace.save().unwrap();
    fs::write(workspace.root.join("note.md"), "newer local edit").unwrap();
    drop(workspace);
    let workspace = Workspace::open(temp.path()).unwrap();
    assert_eq!(
        fs::read_to_string(workspace.root.join("note.md")).unwrap(),
        "newer local edit"
    );
    assert!(workspace.state.applying.is_none());
    assert_eq!(workspace.state.observed["note.md"], b"newer local edit");
}

#[test]
fn invalid_paths_cannot_escape_workspace() {
    for path in [
        "../secret",
        "/absolute",
        "C:/secret",
        ".mesh/state.sqlite",
        "foo/.git/config",
        "x\\..\\y",
        "con.txt",
        "folder./note",
    ] {
        assert!(validate_path(path).is_err(), "{path}");
    }
    assert!(validate_path("notes/readme.md").is_ok());
}

#[test]
fn offline_sync_keeps_durable_local_capture() {
    let temp = TempDir::new().unwrap();
    let missing_remote = temp.path().join("missing.git");
    let mut workspace = init(&temp.path().join("alice"), "Alice", Some(&missing_remote));
    fs::write(workspace.root.join("note.md"), "offline work").unwrap();
    assert!(workspace.sync().is_err());
    let root = workspace.root.clone();
    drop(workspace);
    let workspace = Workspace::open(&root).unwrap();
    assert_eq!(workspace.state.observed["note.md"], b"offline work");
    assert_eq!(workspace.active_session().unwrap().changes.len(), 1);
}

#[test]
fn later_local_edits_do_not_take_ownership_of_received_text() {
    let temp = TempDir::new().unwrap();
    let origin = temp.path().join("remote.git");
    remote(&origin);
    let mut alice = init(&temp.path().join("alice"), "Alice", Some(&origin));
    fs::write(alice.root.join("note.md"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
    finish(&mut alice);
    let mut bob = init(&temp.path().join("bob"), "Bob", Some(&origin));
    bob.sync().unwrap();
    fs::write(alice.root.join("note.md"), "ONE\ntwo\nthree\nfour\nfive\n").unwrap();
    fs::write(bob.root.join("note.md"), "one\ntwo\nthree\nfour\nFIVE\n").unwrap();
    alice.sync().unwrap();
    bob.sync().unwrap();
    alice.sync().unwrap();
    assert_eq!(
        fs::read_to_string(alice.root.join("note.md")).unwrap(),
        "ONE\ntwo\nthree\nfour\nFIVE\n"
    );
    fs::write(alice.root.join("note.md"), "ONE\nTWO\nthree\nfour\nFIVE\n").unwrap();
    alice.capture_at(now()).unwrap();
    let own = alice.active_session().unwrap().changes["note.md"]
        .content
        .as_ref()
        .unwrap();
    assert_eq!(
        own, b"ONE\nTWO\nthree\nfour\nfive\n",
        "Bob's draft must not become Alice's contribution"
    );
}

#[test]
fn journal_recovers_a_crash_after_displacing_the_old_file() {
    let temp = TempDir::new().unwrap();
    let mut workspace = init(temp.path(), "Alice", None);
    fs::write(workspace.root.join("note.md"), "before").unwrap();
    workspace.capture_at(now()).unwrap();
    workspace.state.applying = Some(Files::from([("note.md".into(), b"incoming".to_vec())]));
    let backup = workspace.root.join(".mesh/recovery/test-backup");
    fs::create_dir_all(&backup).unwrap();
    workspace
        .state
        .apply_backups
        .insert("note.md".into(), "test-backup".into());
    workspace.save().unwrap();
    fs::rename(workspace.root.join("note.md"), backup.join("content")).unwrap();
    drop(workspace);
    let workspace = Workspace::open(temp.path()).unwrap();
    assert_eq!(
        fs::read(workspace.root.join("note.md")).unwrap(),
        b"incoming"
    );
    assert_eq!(fs::read(backup.join("content")).unwrap(), b"before");
    assert!(workspace.state.applying.is_none());
}

#[test]
fn reverted_session_is_discarded_without_creating_history() {
    let temp = TempDir::new().unwrap();
    let mut workspace = init(temp.path(), "Alice", None);
    let original = workspace.state.main.clone();
    fs::write(workspace.root.join("note.md"), "temporary").unwrap();
    workspace.capture_at(now()).unwrap();
    fs::remove_file(workspace.root.join("note.md")).unwrap();
    let id = workspace.finish().unwrap().unwrap();
    workspace.sync().unwrap();
    assert_eq!(workspace.state.main, original);
    assert!(workspace.state.manifest.sessions[&id].discarded);
}

#[test]
fn presence_timestamps_belong_to_each_file() {
    let temp = TempDir::new().unwrap();
    let mut workspace = init(temp.path(), "Alice", None);
    fs::write(workspace.root.join("one.md"), "one").unwrap();
    workspace.capture_at(100).unwrap();
    fs::write(workspace.root.join("two.md"), "two").unwrap();
    workspace.capture_at(200).unwrap();
    let session = workspace.active_session().unwrap();
    assert_eq!(session.changes["one.md"].last_edit, 100);
    assert_eq!(session.changes["two.md"].last_edit, 200);
}

#[test]
fn conflict_selection_survives_publishing_the_incoming_session_first() {
    for choose_incoming in [false, true] {
        let temp = TempDir::new().unwrap();
        let origin = temp.path().join("origin.git");
        remote(&origin);
        let mut alice = init(&temp.path().join("alice"), "Alice", Some(&origin));
        fs::write(alice.root.join("image.bin"), [0, 1]).unwrap();
        finish(&mut alice);
        let mut bob = init(&temp.path().join("bob"), "Bob", Some(&origin));
        bob.sync().unwrap();
        fs::write(alice.root.join("image.bin"), [0, 2]).unwrap();
        fs::write(bob.root.join("image.bin"), [0, 3]).unwrap();
        let timestamp = now();
        alice.capture_at(timestamp).unwrap();
        bob.capture_at(timestamp + 1).unwrap();
        alice.sync().unwrap();
        bob.sync().unwrap();
        alice.sync().unwrap();
        let conflict = alice.unresolved().next().unwrap().clone();
        let selected = if choose_incoming {
            conflict.incoming.clone()
        } else {
            conflict.accepted.clone()
        }
        .unwrap();
        alice.resolve(&conflict.id, choose_incoming).unwrap();
        alice.sync().unwrap();
        bob.sync().unwrap();
        // The client that contributed the incoming version publishes first.
        // Arrival/publication order must not invalidate an explicit resolution.
        finish(&mut bob);
        finish(&mut alice);
        bob.sync().unwrap();
        alice.sync().unwrap();
        assert!(
            alice.unresolved().next().is_none(),
            "A resolved conflict reappeared after publication"
        );
        assert!(bob.unresolved().next().is_none());
        assert_eq!(
            alice.git.files(&alice.state.main).unwrap()["image.bin"],
            selected
        );
        assert_eq!(fs::read(alice.root.join("image.bin")).unwrap(), selected);
        assert_eq!(fs::read(bob.root.join("image.bin")).unwrap(), selected);
    }
}

#[test]
fn resolution_preserves_a_view_composed_from_multiple_draft_authors() {
    for choose_incoming in [false, true] {
        let temp = TempDir::new().unwrap();
        let origin = temp.path().join("origin.git");
        remote(&origin);
        let mut alice = init(&temp.path().join("alice"), "Alice", Some(&origin));
        let base = b"one\ntwo\nthree\nfour\nfive\n";
        fs::write(alice.root.join("note.md"), base).unwrap();
        finish(&mut alice);
        let mut bob = init(&temp.path().join("bob"), "Bob", Some(&origin));
        let mut charlie = init(&temp.path().join("charlie"), "Charlie", Some(&origin));
        bob.sync().unwrap();
        charlie.sync().unwrap();
        fs::write(
            alice.root.join("note.md"),
            b"ALICE\ntwo\nthree\nfour\nfive\n",
        )
        .unwrap();
        fs::write(bob.root.join("note.md"), b"one\ntwo\nthree\nfour\nBOB\n").unwrap();
        fs::write(
            charlie.root.join("note.md"),
            b"CHARLIE\ntwo\nthree\nfour\nfive\n",
        )
        .unwrap();
        let timestamp = now();
        alice.capture_at(timestamp).unwrap();
        bob.capture_at(timestamp + 1).unwrap();
        charlie.capture_at(timestamp + 2).unwrap();
        let alice_id = alice.active_session().unwrap().id.clone();
        let bob_id = bob.active_session().unwrap().id.clone();
        alice.sync().unwrap();
        bob.sync().unwrap();
        charlie.sync().unwrap();
        alice.sync().unwrap();
        let conflict = alice.unresolved().next().unwrap().clone();
        assert!(conflict.accepted_sessions.contains_key(&alice_id));
        assert!(conflict.accepted_sessions.contains_key(&bob_id));
        let selected = if choose_incoming {
            conflict.incoming.clone()
        } else {
            conflict.accepted.clone()
        }
        .unwrap();
        alice.resolve(&conflict.id, choose_incoming).unwrap();
        alice.sync().unwrap();
        bob.sync().unwrap();
        charlie.sync().unwrap();
        finish(&mut charlie);
        finish(&mut bob);
        finish(&mut alice);
        bob.sync().unwrap();
        charlie.sync().unwrap();
        alice.sync().unwrap();
        for workspace in [&alice, &bob, &charlie] {
            assert!(workspace.unresolved().next().is_none());
            assert_eq!(fs::read(workspace.root.join("note.md")).unwrap(), selected);
            assert_eq!(
                workspace.git.files(&workspace.state.main).unwrap()["note.md"],
                selected
            );
        }
    }
}

#[test]
fn identical_concurrent_changes_do_not_add_an_empty_session_commit() {
    let temp = TempDir::new().unwrap();
    let origin = temp.path().join("origin.git");
    remote(&origin);
    let mut alice = init(&temp.path().join("alice"), "Alice", Some(&origin));
    fs::write(alice.root.join("note.md"), "original").unwrap();
    finish(&mut alice);
    let mut bob = init(&temp.path().join("bob"), "Bob", Some(&origin));
    bob.sync().unwrap();
    fs::write(alice.root.join("note.md"), "same edit").unwrap();
    fs::write(bob.root.join("note.md"), "same edit").unwrap();
    alice.capture_at(now()).unwrap();
    bob.capture_at(now()).unwrap();
    let bob_id = bob.active_session().unwrap().id.clone();
    finish(&mut alice);
    let main = alice.state.main.clone();
    finish(&mut bob);
    assert_eq!(bob.state.main, main);
    assert!(bob.state.manifest.sessions[&bob_id].discarded);
    assert!(!bob.state.manifest.sessions[&bob_id].changes.is_empty());

    // The status model reports the discarded session as such, before and after
    // a restart, and a discarded session leaves nothing waiting.
    let assert_discarded = |workspace: &Workspace| {
        let snapshot = workspace.status_snapshot();
        let session = snapshot.sessions.iter().find(|s| s.id == bob_id).unwrap();
        assert_eq!(session.stage, dispatch_mesh::status::Stage::Discarded);
        assert_eq!(snapshot.status, dispatch_mesh::status::Summary::Synced);
        let json = serde_json::to_value(&snapshot).unwrap();
        let entry = json["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == bob_id.as_str())
            .unwrap();
        assert_eq!(entry["stage"], "discarded");
        assert_eq!(json["status"], "synced");
    };
    assert_discarded(&bob);
    let root = bob.root.clone();
    drop(bob);
    let bob = Workspace::open(&root).unwrap();
    assert_discarded(&bob);
    assert!(!bob.state.manifest.sessions[&bob_id].changes.is_empty());
    assert_eq!(bob.state.main, main, "no empty commit after restart either");
}

fn origin_messages(origin: &Path) -> String {
    Git {
        dir: origin.to_path_buf(),
    }
    .text(&["log", "--format=%B%x00", "main"])
    .unwrap()
}

fn is_state_file(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with("state.sqlite"))
}

/// Replace the `state.sqlite*` files in `to` with those in `from`. The state DB
/// runs in WAL mode, so a consistent copy takes every state file; nothing else
/// in the folder (Git store, config, locks) is touched.
fn copy_state(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(to).unwrap() {
        let path = entry.unwrap().path();
        if is_state_file(&path) {
            fs::remove_file(path).unwrap();
        }
    }
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("state.sqlite") {
            fs::copy(entry.path(), to.join(&name)).unwrap();
        }
    }
}

#[test]
fn interrupted_publication_is_recognized_and_never_published_twice() {
    let temp = TempDir::new().unwrap();
    let origin = temp.path().join("remote.git");
    remote(&origin);
    let mut alice = init(&temp.path().join("alice"), "Alice", Some(&origin));
    fs::write(alice.root.join("note.md"), "published once").unwrap();
    let id = alice.finish().unwrap().unwrap();
    alice.sync().unwrap();
    let input = alice.summary_input(&id).unwrap().unwrap();
    let session = alice.state.manifest.sessions.get_mut(&id).unwrap();
    session.message = Some(ai::fallback(&input));
    session.message_base = input.message_base;
    alice.save().unwrap();
    let root = alice.root.clone();
    let meta = root.join(".mesh");
    let backup = temp.path().join("state-before-publication");
    drop(alice);
    copy_state(&meta, &backup);

    let mut alice = Workspace::open(&root).unwrap();
    alice.sync().unwrap();
    assert!(alice.state.manifest.sessions[&id].published);
    drop(alice);

    // Crash window: the commit reached the remote, local state never recorded it.
    copy_state(&backup, &meta);
    let mut alice = Workspace::open(&root).unwrap();
    assert!(!alice.state.manifest.sessions[&id].published);

    alice.sync().unwrap();
    alice.sync().unwrap();
    assert!(alice.state.manifest.sessions[&id].published);
    let trailer = format!("Mesh-Session: {id}");
    assert_eq!(
        origin_messages(&origin).matches(&trailer).count(),
        1,
        "the session must be published exactly once"
    );
    assert_eq!(
        fs::read_to_string(alice.root.join("note.md")).unwrap(),
        "published once"
    );
    assert_eq!(
        alice.status_snapshot().sessions[0].stage,
        dispatch_mesh::status::Stage::Published
    );
}

#[test]
fn renames_and_deletions_converge_on_both_clients() {
    let temp = TempDir::new().unwrap();
    let origin = temp.path().join("remote.git");
    remote(&origin);
    let mut alice = init(&temp.path().join("alice"), "Alice", Some(&origin));
    let old_path = "Notizen für März.md";
    let new_path = "Archiv/Notizen für März.md";
    fs::write(alice.root.join(old_path), "Inhalt bleibt").unwrap();
    fs::write(alice.root.join("old.md"), "remove me").unwrap();
    finish(&mut alice);
    let mut bob = init(&temp.path().join("bob"), "Bob", Some(&origin));
    bob.sync().unwrap();
    assert!(bob.root.join(old_path).exists() && bob.root.join("old.md").exists());

    fs::create_dir_all(alice.root.join("Archiv")).unwrap();
    fs::rename(alice.root.join(old_path), alice.root.join(new_path)).unwrap();
    fs::remove_file(alice.root.join("old.md")).unwrap();
    finish(&mut alice);
    bob.sync().unwrap();

    assert_eq!(
        fs::read_to_string(bob.root.join(new_path)).unwrap(),
        "Inhalt bleibt"
    );
    assert!(!bob.root.join(old_path).exists(), "no copy at the old path");
    assert!(!bob.root.join("old.md").exists());
    let published = Git {
        dir: origin.to_path_buf(),
    }
    .tree_entries("main")
    .unwrap();
    assert_eq!(
        published.keys().cloned().collect::<Vec<_>>(),
        vec![new_path.to_string()]
    );
    assert!(alice.state.manifest.sessions.values().all(|s| s.settled()));
}

fn summary(workspace: &Workspace) -> dispatch_mesh::status::Summary {
    workspace.status_snapshot().status
}

fn own_stage(workspace: &Workspace) -> dispatch_mesh::status::Stage {
    let snapshot = workspace.status_snapshot();
    let mut stages = workspace
        .state
        .manifest
        .sessions
        .values()
        .filter(|s| s.device == workspace.config.device)
        .map(|s| {
            snapshot
                .sessions
                .iter()
                .find(|status| status.id == s.id)
                .unwrap()
                .stage
        });
    let stage = stages.next().expect("one own session");
    assert!(stages.next().is_none(), "expected exactly one own session");
    stage
}

#[test]
fn status_follows_a_session_from_local_save_to_publication() {
    use dispatch_mesh::status::{Stage, Summary};
    let temp = TempDir::new().unwrap();
    let origin = temp.path().join("remote.git");
    remote(&origin);
    let mut alice = init(&temp.path().join("alice"), "Alice", Some(&origin));
    assert_eq!(summary(&alice), Summary::Synced);

    fs::write(alice.root.join("note.md"), "first").unwrap();
    assert_eq!(summary(&alice), Summary::SavedLocally, "uncaptured edit");
    alice.capture_at(now()).unwrap();
    assert_eq!(
        own_stage(&alice),
        Stage::SavedLocally,
        "captured, not pushed"
    );

    alice.sync().unwrap();
    assert_eq!(own_stage(&alice), Stage::DraftUploaded);
    assert_eq!(summary(&alice), Summary::DraftUploaded);

    fs::write(alice.root.join("note.md"), "second").unwrap();
    alice.capture_at(now()).unwrap();
    assert_eq!(
        own_stage(&alice),
        Stage::SavedLocally,
        "a newer revision is not uploaded yet"
    );

    let id = alice.finish().unwrap().unwrap();
    alice.sync().unwrap();
    assert_eq!(own_stage(&alice), Stage::WaitingForFinalization);
    assert_eq!(summary(&alice), Summary::WaitingForFinalization);
    assert!(alice.status_snapshot().reason.is_none());

    let input = alice.summary_input(&id).unwrap().unwrap();
    let session = alice.state.manifest.sessions.get_mut(&id).unwrap();
    session.message = Some(ai::fallback(&input));
    session.message_base = input.message_base;
    alice.save().unwrap();
    alice.sync().unwrap();
    assert_eq!(own_stage(&alice), Stage::Published);
    assert_eq!(summary(&alice), Summary::Synced);
}

#[test]
fn a_local_only_workspace_has_no_upload_stage() {
    use dispatch_mesh::status::{Stage, Summary};
    let temp = TempDir::new().unwrap();
    let mut alice = init(&temp.path().join("alice"), "Alice", None);
    fs::write(alice.root.join("note.md"), "local").unwrap();
    alice.capture_at(now()).unwrap();
    assert_eq!(own_stage(&alice), Stage::SavedLocally);
    alice.finish().unwrap();
    assert_eq!(own_stage(&alice), Stage::WaitingForFinalization);
    assert_eq!(summary(&alice), Summary::WaitingForFinalization);
}

#[test]
fn received_drafts_count_as_uploaded_and_do_not_change_the_receiver_summary() {
    use dispatch_mesh::status::{Stage, Summary};
    let temp = TempDir::new().unwrap();
    let origin = temp.path().join("remote.git");
    remote(&origin);
    let mut alice = init(&temp.path().join("alice"), "Alice", Some(&origin));
    let mut bob = init(&temp.path().join("bob"), "Bob", Some(&origin));
    fs::write(alice.root.join("note.md"), "draft").unwrap();
    alice.sync().unwrap();
    let id = alice.active_session().unwrap().id.clone();
    bob.sync().unwrap();
    let snapshot = bob.status_snapshot();
    let received = snapshot.sessions.iter().find(|s| s.id == id).unwrap();
    assert_eq!(received.stage, Stage::DraftUploaded);
    assert_eq!(snapshot.status, Summary::Synced);
}

#[test]
fn upload_bookkeeping_stays_local_and_never_causes_another_push() {
    let temp = TempDir::new().unwrap();
    let origin = temp.path().join("remote.git");
    remote(&origin);
    let mut alice = init(&temp.path().join("alice"), "Alice", Some(&origin));
    fs::write(alice.root.join("note.md"), "draft").unwrap();
    alice.sync().unwrap();
    let id = alice.active_session().unwrap().id.clone();
    assert_eq!(
        alice.state.uploaded.get(&id),
        Some(&alice.state.manifest.sessions[&id].revision)
    );
    let origin_git = Git {
        dir: origin.to_path_buf(),
    };
    let device_ref = format!("refs/heads/mesh/devices/{}", alice.config.device);
    let pushed = origin_git.text(&["rev-parse", &device_ref]).unwrap();
    alice.sync().unwrap();
    alice.sync().unwrap();
    assert_eq!(
        origin_git.text(&["rev-parse", &device_ref]).unwrap(),
        pushed,
        "recording an upload must not produce another device-branch commit"
    );
    for bytes in origin_git.files(&device_ref).unwrap().values() {
        assert!(
            !String::from_utf8_lossy(bytes).contains("uploaded"),
            "upload bookkeeping must not be published"
        );
    }
}

#[test]
fn older_state_without_the_new_fields_loads_with_empty_defaults() {
    let mut value = serde_json::to_value(State::<Vec<u8>>::default()).unwrap();
    let object = value.as_object_mut().unwrap();
    for key in [
        "uploaded",
        "offline",
        "summary_failure",
        "last_sync",
        "saved_copies",
    ] {
        assert!(object.remove(key).is_some(), "{key} is serialized");
    }
    let state: State = serde_json::from_value(value).unwrap();
    assert!(state.uploaded.is_empty());
    assert!(state.offline.is_none() && state.summary_failure.is_none());
    assert!(state.last_sync.is_none() && state.saved_copies.is_none());
}

#[test]
fn unclassified_sync_failures_and_ai_failures_need_attention_with_a_reason() {
    use dispatch_mesh::status::Summary;
    let temp = TempDir::new().unwrap();
    let missing_remote = temp.path().join("missing.git");
    let alice = init(&temp.path().join("alice"), "Alice", Some(&missing_remote));
    fs::write(alice.root.join("note.md"), "work").unwrap();
    let root = alice.root.clone();
    drop(alice);
    assert!(Workspace::sync_and_record(&root).is_err());
    let alice = Workspace::open(&root).unwrap();
    let snapshot = alice.status_snapshot();
    assert_eq!(snapshot.status, Summary::NeedsAttention);
    assert!(
        snapshot.offline.is_none(),
        "a missing repository is not offline"
    );
    assert!(
        snapshot
            .reason
            .as_deref()
            .is_some_and(|r| r.starts_with("Synchronization waiting: ")),
        "{:?}",
        snapshot.reason
    );
    assert_eq!(
        alice.state.observed["note.md"], b"work",
        "capture continues"
    );

    let temp = TempDir::new().unwrap();
    let mut bob = init(&temp.path().join("bob"), "Bob", None);
    bob.state.summary_failure = Some("AI summary failed: Provider returned HTTP 401".into());
    bob.save().unwrap();
    let snapshot = bob.status_snapshot();
    assert_eq!(snapshot.status, Summary::NeedsAttention);
    assert_eq!(
        snapshot.reason.as_deref(),
        Some("AI summary failed: Provider returned HTTP 401")
    );
}

#[test]
fn a_failed_sync_records_its_cause_even_when_another_operation_holds_the_lock() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
        time::{Duration, Instant},
    };
    let temp = TempDir::new().unwrap();
    // A loopback remote that stalls, then answers HTTP 500. A server error is
    // an unclassified Git failure (`needs_attention`) on every platform; a bare
    // hang-up is not portable: Windows curl reports "Empty reply from server",
    // Linux curl "Connection reset by peer" (offline).
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/repo.git", listener.local_addr().unwrap());
    let (network_started, network_signal) = mpsc::channel();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let _ = network_started.send(());
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => request.extend_from_slice(&chunk[..count]),
                }
            }
            std::thread::sleep(Duration::from_secs(1));
            let _ = stream.write_all(
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    let mut alice = init(&temp.path().join("alice"), "Alice", None);
    // Test-only HTTP transport to the loopback fake remote, as in tests/cli.rs.
    alice.config.remote = Some("test-remote".into());
    alice.save_config().unwrap();
    alice
        .git
        .run(&["config", "remote.origin.url", &url], None)
        .unwrap();
    fs::write(alice.root.join("note.md"), "saved before the failing sync").unwrap();
    let root = alice.root.clone();
    drop(alice);

    // A competing operation takes the lock while the sync waits on the network
    // and keeps it past network_call's 5 s re-acquire window.
    let holder_root = root.clone();
    let holder = std::thread::spawn(move || {
        network_signal.recv().unwrap();
        let started = Instant::now();
        let workspace = loop {
            match Workspace::open(&holder_root) {
                Ok(workspace) => break workspace,
                Err(_) if started.elapsed() < Duration::from_secs(10) => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("competing operation never got the lock: {error:#}"),
            }
        };
        std::thread::sleep(Duration::from_secs(7));
        drop(workspace);
    });
    let error = Workspace::sync_and_record(&root).unwrap_err();
    holder.join().unwrap();
    assert!(
        !error.is::<dispatch_mesh::engine::WorkspaceBusy>(),
        "lock contention must not replace the sync's own failure: {error:#}"
    );

    let alice = Workspace::open(&root).unwrap();
    let snapshot = alice.status_snapshot();
    assert_eq!(
        snapshot.status,
        dispatch_mesh::status::Summary::NeedsAttention
    );
    assert!(
        snapshot
            .reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("Synchronization waiting: ")),
        "the cause must be durable: {snapshot:?}"
    );
    assert_eq!(
        alice.state.observed["note.md"],
        b"saved before the failing sync"
    );
    drop(alice);

    // The next successful sync clears the recorded failure.
    let origin = temp.path().join("origin.git");
    remote(&origin);
    let alice = Workspace::open(&root).unwrap();
    alice
        .git
        .run(
            &["config", "remote.origin.url", &origin.to_string_lossy()],
            None,
        )
        .unwrap();
    drop(alice);
    Workspace::sync_and_record(&root).unwrap();
    let alice = Workspace::open(&root).unwrap();
    assert!(alice.state.attention.is_none() && alice.state.offline.is_none());
    assert!(alice.status_snapshot().reason.is_none());
}

// US00003: excluding already synchronized files (ADR-0013).

const LAYOUT: &str = ".obsidian/workspace.json";

fn shared(workspace: &Workspace) -> Files {
    workspace.git.files(&workspace.state.main).unwrap()
}

/// No tree on main holds a path its own ignore rules exclude.
fn assert_main_respects_its_rules(workspace: &Workspace) {
    let files = shared(workspace);
    let policy = Policy::from_files(&workspace.root, &files).unwrap();
    for path in files.keys() {
        assert!(!policy.ignores(path), "main holds excluded {path}");
    }
}

/// Publish every closed session of these devices, as the runner would over
/// later cycles: close, sync, prepare the missing messages, sync again.
fn settle(workspaces: &mut [&mut Workspace]) {
    for _ in 0..4 {
        for workspace in workspaces.iter_mut() {
            workspace.finish().unwrap();
            workspace.sync().unwrap();
            let ids: Vec<_> = workspace
                .state
                .manifest
                .sessions
                .values()
                .filter(|s| {
                    s.device == workspace.config.device
                        && s.closed
                        && !s.settled()
                        && s.message.is_none()
                })
                .map(|s| s.id.clone())
                .collect();
            for id in ids {
                if let Some(input) = workspace.summary_input(&id).unwrap() {
                    let session = workspace.state.manifest.sessions.get_mut(&id).unwrap();
                    session.message = Some(ai::fallback(&input));
                    session.message_base = input.message_base;
                    workspace.save().unwrap();
                }
            }
            workspace.sync().unwrap();
        }
    }
}

/// Two devices on one remote that both hold `LAYOUT` with `content`.
fn two_devices_sharing_layout(temp: &TempDir, content: &str) -> (Workspace, Workspace) {
    let origin = temp.path().join("remote.git");
    remote(&origin);
    let mut alice = init(&temp.path().join("alice"), "Alice", Some(&origin));
    fs::create_dir_all(alice.root.join(".obsidian")).unwrap();
    fs::write(alice.root.join(LAYOUT), content).unwrap();
    fs::write(alice.root.join(".obsidian/app.json"), "{}").unwrap();
    finish(&mut alice);
    let mut bob = init(&temp.path().join("bob"), "Bob", Some(&origin));
    bob.sync().unwrap();
    assert_eq!(fs::read_to_string(bob.root.join(LAYOUT)).unwrap(), content);
    (alice, bob)
}

fn exclude_layout(workspace: &Workspace) {
    fs::write(workspace.root.join(".meshignore"), format!("{LAYOUT}\n")).unwrap();
}

#[test]
fn excluding_a_synchronized_file_stops_syncing_it_without_deleting_it() {
    let temp = TempDir::new().unwrap();
    let mut workspace = init(temp.path(), "Alice", None);
    fs::create_dir_all(workspace.root.join(".obsidian/plugins/dispatch")).unwrap();
    fs::write(workspace.root.join(LAYOUT), "layout-1").unwrap();
    fs::write(workspace.root.join(".obsidian/app.json"), "{}").unwrap();
    fs::write(
        workspace.root.join(".obsidian/plugins/dispatch/data.json"),
        "{}",
    )
    .unwrap();
    finish(&mut workspace);
    assert!(shared(&workspace).contains_key(LAYOUT));

    // AC 1: capture succeeds, the file stays, and main records an attributed
    // removal in the same commit as the rule.
    exclude_layout(&workspace);
    assert!(workspace.capture_at(now()).unwrap());
    finish(&mut workspace);
    assert!(workspace.state.attention.is_none());
    assert_eq!(
        fs::read_to_string(workspace.root.join(LAYOUT)).unwrap(),
        "layout-1"
    );
    let files = shared(&workspace);
    assert!(!files.contains_key(LAYOUT));
    assert!(files.contains_key(".meshignore"));
    let main = workspace.state.main.clone();
    let touched = workspace
        .git
        .text(&["diff-tree", "--no-commit-id", "--name-only", "-r", &main])
        .unwrap();
    assert!(
        touched.lines().any(|line| line == ".meshignore"),
        "{touched}"
    );
    assert!(touched.lines().any(|line| line == LAYOUT), "{touched}");
    let author = workspace
        .git
        .text(&["log", "-1", "--format=%an", &main])
        .unwrap();
    assert_eq!(author.trim(), "Alice");
    let message = workspace
        .git
        .text(&["log", "-1", "--format=%B", &main])
        .unwrap();
    assert!(
        message.contains(&format!("Stopped synchronizing (now ignored): {LAYOUT}")),
        "{message}"
    );
    assert!(
        !message.contains(&format!("Deleted: {LAYOUT}")),
        "{message}"
    );
    assert!(!workspace.state.observed.contains_key(LAYOUT));

    // AC 2: later edits to the excluded file are not captured.
    fs::write(workspace.root.join(LAYOUT), "layout-2").unwrap();
    assert!(!workspace.capture_at(now()).unwrap());
    assert!(workspace.active_session().is_none());

    // AC 3: the rest of .obsidian/ keeps synchronizing.
    fs::write(workspace.root.join(".obsidian/app.json"), "{\"a\":1}").unwrap();
    fs::write(
        workspace.root.join(".obsidian/plugins/dispatch/data.json"),
        "{\"b\":1}",
    )
    .unwrap();
    assert!(workspace.capture_at(now()).unwrap());
    let changes = &workspace.active_session().unwrap().changes;
    assert!(changes.contains_key(".obsidian/app.json"));
    assert!(changes.contains_key(".obsidian/plugins/dispatch/data.json"));
    finish(&mut workspace);
    assert_eq!(shared(&workspace)[".obsidian/app.json"], b"{\"a\":1}");
    assert_main_respects_its_rules(&workspace);
}

#[test]
fn second_device_keeps_its_file_when_the_rule_arrives() {
    let temp = TempDir::new().unwrap();
    let (mut alice, mut bob) = two_devices_sharing_layout(&temp, "shared layout");
    exclude_layout(&alice);
    alice.sync().unwrap();
    // The draft carrying the rule and the removal reaches Bob before it is final.
    bob.sync().unwrap();
    assert_eq!(
        fs::read_to_string(bob.root.join(LAYOUT)).unwrap(),
        "shared layout"
    );
    assert!(bob.state.attention.is_none());
    finish(&mut alice);
    bob.sync().unwrap();
    assert_eq!(fs::read(bob.root.join(LAYOUT)).unwrap(), b"shared layout");
    assert!(bob.state.attention.is_none());
    assert!(!bob.state.observed.contains_key(LAYOUT));
    assert!(!shared(&bob).contains_key(LAYOUT));
    assert_eq!(
        fs::read_to_string(bob.root.join(".meshignore")).unwrap(),
        format!("{LAYOUT}\n")
    );
    fs::write(bob.root.join(LAYOUT), "layout of Bob").unwrap();
    assert!(!bob.capture_at(now()).unwrap());
    bob.sync().unwrap();
    alice.sync().unwrap();
    assert_eq!(
        fs::read_to_string(alice.root.join(LAYOUT)).unwrap(),
        "shared layout"
    );
    assert_main_respects_its_rules(&bob);
}

fn peer_edit_before_the_rule_arrives(bob_publishes_first: bool) {
    let temp = TempDir::new().unwrap();
    let (mut alice, mut bob) = two_devices_sharing_layout(&temp, "base");
    fs::write(bob.root.join(LAYOUT), "bob layout").unwrap();
    bob.sync().unwrap();
    exclude_layout(&alice);
    alice.sync().unwrap();
    // AC 5: Alice receives the edit without a pause, a message or a conflict,
    // and her file is untouched.
    assert!(alice.state.attention.is_none());
    assert!(alice.state.conflicts.values().all(|c| c.path != LAYOUT));
    assert_eq!(fs::read_to_string(alice.root.join(LAYOUT)).unwrap(), "base");
    if bob_publishes_first {
        settle(&mut [&mut bob, &mut alice]);
    } else {
        settle(&mut [&mut alice, &mut bob]);
    }
    for workspace in [&alice, &bob] {
        assert!(workspace.state.attention.is_none());
        // The removal wins without a merge: no conflict is even recorded.
        assert!(workspace.state.conflicts.values().all(|c| c.path != LAYOUT));
        assert!(!shared(workspace).contains_key(LAYOUT));
        assert!(
            workspace
                .state
                .manifest
                .sessions
                .values()
                .all(|s| s.settled())
        );
        assert_main_respects_its_rules(workspace);
    }
    assert_eq!(shared(&alice), shared(&bob));
    assert_eq!(fs::read_to_string(alice.root.join(LAYOUT)).unwrap(), "base");
    assert_eq!(
        fs::read_to_string(bob.root.join(LAYOUT)).unwrap(),
        "bob layout"
    );
}

#[test]
fn peer_edit_to_an_excluded_path_is_dropped_when_the_rule_publishes_first() {
    peer_edit_before_the_rule_arrives(false);
}

#[test]
fn peer_edit_to_an_excluded_path_is_dropped_when_the_edit_publishes_first() {
    peer_edit_before_the_rule_arrives(true);
}

#[test]
fn exclusion_replaces_a_pending_draft_edit() {
    let temp = TempDir::new().unwrap();
    let mut workspace = init(temp.path(), "Alice", None);
    fs::create_dir_all(workspace.root.join(".obsidian")).unwrap();
    fs::write(workspace.root.join(LAYOUT), "published").unwrap();
    finish(&mut workspace);
    fs::write(workspace.root.join(LAYOUT), "draft edit").unwrap();
    workspace.capture_at(now()).unwrap();
    assert!(
        workspace
            .active_session()
            .unwrap()
            .changes
            .contains_key(LAYOUT)
    );
    exclude_layout(&workspace);
    workspace.capture_at(now()).unwrap();
    let change = &workspace.active_session().unwrap().changes[LAYOUT];
    assert_eq!(change.base.as_deref(), Some(&b"published"[..]));
    assert_eq!(change.content, None);
    finish(&mut workspace);
    assert!(!shared(&workspace).contains_key(LAYOUT));
    let history = workspace
        .git
        .text(&["log", "-p", "--format=%H", &workspace.state.main])
        .unwrap();
    assert!(!history.contains("draft edit"));
    assert_eq!(
        fs::read_to_string(workspace.root.join(LAYOUT)).unwrap(),
        "draft edit"
    );
}

#[test]
fn exclusion_settles_an_open_conflict_and_keeps_both_inputs() {
    let temp = TempDir::new().unwrap();
    let (mut alice, mut bob) = two_devices_sharing_layout(&temp, "base");
    fs::write(alice.root.join(LAYOUT), "alice layout").unwrap();
    fs::write(bob.root.join(LAYOUT), "bob layout").unwrap();
    alice.sync().unwrap();
    bob.sync().unwrap();
    let conflict = bob
        .unresolved()
        .next()
        .expect("concurrent layouts conflict")
        .clone();
    assert_eq!(conflict.path, LAYOUT);
    let on_disk = fs::read(bob.root.join(LAYOUT)).unwrap();
    exclude_layout(&bob);
    bob.sync().unwrap();
    // AC 7: settled as a traceable resolution that selects "removed".
    let resolution = &bob.state.manifest.resolutions[&conflict.id];
    assert_eq!(resolution.selected, None);
    assert_eq!(resolution.author.name, "Bob");
    let record = &bob.state.conflicts[&conflict.id];
    assert_eq!(record.base, conflict.base);
    assert_eq!(record.accepted, conflict.accepted);
    assert_eq!(record.incoming, conflict.incoming);
    assert!(bob.unresolved().next().is_none());
    assert_eq!(fs::read(bob.root.join(LAYOUT)).unwrap(), on_disk);
    settle(&mut [&mut bob, &mut alice]);
    for workspace in [&alice, &bob] {
        assert!(workspace.unresolved().next().is_none());
        assert!(!shared(workspace).contains_key(LAYOUT));
        assert!(workspace.state.attention.is_none());
    }
    assert_eq!(
        fs::read_to_string(alice.root.join(LAYOUT)).unwrap(),
        "alice layout"
    );
}

/// Both devices exclude LAYOUT, then hold `alice_copy` / `bob_copy` locally.
fn excluded_on_both(temp: &TempDir, alice_copy: &str, bob_copy: &str) -> (Workspace, Workspace) {
    let (mut alice, mut bob) = two_devices_sharing_layout(temp, "base");
    exclude_layout(&alice);
    settle(&mut [&mut alice, &mut bob]);
    fs::write(alice.root.join(LAYOUT), alice_copy).unwrap();
    fs::write(bob.root.join(LAYOUT), bob_copy).unwrap();
    settle(&mut [&mut alice, &mut bob]);
    assert!(!shared(&alice).contains_key(LAYOUT));
    assert!(!shared(&bob).contains_key(LAYOUT));
    (alice, bob)
}

#[test]
fn re_including_identical_copies_brings_the_file_back_without_conflict() {
    let temp = TempDir::new().unwrap();
    let (mut alice, mut bob) = excluded_on_both(&temp, "same", "same");
    fs::write(alice.root.join(".meshignore"), "").unwrap();
    settle(&mut [&mut alice, &mut bob]);
    for workspace in [&alice, &bob] {
        assert!(workspace.unresolved().next().is_none());
        assert_eq!(shared(workspace)[LAYOUT], b"same");
    }
    assert_eq!(fs::read_to_string(bob.root.join(LAYOUT)).unwrap(), "same");
}

#[test]
fn re_including_differing_copies_is_an_ordinary_conflict() {
    let temp = TempDir::new().unwrap();
    let (mut alice, mut bob) = excluded_on_both(&temp, "alice copy", "bob copy");
    fs::write(alice.root.join(".meshignore"), "").unwrap();
    alice.sync().unwrap();
    bob.sync().unwrap();
    alice.sync().unwrap();
    // AC 9: neither file is overwritten; both versions are kept as conflict inputs.
    assert_eq!(
        fs::read_to_string(alice.root.join(LAYOUT)).unwrap(),
        "alice copy"
    );
    assert_eq!(
        fs::read_to_string(bob.root.join(LAYOUT)).unwrap(),
        "bob copy"
    );
    let conflict = bob
        .unresolved()
        .chain(alice.unresolved())
        .find(|c| c.path == LAYOUT)
        .expect("differing copies conflict")
        .clone();
    let inputs = [conflict.accepted.clone(), conflict.incoming.clone()];
    assert!(inputs.contains(&Some(b"alice copy".to_vec())));
    assert!(inputs.contains(&Some(b"bob copy".to_vec())));
}

#[test]
fn init_writes_the_obsidian_excludes_visibly() {
    let rules = ".obsidian/workspace.json\n.obsidian/workspace-mobile.json\n";
    // A fresh vault.
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join(".obsidian")).unwrap();
    fs::write(temp.path().join(LAYOUT), "layout").unwrap();
    fs::write(temp.path().join(".obsidian/app.json"), "{}").unwrap();
    let workspace = init(temp.path(), "Alice", None);
    assert_eq!(
        fs::read_to_string(temp.path().join(".meshignore")).unwrap(),
        rules
    );
    let session = workspace.active_session().unwrap();
    assert!(session.changes.contains_key(".meshignore"));
    assert!(session.changes.contains_key(".obsidian/app.json"));
    assert!(!session.changes.contains_key(LAYOUT));

    // An existing .meshignore keeps its content and gains only the missing rule.
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join(".obsidian")).unwrap();
    fs::write(
        temp.path().join(".meshignore"),
        "private/\n.obsidian/workspace.json",
    )
    .unwrap();
    init(temp.path(), "Alice", None);
    assert_eq!(
        fs::read_to_string(temp.path().join(".meshignore")).unwrap(),
        format!("private/\n{rules}")
    );

    // Not an Obsidian vault: nothing is written.
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("note.md"), "note").unwrap();
    init(temp.path(), "Alice", None);
    assert!(!temp.path().join(".meshignore").exists());
}

#[test]
fn existing_workspaces_are_never_given_rules_automatically() {
    let temp = TempDir::new().unwrap();
    let mut workspace = init(temp.path(), "Alice", None);
    fs::create_dir_all(workspace.root.join(".obsidian")).unwrap();
    fs::write(workspace.root.join(LAYOUT), "layout").unwrap();
    finish(&mut workspace);
    drop(workspace);
    let mut workspace = Workspace::open(temp.path()).unwrap();
    workspace.sync().unwrap();
    assert!(!temp.path().join(".meshignore").exists());
    assert!(shared(&workspace).contains_key(LAYOUT));
}

#[test]
fn other_unsupported_paths_still_stop_capture() {
    let temp = TempDir::new().unwrap();
    let outer = temp.path().join("outer");
    let mut workspace = init(&outer.join("vault"), "Alice", None);
    fs::write(workspace.root.join("big.md"), "small").unwrap();
    fs::write(workspace.root.join("note.md"), "note").unwrap();
    finish(&mut workspace);

    workspace.config.max_file_bytes = 8;
    fs::write(workspace.root.join("big.md"), "now too large").unwrap();
    let error = workspace.capture_at(now()).unwrap_err().to_string();
    assert!(error.contains("size limit"), "{error}");
    workspace.config.max_file_bytes = 1024;
    fs::write(workspace.root.join("big.md"), "small").unwrap();

    // A rule outside the workspace is not part of its policy: the walker skips
    // the file and the policy doesn't, so capture refuses instead of recording a
    // deletion.
    fs::write(outer.join(".gitignore"), "note.md\n").unwrap();
    let error = workspace.capture_at(now()).unwrap_err().to_string();
    assert!(error.contains("now unsupported: note.md"), "{error}");
    assert_eq!(workspace.state.observed["note.md"], b"note");
}

#[test]
fn an_interrupted_apply_leaves_an_excluded_file_alone_on_recovery() {
    let temp = TempDir::new().unwrap();
    let mut workspace = init(temp.path(), "Alice", None);
    fs::create_dir_all(workspace.root.join(".obsidian")).unwrap();
    fs::write(workspace.root.join(LAYOUT), "layout").unwrap();
    finish(&mut workspace);
    // The journal of an apply that brings the rule and drops the path.
    let mut desired = workspace.state.observed.clone();
    desired.remove(LAYOUT);
    desired.insert(".meshignore".into(), format!("{LAYOUT}\n").into_bytes());
    workspace.state.applying = Some(desired);
    workspace.save().unwrap();
    drop(workspace);
    let workspace = Workspace::open(temp.path()).unwrap();
    assert!(workspace.state.applying.is_none());
    assert_eq!(
        fs::read_to_string(temp.path().join(LAYOUT)).unwrap(),
        "layout"
    );
    assert!(temp.path().join(".meshignore").exists());
    assert!(!workspace.state.observed.contains_key(LAYOUT));
    let recovery = temp.path().join(".mesh/recovery");
    assert!(!recovery.exists() || fs::read_dir(recovery).unwrap().next().is_none());
}

#[test]
fn tree_policy_agrees_with_the_folder_walker() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let files: &[(&str, &str)] = &[
        (
            ".gitignore",
            "*.log\nbuild/\n/root-only.txt\nrules/.meshignore\n",
        ),
        (".ignore", "scratch.txt\n"),
        (
            ".meshignore",
            "\u{feff}!keep.log\nsecret/\n# comment\nspaced.txt   \r\n",
        ),
        ("keep.log", "x"),
        ("drop.log", "x"),
        ("root-only.txt", "x"),
        ("scratch.txt", "x"),
        ("spaced.txt", "x"),
        ("note.md", "x"),
        ("build/out.md", "x"),
        ("build/.gitignore", "!*\n"),
        ("secret/a.md", "x"),
        ("sub/.gitignore", "!*.log\nlocal.txt\n"),
        ("sub/.meshignore", "x.md\n!local.txt\n"),
        ("sub/a.log", "x"),
        ("sub/x.md", "x"),
        ("sub/local.txt", "x"),
        ("sub/root-only.txt", "x"),
        ("sub/deep/x.md", "x"),
        ("sub/deep/.gitignore", "*.md\n"),
        ("sub/deep/y.md", "x"),
        ("sub/deep/z.txt", "x"),
        // F2: rule files matched by a rule, or by themselves, still belong to the tree.
        ("rules/.meshignore", "r.md\n"),
        ("rules/r.md", "x"),
        ("rules/s.md", "x"),
        ("other/.gitignore", ".gitignore\nz.md\n"),
        ("other/z.md", "x"),
        ("other/y.md", "x"),
        ("docs/.env", "x"),
        ("docs/id.key", "x"),
        ("docs/readme.md", "x"),
    ];
    let mut tree = Files::new();
    for (path, content) in files {
        let target = root.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, content).unwrap();
        tree.insert(path.to_string(), content.as_bytes().to_vec());
    }
    let workspace = init(root, "Alice", None);
    let walked = workspace.scan().unwrap();
    let policy = Policy::from_files(&workspace.root, &tree).unwrap();
    for (path, _) in files {
        assert_eq!(
            walked.contains_key(*path),
            !policy.ignores(path),
            "walker and policy disagree on {path}"
        );
    }
    for path in [
        "rules/.meshignore",
        "other/.gitignore",
        "rules/s.md",
        "other/y.md",
    ] {
        assert!(walked.contains_key(path), "{path} must synchronize");
    }
    for path in ["rules/r.md", "other/z.md", "build/.gitignore"] {
        assert!(!walked.contains_key(path), "{path} must stay out");
    }
}

// US00003 code review fixes (F1, F2).

#[cfg(windows)]
fn link_file(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

#[cfg(unix)]
fn link_file(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[test]
fn init_refuses_a_linked_meshignore_without_writing_through_it() {
    let temp = TempDir::new().unwrap();
    let outside = temp.path().join("outside.txt");
    fs::write(&outside, "original\n").unwrap();
    let vault = temp.path().join("vault");
    fs::create_dir_all(vault.join(".obsidian")).unwrap();
    if let Err(error) = link_file(&outside, &vault.join(".meshignore")) {
        // Creating symlinks needs Developer Mode or elevation on Windows.
        eprintln!("skipped: cannot create a symlink here: {error}");
        return;
    }
    let error = Workspace::init(&vault, identity("Alice"), None, "main".into())
        .err()
        .expect("a linked .meshignore is unsupported")
        .to_string();
    assert!(error.contains("Symbolic links are unsupported"), "{error}");
    assert_eq!(fs::read(&outside).unwrap(), b"original\n");
}

#[test]
fn an_ignored_meshignore_still_synchronizes_and_excludes() {
    let temp = TempDir::new().unwrap();
    let (mut alice, mut bob) = two_devices_sharing_layout(&temp, "layout");
    fs::write(alice.root.join(".gitignore"), ".meshignore\n").unwrap();
    finish(&mut alice);
    bob.sync().unwrap();
    // AC 15: the rule file is matched by .gitignore, yet it reaches main and
    // the exclusion behaves as in AC 1.
    exclude_layout(&alice);
    assert!(alice.capture_at(now()).unwrap());
    finish(&mut alice);
    assert!(alice.state.attention.is_none());
    let files = shared(&alice);
    assert_eq!(files[".meshignore"], format!("{LAYOUT}\n").as_bytes());
    assert!(!files.contains_key(LAYOUT));
    assert_eq!(
        fs::read_to_string(alice.root.join(LAYOUT)).unwrap(),
        "layout"
    );
    assert_main_respects_its_rules(&alice);
    bob.sync().unwrap();
    assert!(bob.state.attention.is_none());
    assert_eq!(
        fs::read_to_string(bob.root.join(".meshignore")).unwrap(),
        format!("{LAYOUT}\n")
    );
    assert_eq!(fs::read_to_string(bob.root.join(LAYOUT)).unwrap(), "layout");
    fs::write(bob.root.join(LAYOUT), "layout of Bob").unwrap();
    assert!(!bob.capture_at(now()).unwrap());
}

#[test]
fn a_rule_file_that_ignores_itself_still_synchronizes() {
    let temp = TempDir::new().unwrap();
    let mut workspace = init(temp.path(), "Alice", None);
    fs::write(workspace.root.join("note.md"), "note").unwrap();
    finish(&mut workspace);
    fs::write(workspace.root.join(".gitignore"), ".gitignore\nnote.md\n").unwrap();
    assert!(workspace.capture_at(now()).unwrap());
    finish(&mut workspace);
    assert!(workspace.state.attention.is_none());
    let files = shared(&workspace);
    assert!(files.contains_key(".gitignore"));
    assert!(!files.contains_key("note.md"));
    assert_eq!(
        fs::read_to_string(workspace.root.join("note.md")).unwrap(),
        "note"
    );
}

#[test]
fn a_rule_file_inside_an_excluded_folder_stays_out() {
    let temp = TempDir::new().unwrap();
    let mut workspace = init(temp.path(), "Alice", None);
    fs::create_dir_all(workspace.root.join("build")).unwrap();
    fs::write(workspace.root.join(".gitignore"), "build/\n").unwrap();
    fs::write(workspace.root.join("build/.meshignore"), "x\n").unwrap();
    fs::write(workspace.root.join("note.md"), "note").unwrap();
    finish(&mut workspace);
    let files = shared(&workspace);
    assert!(files.contains_key(".gitignore"));
    assert!(!files.contains_key("build/.meshignore"));
}

#[test]
fn last_sync_is_recorded_on_success_survives_reopening_and_is_coalesced() {
    use dispatch_mesh::engine::LAST_SYNC_PRECISION;
    let temp = TempDir::new().unwrap();
    let origin = temp.path().join("remote.git");
    remote(&origin);
    let mut alice = init(&temp.path().join("alice"), "Alice", Some(&origin));
    let root = alice.root.clone();
    assert_eq!(alice.status_snapshot().last_sync, None, "never synced");
    assert!(alice.status_snapshot().has_remote);

    alice.sync().unwrap();
    let first = alice
        .status_snapshot()
        .last_sync
        .expect("set by a successful sync");
    assert!(now().saturating_sub(first) < 5);
    drop(alice);
    let mut alice = Workspace::open(&root).unwrap();
    assert_eq!(alice.status_snapshot().last_sync, Some(first), "persisted");

    // Within the precision window the stored time is left alone, so an idle
    // workspace does not rewrite its state every cycle.
    let recent = first - (LAST_SYNC_PRECISION - 10);
    alice.state.last_sync = Some(recent);
    alice.save().unwrap();
    alice.sync().unwrap();
    assert_eq!(alice.state.last_sync, Some(recent));
    // Once it is a full minute old, the next success rewrites it.
    let stale = first - LAST_SYNC_PRECISION;
    alice.state.last_sync = Some(stale);
    alice.save().unwrap();
    alice.sync().unwrap();
    assert!(alice.state.last_sync.unwrap() >= first);

    // A failed sync keeps the last success.
    let before = alice.state.last_sync;
    alice.config.remote = Some(temp.path().join("missing.git").display().to_string());
    alice.save_config().unwrap();
    assert!(Workspace::sync_and_record(&root).is_err());
    assert_eq!(Workspace::inspect(&root).unwrap().state.last_sync, before);
}

#[test]
fn a_workspace_without_a_remote_never_records_a_sync() {
    let temp = TempDir::new().unwrap();
    let mut alice = init(temp.path(), "Alice", None);
    fs::write(alice.root.join("note.md"), "local").unwrap();
    finish(&mut alice);
    let status = alice.status_snapshot();
    assert!(!status.has_remote);
    assert_eq!(status.last_sync, None);
}

#[test]
fn session_status_carries_times_purposes_and_the_published_title() {
    let temp = TempDir::new().unwrap();
    let origin = temp.path().join("remote.git");
    remote(&origin);
    let mut alice = init(&temp.path().join("alice"), "Alice", Some(&origin));
    let mut bob = init(&temp.path().join("bob"), "Bob", Some(&origin));
    for purpose in ["Explain setup", "Fix typos", "Explain setup"] {
        let intent = alice
            .register_intent(purpose.into(), BTreeSet::from(["guide.md".into()]))
            .unwrap();
        fs::write(alice.root.join("guide.md"), purpose).unwrap();
        alice.end_intent(&intent).unwrap();
    }
    let id = alice.active_session().unwrap().id.clone();
    // Intents carry whole seconds; give each its own so the recorded order shows.
    let base = now() - 100;
    for intent in alice
        .state
        .manifest
        .sessions
        .get_mut(&id)
        .unwrap()
        .intents
        .values_mut()
    {
        intent.created_at = base + if intent.purpose == "Fix typos" { 0 } else { 10 };
    }
    alice.save().unwrap();
    let stored = alice.state.manifest.sessions[&id].clone();
    let status = alice.status_snapshot();
    let session = status.sessions.iter().find(|s| s.id == id).unwrap();
    assert_eq!(session.started_at, stored.created_at);
    assert_eq!(session.last_saved, stored.last_edit);
    assert_eq!(
        session.purposes,
        ["Fix typos", "Explain setup"],
        "recorded order, without repeats"
    );
    assert_eq!(session.title, None, "open sessions have no title");

    alice.sync().unwrap();
    bob.sync().unwrap();
    let received = bob.status_snapshot();
    let session = received.sessions.iter().find(|s| s.id == id).unwrap();
    assert_eq!(session.author.name, "Alice", "never the receiving user");
    assert_eq!(session.purposes, ["Fix typos", "Explain setup"]);

    finish(&mut alice);
    let status = alice.status_snapshot();
    let session = status.sessions.iter().find(|s| s.id == id).unwrap();
    assert!(session.published);
    let message = alice.state.manifest.sessions[&id].message.clone().unwrap();
    assert_eq!(session.title.as_deref(), Some(message.title.as_str()));
}

/// A workspace whose next open displaces `note.md` ("before") to write "incoming".
fn displacing(temp: &TempDir) -> std::path::PathBuf {
    let mut workspace = init(temp.path(), "Alice", None);
    fs::write(workspace.root.join("note.md"), "before").unwrap();
    workspace.capture_at(now()).unwrap();
    workspace.state.applying = Some(Files::from([("note.md".into(), b"incoming".to_vec())]));
    workspace.save().unwrap();
    workspace.root.clone()
}

#[test]
fn a_displaced_file_is_listed_as_a_saved_copy() {
    let temp = TempDir::new().unwrap();
    let root = displacing(&temp);
    let workspace = Workspace::open(&root).unwrap();
    let copies = workspace.status_snapshot().saved_copies;
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(copies[0].path, "note.md");
    assert!(now().saturating_sub(copies[0].created_at) < 5);
    let content = root
        .join(".mesh/recovery")
        .join(&copies[0].id)
        .join("content");
    assert_eq!(fs::read(content).unwrap(), b"before");
    drop(workspace);
    assert_eq!(
        Workspace::inspect(&root)
            .unwrap()
            .status_snapshot()
            .saved_copies,
        copies,
        "recorded in state"
    );
}

#[test]
fn saved_copies_from_an_earlier_version_are_listed_once_on_open() {
    let temp = TempDir::new().unwrap();
    let root = displacing(&temp);
    let workspace = Workspace::open(&root).unwrap();
    let recorded = workspace.state.saved_copies.clone().unwrap();
    drop(workspace);
    // A not-a-copy folder next to it, as left by hand or by a test.
    fs::create_dir_all(root.join(".mesh/recovery/not-an-id")).unwrap();
    // Forget the record, as a 0.2 workspace never had one.
    let mut workspace = Workspace::open(&root).unwrap();
    workspace.state.saved_copies = None;
    workspace.save().unwrap();
    drop(workspace);
    assert_eq!(
        Workspace::inspect(&root)
            .unwrap()
            .status_snapshot()
            .saved_copies,
        recorded,
        "a read-only status reads the folder"
    );
    let workspace = Workspace::open(&root).unwrap();
    assert_eq!(workspace.state.saved_copies.as_ref(), Some(&recorded));
}

/// The durable state a crash leaves when it interrupts displacing `note.md`
/// ("before") to write "incoming": the copy is recorded in state and in its
/// `record.json`, and the file was moved into it only if `moved`.
fn interrupted_displacement(temp: &TempDir, moved: bool) -> (std::path::PathBuf, String) {
    let mut workspace = init(temp.path(), "Alice", None);
    fs::write(workspace.root.join("note.md"), "before").unwrap();
    workspace.capture_at(now()).unwrap();
    workspace.state.applying = Some(Files::from([("note.md".into(), b"incoming".to_vec())]));
    let id = uuid::Uuid::new_v4().to_string();
    let recovery = workspace.root.join(".mesh/recovery").join(&id);
    fs::create_dir_all(&recovery).unwrap();
    fs::write(
        recovery.join("record.json"),
        serde_json::json!({"path": "note.md", "created_at": now()}).to_string(),
    )
    .unwrap();
    workspace
        .state
        .apply_backups
        .insert("note.md".into(), id.clone());
    workspace
        .state
        .saved_copies
        .get_or_insert_with(Vec::new)
        .push(SavedCopy {
            id: id.clone(),
            path: "note.md".into(),
            created_at: now(),
        });
    workspace.save().unwrap();
    if moved {
        fs::rename(workspace.root.join("note.md"), recovery.join("content")).unwrap();
    }
    (workspace.root.clone(), id)
}

/// Every listed copy holds content, in state and in a read-only status.
fn assert_copies_exist(root: &Path) -> Vec<SavedCopy> {
    let workspace = Workspace::open(root).unwrap();
    assert!(workspace.state.applying.is_none() && workspace.state.apply_backups.is_empty());
    let copies = workspace.status_snapshot().saved_copies;
    for copy in &copies {
        assert!(
            root.join(".mesh/recovery")
                .join(&copy.id)
                .join("content")
                .is_file(),
            "{copy:?} lists a copy that does not exist"
        );
    }
    drop(workspace);
    assert_eq!(
        Workspace::inspect(root)
            .unwrap()
            .status_snapshot()
            .saved_copies,
        copies
    );
    copies
}

#[test]
fn a_crash_before_the_move_then_a_newer_save_lists_no_phantom_copy() {
    let temp = TempDir::new().unwrap();
    let (root, id) = interrupted_displacement(&temp, false);
    fs::write(root.join("note.md"), "newer local edit").unwrap();
    let copies = assert_copies_exist(&root);
    assert!(copies.is_empty(), "{copies:?}");
    assert_eq!(
        fs::read_to_string(root.join("note.md")).unwrap(),
        "newer local edit",
        "the newer save survives"
    );
    assert!(
        root.join(".mesh/recovery")
            .join(&id)
            .join("record.json")
            .is_file(),
        "recovery files are never deleted"
    );
}

#[test]
fn a_crash_before_the_move_with_the_file_already_written_lists_no_phantom_copy() {
    let temp = TempDir::new().unwrap();
    let (root, _) = interrupted_displacement(&temp, false);
    fs::write(root.join("note.md"), "incoming").unwrap();
    assert!(assert_copies_exist(&root).is_empty());
    assert_eq!(fs::read(root.join("note.md")).unwrap(), b"incoming");
}

#[test]
fn a_crash_before_the_move_then_a_replay_lists_only_the_copy_it_made() {
    let temp = TempDir::new().unwrap();
    let (root, abandoned) = interrupted_displacement(&temp, false);
    let copies = assert_copies_exist(&root);
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_ne!(copies[0].id, abandoned);
    assert_eq!(fs::read(root.join("note.md")).unwrap(), b"incoming");
    let content = root
        .join(".mesh/recovery")
        .join(&copies[0].id)
        .join("content");
    assert_eq!(fs::read(content).unwrap(), b"before");
}

#[test]
fn a_crash_after_the_move_keeps_the_real_copy() {
    let temp = TempDir::new().unwrap();
    let (root, id) = interrupted_displacement(&temp, true);
    let copies = assert_copies_exist(&root);
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(copies[0].id, id);
    assert_eq!(fs::read(root.join("note.md")).unwrap(), b"incoming");
    let content = root.join(".mesh/recovery").join(&id).join("content");
    assert_eq!(fs::read(content).unwrap(), b"before");
}

#[test]
fn a_legacy_record_without_content_is_not_backfilled() {
    let temp = TempDir::new().unwrap();
    let root = displacing(&temp);
    let real = Workspace::open(&root).unwrap().state.saved_copies.unwrap();
    // An older version stopped between writing the record and moving the file.
    let empty = root
        .join(".mesh/recovery")
        .join(uuid::Uuid::new_v4().to_string());
    fs::create_dir_all(&empty).unwrap();
    fs::write(
        empty.join("record.json"),
        serde_json::json!({"path": "other.md", "created_at": now()}).to_string(),
    )
    .unwrap();
    let mut workspace = Workspace::open(&root).unwrap();
    workspace.state.saved_copies = None;
    workspace.save().unwrap();
    drop(workspace);
    assert_eq!(
        Workspace::inspect(&root)
            .unwrap()
            .status_snapshot()
            .saved_copies,
        real,
        "a read-only status skips it too"
    );
    assert_eq!(assert_copies_exist(&root), real);
    assert!(empty.join("record.json").is_file(), "left in place");
}
