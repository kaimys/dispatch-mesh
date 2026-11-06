//! What the tray and the status window say, decided in one place (US00001).
//!
//! Pure functions over the workspaces a host runs: labels, the tray icon,
//! how several workspaces sum up, the tooltip and the window's view model.
//! Nothing here touches Tauri, so every rule is unit-tested.

use dispatch_mesh::{runner::RunnerSnapshot, status::Summary};
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
}

pub fn view(workspaces: &[Workspace]) -> Vec<WorkspaceView> {
    order(workspaces)
        .into_iter()
        .map(|w| {
            let offline = w
                .summary()
                .is_some_and(|s| s.status.status == Summary::Offline);
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
                note: offline.then_some(OFFLINE_NOTE),
                already_running: w.state == WorkspaceState::AlreadyRunning,
                retry: matches!(
                    w.state,
                    WorkspaceState::AlreadyRunning | WorkspaceState::Stopped(_)
                ),
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
