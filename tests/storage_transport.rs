use dispatch_mesh::{engine::Workspace, git::Git, model::*, storage, transport};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};

fn init(root: &Path, name: &str, remote: Option<&Path>) -> Workspace {
    Workspace::init(
        root,
        Identity {
            name: name.into(),
            email: format!("{name}@example.test"),
        },
        remote.map(|p| p.to_string_lossy().into_owned()),
        "main".into(),
    )
    .unwrap()
}

fn fixture(workspace: &mut Workspace) -> Manifest {
    fs::write(workspace.root.join("note.md"), "Readable draft\n").unwrap();
    workspace.capture_at(now()).unwrap();
    let mut session = workspace.active_session().unwrap().clone();
    session.changes.insert(
        "deleted.md".into(),
        Change {
            base: Some(b"deleted original".to_vec()),
            content: None,
            intent: None,
            last_edit: now(),
        },
    );
    session.changes.insert(
        "empty.bin".into(),
        Change {
            base: None,
            content: Some(vec![]),
            intent: None,
            last_edit: now(),
        },
    );
    let id = digest("conflict fixture");
    let conflict = Conflict {
        id: id.clone(),
        path: "image.bin".into(),
        base: Some(vec![0, 255, 1]),
        accepted: Some(vec![0, 255, 2]),
        incoming: Some(vec![0, 255, 3]),
        session: session.id.clone(),
        author: workspace.config.identity.clone(),
        binary: true,
        accepted_sessions: BTreeMap::new(),
        created_at: now(),
    };
    let resolution = Resolution {
        conflict: id.clone(),
        selected: conflict.incoming.clone(),
        author: workspace.config.identity.clone(),
        created_at: now(),
    };
    Manifest {
        version: 2,
        sessions: BTreeMap::from([(session.id.clone(), session)]),
        conflicts: BTreeMap::from([(id.clone(), conflict)]),
        resolutions: BTreeMap::from([(id, resolution)]),
    }
}

#[test]
fn device_branch_contains_readable_files_and_metadata_references_before_publication() {
    let temp = tempfile::tempdir().unwrap();
    let origin = temp.path().join("origin.git");
    let git = Git {
        dir: origin.clone(),
    };
    git.init().unwrap();
    let mut alice = init(&temp.path().join("alice"), "Alice", Some(&origin));
    fs::create_dir(alice.root.join("notes")).unwrap();
    fs::write(
        alice.root.join("notes/test.md"),
        "Hello from a live draft\n",
    )
    .unwrap();
    fs::write(alice.root.join("asset.bin"), [0, 255, 10, 13]).unwrap();
    alice.sync().unwrap();
    let session = alice.active_session().unwrap();
    let id = session.id.clone();
    let reference = format!("refs/heads/mesh/devices/{}", alice.config.device);
    let files = git.files(&reference).unwrap();
    assert_eq!(
        files[&format!("drafts/{id}/files/notes/test.md")],
        b"Hello from a live draft\n"
    );
    assert_eq!(
        files[&format!("drafts/{id}/files/asset.bin")],
        vec![0, 255, 10, 13]
    );
    let metadata: Value = serde_json::from_slice(&files["state.json"]).unwrap();
    assert_eq!(metadata["version"], 2);
    assert!(metadata["sessions"][&id]["changes"]["asset.bin"]["content"]["blob"].is_string());
    assert!(!metadata.to_string().contains("Hello from a live draft"));
    git.run(&["gc", "--prune=now"], None).unwrap();
    let mut bob = init(&temp.path().join("bob"), "Bob", Some(&origin));
    bob.sync().unwrap();
    assert_eq!(
        fs::read(bob.root.join("notes/test.md")).unwrap(),
        b"Hello from a live draft\n"
    );
    assert_eq!(
        fs::read(bob.root.join("asset.bin")).unwrap(),
        vec![0, 255, 10, 13]
    );
    assert!(bob.active_session().is_none());
    assert!(!bob.state.manifest.sessions[&id].published);
}

