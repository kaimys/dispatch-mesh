//! Conflict list metadata and on-demand detail (US00006).

use dispatch_mesh::{
    ai,
    engine::Workspace,
    git::Git,
    model::{Identity, now},
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

const BASE_NOTE: &str = "one\ntwo\nthree\n";

/// Alice and Bob share `note.md`, then both change its first line. Alice's
/// edit carries a requested purpose. Bob syncs last, so his folder holds his
/// own version (the current one) and Alice's is kept aside (the other one).
// Sessions render in order of their first save, and the earliest one is the
// current version: Bob saves first, Alice five seconds later.
fn text_conflict(temp: &TempDir) -> (Workspace, Workspace) {
    let origin = temp.path().join("remote.git");
    remote(&origin);
    let mut alice = init(&temp.path().join("alice"), "Alice", Some(&origin));
    fs::write(alice.root.join("note.md"), BASE_NOTE).unwrap();
    finish(&mut alice);
    let mut bob = init(&temp.path().join("bob"), "Bob", Some(&origin));
    bob.sync().unwrap();
    let intent = alice
        .register_intent(
            "Reword the opening".into(),
            BTreeSet::from(["note.md".into()]),
        )
        .unwrap();
    fs::write(alice.root.join("note.md"), "ALICE\ntwo\nthree\n").unwrap();
    alice.capture_at(now() + 5).unwrap();
    alice.end_intent(&intent).unwrap();
    fs::write(bob.root.join("note.md"), "BOB\ntwo\nthree\n").unwrap();
    bob.capture_at(now()).unwrap();
    alice.sync().unwrap();
    bob.sync().unwrap();
    assert_eq!(bob.unresolved().count(), 1, "Bob holds the conflict");
    (alice, bob)
}

#[test]
fn status_lists_both_versions_of_a_text_conflict_with_their_authors() {
    let temp = TempDir::new().unwrap();
    let (alice, bob) = text_conflict(&temp);
    let status = bob.status_snapshot();
    let [conflict] = status.unresolved_conflicts.as_slice() else {
        panic!("{:?}", status.unresolved_conflicts)
    };
    let record = bob.unresolved().next().unwrap();
    assert_eq!(
        (conflict.path.as_str(), conflict.binary),
        ("note.md", false)
    );
    assert_eq!(conflict.created_at, record.created_at);

    assert!(!conflict.current.shared_history && !conflict.current.deleted);
    assert_eq!(conflict.current.size, "BOB\ntwo\nthree\n".len() as u64);
    let [current] = conflict.current.contributors.as_slice() else {
        panic!("{:?}", conflict.current)
    };
    assert_eq!(current.author, identity("Bob"));
    assert_eq!(current.device, bob.config.device);
    assert!(current.last_saved > 0 && current.purposes.is_empty());

    let [other] = conflict.other.contributors.as_slice() else {
        panic!("{:?}", conflict.other)
    };
    assert_eq!(other.author, identity("Alice"));
    assert_eq!(other.device, alice.config.device);
    assert_eq!(other.purposes, ["Reword the opening"]);
    assert_eq!(conflict.other.size, "ALICE\ntwo\nthree\n".len() as u64);
}

#[test]
fn a_purpose_only_counts_for_the_files_its_intent_covers() {
    let temp = TempDir::new().unwrap();
    let origin = temp.path().join("remote.git");
    remote(&origin);
    let mut alice = init(&temp.path().join("alice"), "Alice", Some(&origin));
    fs::write(alice.root.join("note.md"), BASE_NOTE).unwrap();
    finish(&mut alice);
    let mut bob = init(&temp.path().join("bob"), "Bob", Some(&origin));
    bob.sync().unwrap();
    let intent = alice
        .register_intent("Fix other".into(), BTreeSet::from(["other.md".into()]))
        .unwrap();
    fs::write(alice.root.join("other.md"), "x").unwrap();
    alice.capture_at(now() + 5).unwrap();
    alice.end_intent(&intent).unwrap();
    fs::write(alice.root.join("note.md"), "ALICE\ntwo\nthree\n").unwrap();
    fs::write(bob.root.join("note.md"), "BOB\ntwo\nthree\n").unwrap();
    alice.capture_at(now() + 5).unwrap();
    bob.capture_at(now()).unwrap();
    alice.sync().unwrap();
    bob.sync().unwrap();
    let status = bob.status_snapshot();
    let other = &status.unresolved_conflicts[0].other.contributors[0];
    assert!(other.purposes.is_empty(), "{other:?}");
}

#[test]
fn status_marks_binary_sizes_and_a_deleted_version() {
    let temp = TempDir::new().unwrap();
    let origin = temp.path().join("remote.git");
    remote(&origin);
    let mut alice = init(&temp.path().join("alice"), "Alice", Some(&origin));
    fs::write(alice.root.join("logo.png"), [0, 1, 2]).unwrap();
    fs::write(alice.root.join("note.md"), BASE_NOTE).unwrap();
    finish(&mut alice);
    let mut bob = init(&temp.path().join("bob"), "Bob", Some(&origin));
    bob.sync().unwrap();
    fs::write(alice.root.join("logo.png"), [0, 1, 2, 3, 4]).unwrap();
    fs::remove_file(alice.root.join("note.md")).unwrap();
    fs::write(bob.root.join("logo.png"), [0, 9]).unwrap();
    fs::write(bob.root.join("note.md"), "BOB\ntwo\nthree\n").unwrap();
    alice.capture_at(now() + 5).unwrap();
    bob.capture_at(now()).unwrap();
    alice.sync().unwrap();
    bob.sync().unwrap();
    let status = bob.status_snapshot();
    let by_path = |path: &str| {
        status
            .unresolved_conflicts
            .iter()
            .find(|c| c.path == path)
            .unwrap_or_else(|| panic!("{path}: {:?}", status.unresolved_conflicts))
    };
    let logo = by_path("logo.png");
    assert!(logo.binary);
    assert_eq!((logo.current.size, logo.other.size), (2, 5));
    let note = by_path("note.md");
    assert!(note.other.deleted && !note.current.deleted, "{note:?}");
    assert_eq!(note.other.size, 0);
}

#[test]
fn a_current_version_taken_from_shared_history_has_no_contributors() {
    let temp = TempDir::new().unwrap();
    let origin = temp.path().join("remote.git");
    remote(&origin);
    let mut alice = init(&temp.path().join("alice"), "Alice", Some(&origin));
    fs::write(alice.root.join("note.md"), BASE_NOTE).unwrap();
    finish(&mut alice);
    let mut bob = init(&temp.path().join("bob"), "Bob", Some(&origin));
    let mut carol = init(&temp.path().join("carol"), "Carol", Some(&origin));
    bob.sync().unwrap();
    carol.sync().unwrap();
    fs::write(alice.root.join("note.md"), "ALICE\ntwo\nthree\n").unwrap();
    finish(&mut alice);
    // Carol edited the same line from the old base and has not synced since.
    fs::write(carol.root.join("note.md"), "CAROL\ntwo\nthree\n").unwrap();
    carol.capture_at(now()).unwrap();
    // Bob has no session of his own: he receives Alice's published change,
    // then Carol's draft.
    bob.sync().unwrap();
    carol.sync().unwrap();
    bob.sync().unwrap();
    let Some(conflict) = bob.unresolved().next().cloned() else {
        panic!("no conflict on Bob: {:?}", bob.status_snapshot().sessions)
    };
    let status = bob.status_snapshot();
    let listed = &status.unresolved_conflicts[0];
    assert!(conflict.accepted_sessions.is_empty(), "{conflict:?}");
    assert!(listed.current.shared_history);
    assert!(listed.current.contributors.is_empty());
    assert_eq!(listed.other.contributors[0].author, identity("Carol"));

    // The detail names the commit that made the current version.
    let detail = bob.conflict_detail(&conflict.id).unwrap();
    let history = detail.current_history.expect("main's commit made it");
    assert_eq!(history.author, identity("Alice"));
    assert!(history.time > 0);
    assert_eq!(detail.current.as_deref(), Some(&b"ALICE\ntwo\nthree\n"[..]));
}

#[test]
fn conflict_detail_returns_the_exact_versions_and_the_base_history() {
    let temp = TempDir::new().unwrap();
    let (_alice, bob) = text_conflict(&temp);
    let id = bob.unresolved().next().unwrap().id.clone();
    let detail = bob.conflict_detail(&id).unwrap();
    assert_eq!(detail.base.as_deref(), Some(BASE_NOTE.as_bytes()));
    assert_eq!(detail.current.as_deref(), Some(&b"BOB\ntwo\nthree\n"[..]));
    assert_eq!(detail.other.as_deref(), Some(&b"ALICE\ntwo\nthree\n"[..]));
    assert_eq!(detail.status, bob.status_snapshot().unresolved_conflicts[0]);
    assert_eq!(detail.edited_after, None);
    assert_eq!(bob.conflict_edited_after(&id).unwrap(), None);
    // Bob's version is a draft, so no commit on main made it.
    assert_eq!(detail.current_history, None);
    // The base is the commit Alice published first.
    let base = detail.base_history.expect("the base is on main");
    assert_eq!(base.author, identity("Alice"));
    assert!(bob.conflict_detail("missing").is_err());
}

#[test]
fn history_is_only_named_when_the_commit_holds_exactly_that_version() {
    let temp = TempDir::new().unwrap();
    let (_alice, mut bob) = text_conflict(&temp);
    let id = bob.unresolved().next().unwrap().id.clone();
    // Pretend the base is something no commit holds.
    bob.state.conflicts.get_mut(&id).unwrap().base = Some(b"not in history".to_vec());
    let detail = bob.conflict_detail(&id).unwrap();
    assert_eq!(detail.base_history, None);
}

#[test]
fn detail_reports_a_later_save_and_resolving_is_then_refused() {
    let temp = TempDir::new().unwrap();
    let (_alice, mut bob) = text_conflict(&temp);
    let id = bob.unresolved().next().unwrap().id.clone();
    let created = bob.state.conflicts[&id].created_at;
    fs::write(bob.root.join("note.md"), "BOB again\ntwo\nthree\n").unwrap();
    bob.capture_at(created + 30).unwrap();
    let detail = bob.conflict_detail(&id).unwrap();
    assert_eq!(detail.edited_after, Some(created + 30));
    assert_eq!(bob.conflict_edited_after(&id).unwrap(), Some(created + 30));
    let error = bob.resolve(&id, true).unwrap_err();
    assert!(format!("{error:#}").contains("newer edits"), "{error:#}");
    assert_eq!(bob.unresolved().count(), 1, "nothing was recorded");
}

#[test]
fn a_resolved_conflict_has_no_detail() {
    let temp = TempDir::new().unwrap();
    let (_alice, mut bob) = text_conflict(&temp);
    let id = bob.unresolved().next().unwrap().id.clone();
    bob.resolve(&id, false).unwrap();
    assert!(bob.conflict_detail(&id).is_err());
    assert!(bob.conflict_edited_after(&id).is_err());
    assert!(bob.status_snapshot().unresolved_conflicts.is_empty());
}

#[test]
fn status_json_keeps_the_existing_conflict_keys_and_names_sides_current_and_other() {
    let temp = TempDir::new().unwrap();
    let (_alice, bob) = text_conflict(&temp);
    let json = serde_json::to_value(bob.status_snapshot()).unwrap();
    let conflict = &json["unresolved_conflicts"][0];
    for key in ["id", "path", "binary"] {
        assert!(!conflict[key].is_null(), "{key} stays: {conflict}");
    }
    assert!(conflict["created_at"].is_u64());
    for side in ["current", "other"] {
        let side = &conflict[side];
        for key in ["contributors", "shared_history", "deleted", "size"] {
            assert!(!side[key].is_null(), "{key}: {side}");
        }
        let person = &side["contributors"][0];
        for key in ["author", "device", "last_saved", "purposes"] {
            assert!(!person[key].is_null(), "{key}: {person}");
        }
    }
    let text = conflict.to_string();
    assert!(
        !text.contains("accepted") && !text.contains("incoming"),
        "{text}"
    );
}
