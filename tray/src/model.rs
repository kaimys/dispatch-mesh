//! What the tray and the status window say, decided in one place (US00001).
//!
//! Pure functions over the workspaces a host runs: labels, the tray icon,
//! how several workspaces sum up, the tooltip and the window's view model.
//! Nothing here touches Tauri, so every rule is unit-tested.

use dispatch_mesh::{
    engine::{RECEIVED_DRAFT_DEPENDENCY, SYNC_FAILURE},
    runner::RunnerSnapshot,
    status::{SessionStatus, Stage, Status, Summary},
};
use serde::Serialize;
use std::path::PathBuf;

/// Windows stores a tray tooltip in 128 UTF-16 units, including the terminator.
pub const TOOLTIP_LIMIT: usize = 127;

const PRODUCT: &str = "Dispatch Mesh";

#[derive(Clone, Debug, PartialEq)]
pub enum WorkspaceState {
    /// The engine is being started.
    Starting,
    /// The engine runs here; `None` until its first snapshot.
    Running(Option<Box<RunnerSnapshot>>),
    /// Another runner (CLI `run` or another host) owns this workspace.
    AlreadyRunning,
    /// The engine is not running here: it failed to start or its loop ended.
    Stopped(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Workspace {
    pub id: usize,
    pub name: String,
    pub path: PathBuf,
    pub state: WorkspaceState,
}

/// The four tray icons (ADR-0010 states folded as decided in US00001).
/// Ordered by precedence: the smallest wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Icon {
    Attention,
    Offline,
    Working,
    UpToDate,
}

impl Icon {
    pub fn file_stem(self) -> &'static str {
        match self {
            Icon::Attention => "attention",
            Icon::Offline => "offline",
            Icon::Working => "working",
            Icon::UpToDate => "synced",
        }
    }
}

pub fn icon(summary: Summary) -> Icon {
    match summary {
        Summary::NeedsAttention => Icon::Attention,
        Summary::Offline => Icon::Offline,
        Summary::SavedLocally | Summary::DraftUploaded | Summary::WaitingForFinalization => {
            Icon::Working
        }
        Summary::Synced => Icon::UpToDate,
    }
}

/// The state's name in the status window.
pub fn label(summary: Summary) -> &'static str {
    match summary {
        Summary::NeedsAttention => "Needs attention",
        Summary::Offline => "Offline — changes saved on this device",
        Summary::SavedLocally => "Saved on this device",
        Summary::DraftUploaded => "Shared as draft",
        Summary::WaitingForFinalization => "Finishing up",
        Summary::Synced => "Up to date",
    }
}

/// The state's name in the tooltip, which already starts with "Dispatch Mesh —".
fn short_label(summary: Summary) -> &'static str {
    match summary {
        Summary::Offline => "Offline, changes saved on this device",
        other => label(other),
    }
}

const NOT_SYNCING: &str = "Not syncing";
const ALREADY_RUNNING: &str = "Mesh is already running for this workspace";
const STARTING: &str = "Starting";
pub const OFFLINE_NOTE: &str = "Keep working — your changes upload when the connection returns.";
pub const RETRY_NOTE: &str = "Mesh retries automatically.";
pub const FINISH_SESSION: &str = "Finish session";
const NOT_SYNCED_YET: &str = "Not synced yet";
const THIS_DEVICE_ONLY: &str = "This device only";
/// How many settled sessions the activity list keeps (US00005).
pub const SETTLED_SHOWN: usize = 10;

/// A session's stage in the window. Vocabulary from the design briefing §5.
pub fn stage_label(stage: Stage) -> &'static str {
    match stage {
        Stage::SavedLocally => "Saved on this device",
        Stage::DraftUploaded => "Shared as draft",
        Stage::WaitingForFinalization => "Finishing up",
        Stage::Published => "Added to shared history",
        Stage::Discarded => "No changes kept",
    }
}

/// The stage track a session moves along. A workspace without a remote has
/// no draft upload, so its track skips that step.
fn track(has_remote: bool) -> Vec<Stage> {
    let mut stages = vec![Stage::SavedLocally];
    if has_remote {
        stages.push(Stage::DraftUploaded);
    }
    stages.extend([Stage::WaitingForFinalization, Stage::Published]);
    stages
}

impl Workspace {
    fn summary(&self) -> Option<&RunnerSnapshot> {
        match &self.state {
            WorkspaceState::Running(Some(snapshot)) => Some(snapshot),
            _ => None,
        }
    }