#[test]
fn native_snapshots_keep_deleted_empty_and_conflicting_versions_reachable() {
    let temp = tempfile::tempdir().unwrap();
    let mut workspace = init(temp.path(), "Alice", None);
    let manifest = fixture(&mut workspace);
    let entries = transport::encode(&workspace.git, manifest.clone()).unwrap();
    let commit = workspace
        .git
        .commit_objects(
            &entries,
            None,
            &workspace.config.identity,
            "Native snapshot",
        )
        .unwrap();
    workspace
        .git
        .run(&["update-ref", "refs/heads/test-snapshot", &commit], None)
        .unwrap();
    workspace.git.run(&["gc", "--prune=now"], None).unwrap();
    let decoded = transport::decode(&workspace.git, &commit, 1024).unwrap();
    assert_eq!(
        serde_json::to_value(decoded).unwrap(),
        serde_json::to_value(&manifest).unwrap()
    );
    let session = manifest.sessions.values().next().unwrap();
    assert!(!entries.contains_key(&format!("drafts/{}/files/deleted.md", session.id)));
    assert!(entries.contains_key(&format!("drafts/{}/base/deleted.md", session.id)));
    let conflict = manifest.conflicts.values().next().unwrap();
    assert_eq!(
        entries[&format!("conflicts/{}/incoming", conflict.id)],
        entries[&format!("resolutions/{}/selected", conflict.id)]
    );
}

#[test]
fn remote_metadata_cannot_reference_unshared_local_objects() {
    let temp = tempfile::tempdir().unwrap();
    let mut workspace = init(temp.path(), "Alice", None);
    let manifest = fixture(&mut workspace);
    let id = manifest.sessions.keys().next().unwrap().clone();
    let mut entries = transport::encode(&workspace.git, manifest).unwrap();
    let private_blob = workspace
        .git
        .object(b"not part of the remote snapshot")
        .unwrap();
    let mut metadata: Value = serde_json::from_slice(
        &workspace
            .git
            .read_blob(&entries["state.json"], 1024 * 1024)
            .unwrap(),
    )
    .unwrap();
    metadata["sessions"][&id]["changes"]["note.md"]["content"] = json!({"blob": private_blob});
    entries.insert(
        "state.json".into(),
        workspace
            .git
            .object(&serde_json::to_vec(&metadata).unwrap())
            .unwrap(),
    );
    let commit = workspace
        .git
        .commit_objects(
            &entries,
            None,
            &workspace.config.identity,
            "Invalid snapshot",
        )
        .unwrap();
    let error = transport::decode(&workspace.git, &commit, 1024).unwrap_err();
    assert!(error.to_string().contains("outside its file tree"));
}

#[test]
fn legacy_device_snapshots_are_still_readable() {
    let temp = tempfile::tempdir().unwrap();
    let mut workspace = init(temp.path(), "Alice", None);
    let mut manifest = fixture(&mut workspace);
    manifest.version = 1;
    let files = Files::from([("state.json".into(), serde_json::to_vec(&manifest).unwrap())]);
    let commit = workspace
        .git
        .commit(&files, None, &workspace.config.identity, "Legacy snapshot")
        .unwrap();
    let decoded = transport::decode(&workspace.git, &commit, 1024).unwrap();
    assert_eq!(
        serde_json::to_value(decoded).unwrap(),
        serde_json::to_value(manifest).unwrap()
    );
}

