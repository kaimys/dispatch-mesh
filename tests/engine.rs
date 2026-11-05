use dispatch_mesh::{
    ai,
    engine::Workspace,
    git::{Git, validate_path},
    model::*,
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
fn changing_ignore_rules_does_not_publish_a_deletion_of_existing_content() {
    let temp = TempDir::new().unwrap();
    let mut workspace = init(temp.path(), "Alice", None);
    fs::write(workspace.root.join("note.md"), "keep this").unwrap();
    workspace.capture_at(now()).unwrap();
    fs::write(workspace.root.join(".meshignore"), "note.md\n").unwrap();
    assert!(workspace.capture_at(now()).is_err());
    assert_eq!(workspace.state.observed["note.md"], b"keep this");
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
    for key in ["uploaded", "offline", "summary_failure"] {
        assert!(object.remove(key).is_some(), "{key} is serialized");
    }
    let state: State = serde_json::from_value(value).unwrap();
    assert!(state.uploaded.is_empty());
    assert!(state.offline.is_none() && state.summary_failure.is_none());
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