    /// Its contribution to the tray icon. `None`: it does not count, because
    /// another runner keeps that folder syncing.
    pub fn icon(&self) -> Option<Icon> {
        match &self.state {
            WorkspaceState::Running(Some(snapshot)) => Some(icon(snapshot.status.status)),
            WorkspaceState::Starting | WorkspaceState::Running(None) => Some(Icon::Working),
            WorkspaceState::AlreadyRunning => None,
            WorkspaceState::Stopped(_) => Some(Icon::Attention),
        }
    }

    pub fn label(&self) -> &'static str {
        match &self.state {
            WorkspaceState::Running(Some(snapshot)) => label(snapshot.status.status),
            WorkspaceState::Starting | WorkspaceState::Running(None) => STARTING,
            WorkspaceState::AlreadyRunning => ALREADY_RUNNING,
            WorkspaceState::Stopped(_) => NOT_SYNCING,
        }
    }

    fn short_label(&self) -> &'static str {
        match self.summary() {
            Some(snapshot) => short_label(snapshot.status.status),
            None => self.label(),
        }
    }

    /// Why the workspace needs attention or is offline, or why it stopped.
    pub fn reason(&self) -> Option<&str> {
        match &self.state {
            WorkspaceState::Running(Some(snapshot)) => snapshot.status.reason.as_deref(),
            WorkspaceState::Stopped(reason) => Some(reason),
            _ => None,
        }
    }

    /// The workspace's latest status, when its engine runs here.
    pub fn status(&self) -> Option<&Status> {
        self.summary().map(|snapshot| &snapshot.status)
    }

    pub fn has_open_session(&self) -> bool {
        self.summary()
            .is_some_and(|snapshot| snapshot.status.active_session.is_some())
    }

    /// Who needs the user first: attention, then a refused start, then
    /// offline, working and up to date.
    fn urgency(&self) -> u8 {
        match self.icon() {
            Some(Icon::Attention) => 0,
            None => 1,
            Some(Icon::Offline) => 2,
            Some(Icon::Working) => 3,
            Some(Icon::UpToDate) => 4,
        }
    }
}

/// Workspaces in window order: most urgent first, then by name.
pub fn order(workspaces: &[Workspace]) -> Vec<&Workspace> {
    let mut ordered: Vec<_> = workspaces.iter().collect();
    ordered.sort_by(|a, b| {
        (a.urgency(), a.name.to_lowercase(), a.id).cmp(&(b.urgency(), b.name.to_lowercase(), b.id))
    });
    ordered
}

/// The tray icon for all workspaces: the highest-precedence state of those
/// that count. With none counting (no workspace, or every one held by another
/// runner), nothing is syncing here, which needs the user.
pub fn aggregate(workspaces: &[Workspace]) -> Icon {
    workspaces
        .iter()
        .filter_map(Workspace::icon)
        .min()
        .unwrap_or(Icon::Attention)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Cut `text` so that `prefix + text` fits the tooltip limit, marking the cut.
fn fit(prefix: &str, text: &str) -> String {
    let room = TOOLTIP_LIMIT.saturating_sub(utf16_len(prefix));
    if utf16_len(text) <= room {
        return format!("{prefix}{text}");
    }
    let mut kept = String::new();
    for c in text.chars() {
        if utf16_len(&kept) + c.len_utf16() + 1 > room {
            break;
        }
        kept.push(c);
    }
    format!("{prefix}{}…", kept.trim_end())
}

/// The tray tooltip, also the icon's accessible name. It always keeps
/// `Dispatch Mesh — <state>` and adds the reason for attention and offline;
/// when too long, the summary of other workspaces goes first, then the reason.
pub fn tooltip(workspaces: &[Workspace]) -> String {
    if workspaces.is_empty() {
        return format!("{PRODUCT} — No workspace yet");
    }
    let counted: Vec<&Workspace> = order(workspaces)
        .into_iter()
        .filter(|w| w.icon().is_some())
        .collect();
    let Some(first) = counted.first() else {
        return format!("{PRODUCT} — Needs attention: Mesh isn't syncing any workspace here");
    };
    let head = format!("{PRODUCT} — {}", first.short_label());
    // Attention and offline always say why (ADR-0010 supplies a reason).
    let explained = matches!(first.icon(), Some(Icon::Attention | Icon::Offline));
    if counted.len() == 1 && workspaces.len() == 1 {
        return match (explained, first.reason()) {
            (true, Some(reason)) => fit(&format!("{head}: "), reason),
            _ => head,
        };
    }
    let rest = &counted[1..];
    if rest.iter().all(|w| w.icon() == first.icon()) && first.icon() == Some(Icon::UpToDate) {
        return format!(
            "{head} ({})",
            plural(counted.len(), "workspace", "workspaces")
        );
    }
    let subject = match (explained, first.reason()) {
        (true, Some(reason)) => format!("{}, {reason}", first.name),
        _ => first.name.clone(),
    };
    let more_attention = rest
        .iter()
        .filter(|w| w.icon() == Some(Icon::Attention))
        .count();
    let more_offline = rest
        .iter()
        .filter(|w| w.icon() == Some(Icon::Offline))
        .count();
    let others = if rest.is_empty() {
        None
    } else if more_attention > 0 {
        Some(format!(
            "{more_attention} more {}",
            if more_attention == 1 {
                "needs attention"
            } else {
                "need attention"
            }
        ))
    } else if more_offline > 0 {
        Some(format!("{more_offline} more offline"))
    } else if rest.iter().all(|w| w.icon() == Some(Icon::UpToDate)) {
        Some(format!(
            "{} up to date",
            plural(rest.len(), "other", "others")
        ))
    } else {
        Some(format!(
            "{} fine",
            plural(rest.len(), "other workspace", "other workspaces")
        ))
    };
    let prefix = format!("{head}: ");
    if let Some(others) = others {
        let full = format!("{prefix}{subject} · {others}");
        if utf16_len(&full) <= TOOLTIP_LIMIT {
            return full;
        }
    }
    fit(&prefix, &subject)
}

/// Whether the Quit hint shows: some workspace has an open session.
pub fn any_open_session(workspaces: &[Workspace]) -> bool {
    workspaces.iter().any(Workspace::has_open_session)
}

/// The tray menu's Finish-session items: one per workspace with an open
/// session, named after it; just "Finish session" when only one has one.
pub fn finish_items(workspaces: &[Workspace]) -> Vec<(String, usize)> {
    let open: Vec<&Workspace> = workspaces.iter().filter(|w| w.has_open_session()).collect();
    match open.as_slice() {
        [only] => vec![(FINISH_SESSION.to_owned(), only.id)],
        several => several
            .iter()
            .map(|w| (format!("{FINISH_SESSION} — {}", w.name), w.id))
            .collect(),
    }
}

/// One step of a session's stage track.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StepView {
    pub label: &'static str,
    /// `done`, `current` or `todo`.
    pub state: &'static str,
}