#[test]
fn local_state_uses_deduplicated_blobs_for_all_recovery_fields() {
    let temp = tempfile::tempdir().unwrap();
    let mut workspace = init(temp.path(), "Alice", None);
    let manifest = fixture(&mut workspace);
    let mut state = workspace.state.clone();
    state.conflicts = manifest.conflicts.clone();
    state.manifest = manifest;
    state.observed.insert("image.bin".into(), vec![0, 255, 3]);
    state.applying = Some(state.observed.clone());
    state.pending_capture = Some(state.observed.clone());
    let db = Connection::open_in_memory().unwrap();
    storage::initialize(&db).unwrap();
    storage::save(&db, &state).unwrap();
    let value: String = db
        .query_row("SELECT value FROM state", [], |row| row.get(0))
        .unwrap();
    let metadata: Value = serde_json::from_str(&value).unwrap();
    assert_eq!(metadata["format"], 2);
    for field in ["observed", "applying", "pending_capture"] {
        assert!(metadata["state"][field]["image.bin"]["blob"].is_string());
    }
    let selected = &metadata["state"]["observed"]["image.bin"]["blob"];
    assert_eq!(
        db.query_row::<String, _, _>(
            "SELECT typeof(content) FROM blobs WHERE id=?1",
            [selected.as_str().unwrap()],
            |row| row.get(0)
        )
        .unwrap(),
        "blob"
    );
    assert_eq!(
        serde_json::to_value(storage::load(&db).unwrap()).unwrap(),
        serde_json::to_value(state).unwrap()
    );
    let count: u64 = db
        .query_row("SELECT count(*) FROM blobs", [], |row| row.get(0))
        .unwrap();
    storage::save(&db, &storage::load(&db).unwrap()).unwrap();
    assert_eq!(
        db.query_row::<u64, _, _>("SELECT count(*) FROM blobs", [], |row| row.get(0))
            .unwrap(),
        count
    );
}

#[test]
fn opening_a_legacy_workspace_migrates_without_changing_session_identity() {
    let temp = tempfile::tempdir().unwrap();
    let mut workspace = init(temp.path(), "Alice", None);
    fs::write(workspace.root.join("note.md"), "legacy saved work").unwrap();
    workspace.capture_at(now()).unwrap();
    let before = workspace.state.clone();
    let session = workspace.active_session().unwrap().clone();
    let legacy = serde_json::to_string(&before).unwrap();
    drop(workspace);
    let db = Connection::open(temp.path().join(".mesh/state.sqlite")).unwrap();
    db.execute("UPDATE state SET value=?1 WHERE id=1", [&legacy])
        .unwrap();
    db.execute_batch("DROP TABLE blobs; DROP TABLE state_migrations;")
        .unwrap();
    assert_eq!(
        Workspace::inspect(temp.path())
            .unwrap()
            .active_session()
            .unwrap()
            .id,
        session.id
    );
    let workspace = Workspace::open(temp.path()).unwrap();
    assert_eq!(workspace.active_session().unwrap().id, session.id);
    assert_eq!(
        workspace.active_session().unwrap().snapshot_hash(),
        session.snapshot_hash()
    );
    assert_eq!(
        serde_json::to_value(&workspace.state).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    let saved: String = db
        .query_row("SELECT value FROM state", [], |row| row.get(0))
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&saved).unwrap()["format"], 2);
    assert_eq!(
        db.query_row::<String, _, _>(
            "SELECT value FROM state_migrations WHERE format=1",
            [],
            |row| row.get(0)
        )
        .unwrap(),
        legacy
    );
}

#[test]
fn failed_blob_migration_leaves_legacy_state_intact() {
    let db = Connection::open_in_memory().unwrap();
    storage::initialize(&db).unwrap();
    let mut state = State::default();
    state
        .observed
        .insert("note.md".into(), b"valuable work".to_vec());
    let legacy = serde_json::to_string(&state).unwrap();
    db.execute("INSERT INTO state(id,value) VALUES(1,?1)", [&legacy])
        .unwrap();
    db.execute_batch("CREATE TRIGGER fail_blob BEFORE INSERT ON blobs BEGIN SELECT RAISE(ABORT,'simulated storage failure'); END;").unwrap();
    assert!(storage::save(&db, &state).is_err());
    assert_eq!(
        db.query_row::<String, _, _>("SELECT value FROM state", [], |row| row.get(0))
            .unwrap(),
        legacy
    );
    assert_eq!(storage::load(&db).unwrap().observed, state.observed);
    assert_eq!(
        db.query_row::<u64, _, _>("SELECT count(*) FROM blobs", [], |row| row.get(0))
            .unwrap(),
        0
    );
}