/// One session in the activity list.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SessionView {
    pub id: String,
    pub stage: &'static str,
    /// Empty for a discarded session, which left the track.
    pub steps: Vec<StepView>,
    pub files: Vec<String>,
    pub author: String,
    pub started_at: u64,
    pub last_saved: u64,
    pub purposes: Vec<String>,
    pub title: Option<String>,
}

fn session_view(session: &SessionStatus, has_remote: bool) -> SessionView {
    let steps = if session.stage == Stage::Discarded {
        Vec::new()
    } else {
        let stages = track(has_remote);
        // Every non-discarded stage is on the track: a workspace without a
        // remote receives no drafts and uploads none of its own.
        let current = stages
            .iter()
            .position(|stage| *stage == session.stage)
            .unwrap_or_default();
        stages
            .iter()
            .enumerate()
            .map(|(index, stage)| StepView {
                label: stage_label(*stage),
                state: match index.cmp(&current) {
                    std::cmp::Ordering::Less => "done",
                    std::cmp::Ordering::Equal if *stage == Stage::Published => "done",
                    std::cmp::Ordering::Equal => "current",
                    std::cmp::Ordering::Greater => "todo",
                },
            })
            .collect()
    };
    SessionView {
        id: session.id.clone(),
        stage: stage_label(session.stage),
        steps,
        files: session.files.clone(),
        author: session.author.name.clone(),
        started_at: session.started_at,
        last_saved: session.last_saved,
        purposes: session.purposes.clone(),
        title: session.title.clone(),
    }
}

/// The activity list: every unsettled session, then the most recently
/// settled ones, each group newest first. Settling records no time, so the
/// last save orders settled sessions.
pub fn activity(sessions: &[SessionStatus], has_remote: bool) -> Vec<SessionView> {
    let newest_first = |list: &mut Vec<&SessionStatus>| {
        list.sort_by(|a, b| (b.last_saved, &b.id).cmp(&(a.last_saved, &a.id)))
    };
    let (mut done, mut open): (Vec<&SessionStatus>, Vec<&SessionStatus>) =
        sessions.iter().partition(|s| s.published || s.discarded);
    newest_first(&mut open);
    newest_first(&mut done);
    open.into_iter()
        .chain(done.into_iter().take(SETTLED_SHOWN))
        .map(|s| session_view(s, has_remote))
        .collect()
}

/// A file Mesh moved aside, as the window lists it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SavedCopyView {
    pub id: String,
    pub path: String,
    pub created_at: u64,
}