#[test]
fn legacy_draft_upgrades_and_finalizes_once_without_rewriting_its_history() {
    let temp = tempfile::tempdir().unwrap();
    let origin = temp.path().join("origin.git");
    let remote = Git {
        dir: origin.clone(),
    };
    remote.init().unwrap();
    let root = temp.path().join("alice");
    let mut alice = init(&root, "Alice", Some(&origin));
    fs::write(
        alice.root.join("test.md"),
        "Existing draft survives upgrade\n",
    )
    .unwrap();
    alice.capture_at(now()).unwrap();
    let id = alice.active_session().unwrap().id.clone();
    let fingerprint = alice.active_session().unwrap().snapshot_hash();
    let reference = format!("refs/heads/mesh/devices/{}", alice.config.device);
    let legacy_files = Files::from([(
        "state.json".into(),
        serde_json::to_vec(&alice.state.manifest).unwrap(),
    )]);
    let legacy_commit = alice
        .git
        .commit(
            &legacy_files,
            None,
            &alice.config.identity,
            "Version-one draft",
        )
        .unwrap();
    alice
        .git
        .run(&["update-ref", &reference, &legacy_commit], None)
        .unwrap();
    alice
        .git
        .run(
            &["push", "origin", &format!("{reference}:{reference}")],
            None,
        )
        .unwrap();
    let legacy_state = serde_json::to_string(&alice.state).unwrap();
    drop(alice);
    let db = Connection::open(root.join(".mesh/state.sqlite")).unwrap();
    db.execute("UPDATE state SET value=?1 WHERE id=1", [legacy_state])
        .unwrap();
    drop(db);
    let mut alice = Workspace::open(&root).unwrap();
    assert_eq!(alice.active_session().unwrap().snapshot_hash(), fingerprint);
    alice.sync().unwrap();
    let native = remote.text(&["rev-parse", &reference]).unwrap();
    assert_eq!(
        remote.text(&["rev-parse", &format!("{native}^")]).unwrap(),
        legacy_commit
    );
    assert_eq!(
        transport::decode(&remote, &native, 1024).unwrap().version,
        2
    );
    alice.finish().unwrap();
    let prepared = alice.summary_input(&id).unwrap().unwrap();
    let session = alice.state.manifest.sessions.get_mut(&id).unwrap();
    session.message = Some(dispatch_mesh::ai::fallback(&prepared));
    session.message_base = prepared.message_base;
    alice.save().unwrap();
    alice.sync().unwrap();
    let main = alice.state.main.clone();
    assert!(alice.state.manifest.sessions[&id].published);
    assert_eq!(
        remote.files("main").unwrap()["test.md"],
        b"Existing draft survives upgrade\n"
    );
    let marker = format!("Mesh-Session: {id}");
    assert_eq!(
        remote
            .text(&["log", "--format=%B", "main"])
            .unwrap()
            .matches(&marker)
            .count(),
        1
    );
    let snapshot = remote.text(&["rev-parse", &reference]).unwrap();
    alice.sync().unwrap();
    assert_eq!(alice.state.main, main);
    assert_eq!(remote.text(&["rev-parse", &reference]).unwrap(), snapshot);
    let mut bob = init(&temp.path().join("bob"), "Bob", Some(&origin));
    bob.sync().unwrap();
    assert_eq!(
        fs::read(bob.root.join("test.md")).unwrap(),
        b"Existing draft survives upgrade\n"
    );
    assert!(bob.active_session().is_none());
}