/// One workspace as the status window renders it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WorkspaceView {
    pub id: usize,
    pub name: String,
    pub path: String,
    pub label: &'static str,
    /// Tone and glyph: `attention`, `offline`, `working`, `synced` or `neutral`.
    pub tone: &'static str,
    pub reason: Option<String>,
    pub note: Option<&'static str>,
    pub already_running: bool,
    pub retry: bool,
    /// The last successful sync; `None` with `sync_label` saying why.
    pub last_sync: Option<u64>,
    /// What the header says instead of a sync time.
    pub sync_label: Option<&'static str>,
    /// Whether **Finish session** is offered.
    pub open_session: bool,
    /// The attention card's primary action: `finish_session` or none.
    pub action: Option<&'static str>,
    pub sessions: Vec<SessionView>,
    /// Newest first.
    pub saved_copies: Vec<SavedCopyView>,
}

pub fn view(workspaces: &[Workspace]) -> Vec<WorkspaceView> {
    order(workspaces)
        .into_iter()
        .map(|w| {
            let status = w.status();
            let offline = status.is_some_and(|s| s.status == Summary::Offline);
            let attention_reason = status
                .filter(|s| s.status == Summary::NeedsAttention)
                .and_then(|s| s.reason.as_deref());
            let sync_failed = attention_reason.is_some_and(|r| r.starts_with(SYNC_FAILURE));
            let open_session = w.has_open_session();
            WorkspaceView {
                id: w.id,
                name: w.name.clone(),
                path: w.path.display().to_string(),
                label: w.label(),
                tone: match w.icon() {
                    None => "neutral",
                    Some(Icon::Attention) => "attention",
                    Some(Icon::Offline) => "offline",
                    Some(Icon::Working) => "working",
                    Some(Icon::UpToDate) => "synced",
                },
                reason: w.reason().map(str::to_owned),
                note: if offline {
                    Some(OFFLINE_NOTE)
                } else {
                    sync_failed.then_some(RETRY_NOTE)
                },
                already_running: w.state == WorkspaceState::AlreadyRunning,
                retry: matches!(
                    w.state,
                    WorkspaceState::AlreadyRunning | WorkspaceState::Stopped(_)
                ),
                last_sync: status.and_then(|s| s.last_sync),
                sync_label: status.and_then(|s| match (s.has_remote, s.last_sync) {
                    (false, _) => Some(THIS_DEVICE_ONLY),
                    (true, None) => Some(NOT_SYNCED_YET),
                    (true, Some(_)) => None,
                }),
                open_session,
                action: (open_session
                    && attention_reason.is_some_and(|r| r.starts_with(RECEIVED_DRAFT_DEPENDENCY)))
                .then_some("finish_session"),
                sessions: status
                    .map(|s| activity(&s.sessions, s.has_remote))
                    .unwrap_or_default(),
                saved_copies: status
                    .map(|s| {
                        s.saved_copies
                            .iter()
                            .rev()
                            .map(|copy| SavedCopyView {
                                id: copy.id.clone(),
                                path: copy.path.clone(),
                                created_at: copy.created_at,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dispatch_mesh::status::Status;

    const ALL: [Summary; 6] = [
        Summary::NeedsAttention,
        Summary::Offline,
        Summary::SavedLocally,
        Summary::DraftUploaded,
        Summary::WaitingForFinalization,
        Summary::Synced,
    ];

    fn status(summary: Summary, reason: Option<&str>, session: bool) -> Status {
        Status {
            status: summary,
            reason: reason.map(str::to_owned),
            device: "device".into(),
            main: String::new(),
            commit_message_mode: "local_fallback",
            attention: None,
            offline: None,
            summary_failure: None,
            capture_error: None,
            local_changes_waiting: false,
            recovery_pending: false,
            active_session: session.then(|| "session".to_owned()),
            sessions: Vec::new(),
            unresolved_conflicts: Vec::new(),
            presence: Vec::new(),
            has_remote: true,
            last_sync: None,
            saved_copies: Vec::new(),
        }
    }

    fn running(id: usize, name: &str, summary: Summary, reason: Option<&str>) -> Workspace {
        Workspace {
            id,
            name: name.into(),
            path: PathBuf::from(format!("C:\\Users\\alex\\Documents\\{name}")),
            state: WorkspaceState::Running(Some(Box::new(RunnerSnapshot {
                status: status(summary, reason, false),
                capture_error: None,
                sync_error: None,
                summary_error: None,
            }))),
        }
    }

    fn with_state(id: usize, name: &str, state: WorkspaceState) -> Workspace {
        Workspace {
            id,
            name: name.into(),
            path: PathBuf::from(name),
            state,
        }
    }

    #[test]
    fn six_states_fold_into_four_icons() {
        let icons: Vec<_> = ALL.iter().map(|s| icon(*s)).collect();
        assert_eq!(
            icons,
            [
                Icon::Attention,
                Icon::Offline,
                Icon::Working,
                Icon::Working,
                Icon::Working,
                Icon::UpToDate
            ]
        );
    }

    #[test]
    fn every_state_has_its_own_label() {
        let labels: Vec<_> = ALL.iter().map(|s| label(*s)).collect();
        assert_eq!(
            labels,
            [
                "Needs attention",
                "Offline — changes saved on this device",
                "Saved on this device",
                "Shared as draft",
                "Finishing up",
                "Up to date"
            ]
        );
    }

    #[test]
    fn aggregate_follows_precedence_for_every_pair() {
        for a in ALL {
            for b in ALL {
                let both = [running(0, "A", a, None), running(1, "B", b, None)];
                assert_eq!(aggregate(&both), icon(a).min(icon(b)), "{a:?} + {b:?}");
            }
        }
    }

    #[test]
    fn stopped_counts_as_attention_and_already_running_does_not_count() {
        let stopped = with_state(0, "A", WorkspaceState::Stopped("gone".into()));
        let held = with_state(1, "B", WorkspaceState::AlreadyRunning);
        let synced = running(2, "C", Summary::Synced, None);
        assert_eq!(aggregate(&[held.clone(), synced.clone()]), Icon::UpToDate);
        assert_eq!(aggregate(&[stopped, synced]), Icon::Attention);
        assert_eq!(aggregate(std::slice::from_ref(&held)), Icon::Attention);
        assert_eq!(aggregate(&[]), Icon::Attention);
        assert_eq!(
            tooltip(&[held]),
            "Dispatch Mesh — Needs attention: Mesh isn't syncing any workspace here"
        );
        assert_eq!(tooltip(&[]), "Dispatch Mesh — No workspace yet");
    }

    #[test]
    fn starting_shows_as_working() {
        let starting = with_state(0, "A", WorkspaceState::Starting);
        assert_eq!(aggregate(std::slice::from_ref(&starting)), Icon::Working);
        assert_eq!(tooltip(&[starting]), "Dispatch Mesh — Starting");
    }

    #[test]
    fn single_workspace_tooltips_name_the_state_and_attention_reason() {
        let one = |s, r| tooltip(&[running(0, "Team Wiki", s, r)]);
        assert_eq!(one(Summary::Synced, None), "Dispatch Mesh — Up to date");
        assert_eq!(
            one(Summary::DraftUploaded, None),
            "Dispatch Mesh — Shared as draft"
        );
        assert_eq!(
            one(
                Summary::Offline,
                Some("Could not reach the remote (connection timed out)")
            ),
            "Dispatch Mesh — Offline, changes saved on this device: Could not reach the remote (connection timed out)"
        );
        assert_eq!(
            one(Summary::NeedsAttention, Some("2 unresolved conflicts")),
            "Dispatch Mesh — Needs attention: 2 unresolved conflicts"
        );
    }

    #[test]
    fn several_workspaces_name_the_deciding_one_and_sum_up_the_rest() {
        let wiki = running(
            0,
            "Team Wiki",
            Summary::NeedsAttention,
            Some("2 unresolved conflicts"),
        );
        let notes = running(1, "Research notes", Summary::DraftUploaded, None);
        let handbook = running(2, "Design handbook", Summary::Synced, None);
        assert_eq!(
            tooltip(&[handbook.clone(), notes.clone(), wiki]),
            "Dispatch Mesh — Needs attention: Team Wiki, 2 unresolved conflicts · 2 other workspaces fine"
        );
        assert_eq!(
            tooltip(&[handbook.clone(), notes]),
            "Dispatch Mesh — Shared as draft: Research notes · 1 other up to date"
        );
        let offline = |id, name| running(id, name, Summary::Offline, Some("timeout"));
        assert_eq!(
            tooltip(&[offline(0, "A"), offline(1, "B")]),
            "Dispatch Mesh — Offline, changes saved on this device: A, timeout · 1 more offline"
        );
        let wiki_offline = running(
            0,
            "Team Wiki",
            Summary::Offline,
            Some("Could not reach the remote"),
        );
        assert_eq!(
            tooltip(&[handbook.clone(), wiki_offline.clone()]),
            "Dispatch Mesh — Offline, changes saved on this device: Team Wiki, Could not reach the remote · 1 other up to date"
        );
        let conflict = running(
            1,
            "Notes",
            Summary::NeedsAttention,
            Some("1 unresolved conflict"),
        );
        assert_eq!(
            tooltip(&[wiki_offline, conflict]),
            "Dispatch Mesh — Needs attention: Notes, 1 unresolved conflict · 1 more offline"
        );
        let synced = |id, name| running(id, name, Summary::Synced, None);
        assert_eq!(
            tooltip(&[synced(0, "A"), synced(1, "B"), synced(2, "C")]),
            "Dispatch Mesh — Up to date (3 workspaces)"
        );
        let a = running(0, "A", Summary::NeedsAttention, Some("x"));
        let b = running(1, "B", Summary::NeedsAttention, Some("y"));
        assert_eq!(
            tooltip(&[b, a]),
            "Dispatch Mesh — Needs attention: A, x · 1 more needs attention"
        );
    }

    #[test]
    fn long_tooltips_keep_the_state_and_stay_within_the_windows_limit() {
        let offline_reason =
            "Could not reach the remote ".to_owned() + &"(very long cause) ".repeat(10);
        let offline = running(0, "Team Wiki", Summary::Offline, Some(&offline_reason));
        let text = tooltip(&[offline, running(1, "Other", Summary::Synced, None)]);
        assert!(text.encode_utf16().count() <= TOOLTIP_LIMIT, "{text}");
        assert!(
            text.starts_with(
                "Dispatch Mesh — Offline, changes saved on this device: Team Wiki, Could not reach"
            ),
            "{text}"
        );
        assert!(text.ends_with('…') && !text.contains(" · "), "{text}");

        let reason = "Local capture waiting: ".to_owned() + &"verylongfolder/".repeat(20);
        let wiki = running(0, "Team Wiki", Summary::NeedsAttention, Some(&reason));
        let other = running(1, "Other", Summary::Synced, None);
        for text in [
            tooltip(std::slice::from_ref(&wiki)),
            tooltip(&[wiki, other]),
        ] {
            assert!(text.encode_utf16().count() <= TOOLTIP_LIMIT, "{text}");
            assert!(
                text.starts_with("Dispatch Mesh — Needs attention: "),
                "{text}"
            );
            assert!(text.ends_with('…'), "{text}");
            assert!(
                !text.contains(" · "),
                "the summary goes before the reason: {text}"
            );
        }
    }

    #[test]
    fn window_lists_the_most_urgent_first_then_by_name() {
        let list = [
            running(0, "b synced", Summary::Synced, None),
            with_state(1, "held", WorkspaceState::AlreadyRunning),
            running(2, "a synced", Summary::Synced, None),
            running(3, "offline", Summary::Offline, Some("timeout")),
            running(
                4,
                "conflict",
                Summary::NeedsAttention,
                Some("1 unresolved conflict"),
            ),
            running(5, "draft", Summary::DraftUploaded, None),
        ];
        let names: Vec<_> = order(&list).iter().map(|w| w.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "conflict", "held", "offline", "draft", "a synced", "b synced"
            ]
        );
    }

    #[test]
    fn view_carries_reason_offline_note_and_retry() {
        let list = [
            running(
                0,
                "Team Wiki",
                Summary::Offline,
                Some("Could not reach the remote"),
            ),
            with_state(1, "Held", WorkspaceState::AlreadyRunning),
            with_state(
                2,
                "Broken",
                WorkspaceState::Stopped("Not a Mesh workspace".into()),
            ),
            running(3, "Fine", Summary::Synced, None),
        ];
        let views = view(&list);
        let get = |name: &str| views.iter().find(|v| v.name == name).unwrap();
        let wiki = get("Team Wiki");
        assert_eq!(wiki.label, "Offline — changes saved on this device");
        assert_eq!(wiki.reason.as_deref(), Some("Could not reach the remote"));
        assert_eq!(wiki.note, Some(OFFLINE_NOTE));
        assert_eq!((wiki.tone, wiki.retry), ("offline", false));
        let held = get("Held");
        assert!(held.already_running && held.retry);
        assert_eq!((held.tone, held.label), ("neutral", ALREADY_RUNNING));
        let broken = get("Broken");
        assert_eq!(broken.reason.as_deref(), Some("Not a Mesh workspace"));
        assert!(broken.retry && !broken.already_running);
        assert_eq!(broken.tone, "attention");
        let fine = get("Fine");
        assert_eq!(
            (fine.reason.clone(), fine.note, fine.retry),
            (None, None, false)
        );
    }

    fn session(id: &str, stage: Stage, last_saved: u64) -> SessionStatus {
        SessionStatus {
            id: id.into(),
            author: dispatch_mesh::model::Identity {
                name: "Alice".into(),
                email: "alice@example.test".into(),
            },
            stage,
            closed: !matches!(stage, Stage::SavedLocally | Stage::DraftUploaded),
            published: stage == Stage::Published,
            discarded: stage == Stage::Discarded,
            files: vec!["note.md".into()],
            dependencies: Default::default(),
            started_at: last_saved.saturating_sub(60),
            last_saved,
            title: (stage == Stage::Published).then(|| "Update note".into()),
            purposes: Vec::new(),
        }
    }

    fn with_status(id: usize, name: &str, edit: impl FnOnce(&mut Status)) -> Workspace {
        let mut workspace = running(id, name, Summary::Synced, None);
        if let WorkspaceState::Running(Some(snapshot)) = &mut workspace.state {
            edit(&mut snapshot.status);
        }
        workspace
    }

    fn ids(list: &[SessionView]) -> Vec<&str> {
        list.iter().map(|s| s.id.as_str()).collect()
    }

    #[test]
    fn activity_lists_every_unsettled_session_and_the_ten_latest_settled() {
        assert!(activity(&[], true).is_empty());

        let mut sessions = vec![
            session("open-old", Stage::SavedLocally, 100),
            session("open-new", Stage::DraftUploaded, 900),
            session("finishing", Stage::WaitingForFinalization, 500),
        ];
        for n in 0..5 {
            sessions.push(session(&format!("done-{n}"), Stage::Published, 200 + n));
        }
        assert_eq!(
            ids(&activity(&sessions, true)),
            [
                "open-new",
                "finishing",
                "open-old",
                "done-4",
                "done-3",
                "done-2",
                "done-1",
                "done-0"
            ]
        );

        for n in 5..15 {
            let stage = if n % 2 == 0 {
                Stage::Published
            } else {
                Stage::Discarded
            };
            sessions.push(session(&format!("done-{n}"), stage, 200 + n));
        }
        let list = activity(&sessions, true);
        assert_eq!(list.len(), 3 + SETTLED_SHOWN);
        assert_eq!(
            ids(&list[3..]),
            [
                "done-14", "done-13", "done-12", "done-11", "done-10", "done-9", "done-8",
                "done-7", "done-6", "done-5"
            ],
            "the ten most recently settled, published or discarded"
        );
    }

    fn track_of(stage: Stage, has_remote: bool) -> Vec<(&'static str, &'static str)> {
        session_view(&session("s", stage, 1), has_remote)
            .steps
            .iter()
            .map(|step| (step.label, step.state))
            .collect()
    }

    #[test]
    fn every_stage_has_a_label_and_a_place_on_the_track() {
        assert_eq!(
            track_of(Stage::SavedLocally, true),
            [
                ("Saved on this device", "current"),
                ("Shared as draft", "todo"),
                ("Finishing up", "todo"),
                ("Added to shared history", "todo"),
            ]
        );
        assert_eq!(
            track_of(Stage::DraftUploaded, true),
            [
                ("Saved on this device", "done"),
                ("Shared as draft", "current"),
                ("Finishing up", "todo"),
                ("Added to shared history", "todo"),
            ]
        );
        assert_eq!(
            track_of(Stage::WaitingForFinalization, true),
            [
                ("Saved on this device", "done"),
                ("Shared as draft", "done"),
                ("Finishing up", "current"),
                ("Added to shared history", "todo"),
            ]
        );
        assert_eq!(
            track_of(Stage::Published, true),
            [
                ("Saved on this device", "done"),
                ("Shared as draft", "done"),
                ("Finishing up", "done"),
                ("Added to shared history", "done"),
            ]
        );
        assert!(track_of(Stage::Discarded, true).is_empty());
        for (stage, label) in [
            (Stage::SavedLocally, "Saved on this device"),
            (Stage::DraftUploaded, "Shared as draft"),
            (Stage::WaitingForFinalization, "Finishing up"),
            (Stage::Published, "Added to shared history"),
            (Stage::Discarded, "No changes kept"),
        ] {
            assert_eq!(session_view(&session("s", stage, 1), true).stage, label);
        }
    }

    #[test]
    fn a_workspace_without_a_remote_has_no_draft_step() {
        assert_eq!(
            track_of(Stage::WaitingForFinalization, false),
            [
                ("Saved on this device", "done"),
                ("Finishing up", "current"),
                ("Added to shared history", "todo"),
            ]
        );
        for stage in [
            Stage::SavedLocally,
            Stage::WaitingForFinalization,
            Stage::Published,
        ] {
            assert!(
                !track_of(stage, false)
                    .iter()
                    .any(|(label, _)| *label == "Shared as draft")
            );
        }
    }

    #[test]
    fn sessions_show_their_own_author_purposes_title_and_times() {
        let mut received = session("bob", Stage::DraftUploaded, 300);
        received.author.name = "Bob".into();
        received.purposes = vec!["Explain setup".into()];
        let view = session_view(&received, true);
        assert_eq!(view.author, "Bob", "never the local user");
        assert_eq!(view.purposes, ["Explain setup"]);
        assert_eq!((view.started_at, view.last_saved), (240, 300));
        assert_eq!(view.title, None);
        let published = session_view(&session("p", Stage::Published, 1), true);
        assert_eq!(published.title.as_deref(), Some("Update note"));
    }

    #[test]
    fn header_says_when_the_last_sync_was_or_why_there_is_none() {
        let view_of = |w: Workspace| view(&[w]).remove(0);
        let synced = view_of(with_status(0, "A", |s| s.last_sync = Some(1_000)));
        assert_eq!((synced.last_sync, synced.sync_label), (Some(1_000), None));
        let never = view_of(with_status(0, "A", |_| {}));
        assert_eq!(
            (never.last_sync, never.sync_label),
            (None, Some("Not synced yet"))
        );
        let local = view_of(with_status(0, "A", |s| s.has_remote = false));
        assert_eq!(local.sync_label, Some("This device only"));
        let starting = view_of(with_state(0, "A", WorkspaceState::Starting));
        assert_eq!((starting.last_sync, starting.sync_label), (None, None));
    }

    #[test]
    fn attention_offers_finish_session_or_says_mesh_retries() {
        let attention = |reason: &str, open: bool| {
            let mut w = running(0, "A", Summary::NeedsAttention, Some(reason));
            if let WorkspaceState::Running(Some(snapshot)) = &mut w.state {
                snapshot.status.active_session = open.then(|| "s".into());
            }
            view(&[w]).remove(0)
        };
        let dependent = attention(
            "Edits depend on a received draft and cannot be attributed safely. Bytes are saved locally.",
            true,
        );
        assert_eq!(dependent.action, Some("finish_session"));
        assert_eq!(dependent.note, None);
        assert!(dependent.open_session);

        let failed = attention("Synchronization waiting: remote rejected the update", false);
        assert_eq!((failed.action, failed.note), (None, Some(RETRY_NOTE)));

        for reason in [
            "1 unresolved conflict",
            "Local capture waiting: access denied",
            "AI summary failed: quota exceeded",
        ] {
            let other = attention(reason, true);
            assert_eq!((other.action, other.note), (None, None), "{reason}");
            assert!(other.open_session, "Finish session stays available");
        }
        let offline = view(&[running(0, "A", Summary::Offline, Some("timeout"))]).remove(0);
        assert_eq!(offline.note, Some(OFFLINE_NOTE));
    }

    #[test]
    fn saved_copies_are_listed_newest_first() {
        let copy = |id: &str, at| dispatch_mesh::model::SavedCopy {
            id: id.into(),
            path: format!("{id}.md"),
            created_at: at,
        };
        let view = view(&[with_status(0, "A", |s| {
            s.saved_copies = vec![copy("old", 1), copy("new", 2)]
        })])
        .remove(0);
        let listed: Vec<_> = view
            .saved_copies
            .iter()
            .map(|c| (c.path.as_str(), c.created_at))
            .collect();
        assert_eq!(listed, [("new.md", 2), ("old.md", 1)]);
    }

    #[test]
    fn finish_items_name_the_workspace_only_when_several_have_a_session() {
        let open = |id, name| with_status(id, name, |s| s.active_session = Some("s".into()));
        let idle = |id, name| with_status(id, name, |_| {});
        assert!(finish_items(&[idle(0, "A"), idle(1, "B")]).is_empty());
        assert_eq!(
            finish_items(&[idle(0, "A"), open(1, "Team Wiki")]),
            [("Finish session".to_owned(), 1)]
        );
        assert_eq!(
            finish_items(&[
                open(0, "Team Wiki"),
                idle(1, "B"),
                open(2, "Notes"),
                with_state(3, "Held", WorkspaceState::AlreadyRunning),
            ]),
            [
                ("Finish session — Team Wiki".to_owned(), 0),
                ("Finish session — Notes".to_owned(), 2)
            ]
        );
    }

    #[test]
    fn quit_hint_follows_open_sessions() {
        let mut open = running(0, "A", Summary::DraftUploaded, None);
        assert!(!any_open_session(std::slice::from_ref(&open)));
        if let WorkspaceState::Running(Some(snapshot)) = &mut open.state {
            snapshot.status.active_session = Some("s".into());
        }
        assert!(any_open_session(&[
            open,
            with_state(1, "B", WorkspaceState::AlreadyRunning)
        ]));
    }
}
