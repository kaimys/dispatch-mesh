//! What the tray and the status window say, decided in one place (US00001).
//!
//! Pure functions over the workspaces a host runs: labels, the tray icon,
//! how several workspaces sum up, the tooltip and the window's view model.
//! Nothing here touches Tauri, so every rule is unit-tested.

use dispatch_mesh::{
    engine::{CIRCULAR_DEPENDENCY, RECEIVED_DRAFT_DEPENDENCY, SYNC_FAILURE},
    runner::RunnerSnapshot,
    status::{CAPTURE_WAITING, SessionStatus, Stage, Status, Summary},
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
/// Reassurance lines under an attention reason (US00012 Q8).
pub const CONFLICTS_REASSURANCE: &str =
    "Both versions of each file are kept. Other files keep syncing.";
pub const SYNC_REASSURANCE: &str =
    "Your edits are saved on this device. Mesh retries automatically.";
pub const CAPTURE_REASSURANCE: &str =
    "Saved files stay as they are. Mesh tries again on the next save.";
pub const DEPENDENT_REASSURANCE: &str =
    "Your edits are saved on this device. Finish the session to share them.";
pub const SUMMARY_REASSURANCE: &str =
    "Your session is added to shared history with a local summary.";
/// The state card's line for the working and synced states (design spec).
const SAVED_BODY: &str = "Your latest edits are captured. Sharing them as a draft in a moment.";
/// Saved on this device without a remote: nothing is shared as a draft.
const SAVED_LOCAL_BODY: &str = "Your latest edits are captured. They join history after 5 minutes without edits, or when you finish the session.";
const DRAFT_BODY: &str = "Collaborators can see your session. It joins shared history after 5 minutes without edits, or when you finish it.";
const SYNCED_BODY: &str = "Everything you saved is in shared history.";
/// The author of this device's sessions.
const YOU: &str = "You";
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

/// The status glyph of the window (design spec, *Glyphs*): the file stem of
/// `ui/glyphs/status-<glyph>-<light|dark>.svg`, or `none`.
pub fn status_glyph(icon: Option<Icon>) -> &'static str {
    match icon {
        Some(Icon::Attention) => "needs-attention",
        Some(Icon::Offline) => "offline",
        Some(Icon::Working) => "working",
        Some(Icon::UpToDate) => "up-to-date",
        None => "none",
    }
}

/// The state card's line under the label for the working and synced states
/// (design spec, *State card per summary state*; US00012 Q8).
fn working_body(status: &Status) -> Option<String> {
    let line = match status.status {
        Summary::SavedLocally if status.has_remote => SAVED_BODY,
        Summary::SavedLocally => SAVED_LOCAL_BODY,
        Summary::DraftUploaded => DRAFT_BODY,
        Summary::Synced => SYNCED_BODY,
        Summary::WaitingForFinalization => {
            let finishing = status
                .sessions
                .iter()
                .filter(|s| s.device == status.device && s.stage == Stage::WaitingForFinalization)
                .count();
            return Some(if finishing > 1 {
                format!("Adding your {finishing} finished sessions to shared history.")
            } else {
                "Adding your finished session to shared history.".to_owned()
            });
        }
        Summary::NeedsAttention | Summary::Offline => return None,
    };
    Some(line.to_owned())
}

/// The reassurance under an attention or offline reason (US00012 Q8). A
/// reason without an agreed line gets none rather than a guessed one.
fn reassurance(status: &Status) -> Option<&'static str> {
    let reason = status.reason.as_deref()?;
    match status.status {
        Summary::Offline => Some(OFFLINE_NOTE),
        Summary::NeedsAttention if !status.unresolved_conflicts.is_empty() => {
            Some(CONFLICTS_REASSURANCE)
        }
        Summary::NeedsAttention if reason.starts_with(SYNC_FAILURE) => Some(SYNC_REASSURANCE),
        Summary::NeedsAttention if reason.starts_with(CAPTURE_WAITING) => Some(CAPTURE_REASSURANCE),
        Summary::NeedsAttention
            if reason.starts_with(RECEIVED_DRAFT_DEPENDENCY) || reason == CIRCULAR_DEPENDENCY =>
        {
            Some(DEPENDENT_REASSURANCE)
        }
        Summary::NeedsAttention if status.summary_failure.as_deref() == Some(reason) => {
            Some(SUMMARY_REASSURANCE)
        }
        _ => None,
    }
}

/// Up to two initials from a name: the first letters of its first and last
/// word, upper-cased.
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    let pick = match words.as_slice() {
        [] => return "?".to_owned(),
        [only] => vec![*only],
        [first, .., last] => vec![*first, *last],
    };
    pick.iter()
        .filter_map(|word| word.chars().next())
        .flat_map(char::to_uppercase)
        .collect()
}

/// Avatar colors (US00012 Q9): the same in both themes, each dark enough for
/// white initials. `ui/app.css` mirrors them as `.avatar.c0` … `.c5` and
/// `.avatar.you`; a test checks both agree.
pub const AVATAR_COLORS: [&str; 6] = [
    "#2457C5", "#7A4FB0", "#2F6B4F", "#B4490B", "#3D3F44", "#0E6E7E",
];
pub const YOU_COLOR: &str = "#3D3F44";

/// A stable color index for an author: FNV-1a over the lower-cased email, so
/// a person has the same color on every device.
pub fn avatar_color(email: &str) -> usize {
    let hash = email
        .to_lowercase()
        .bytes()
        .fold(0x811c_9dc5_u32, |hash, byte| {
            (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
        });
    hash as usize % AVATAR_COLORS.len()
}

/// A session's avatar: initials and a color class (`c0` … `c5`, or `you`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AvatarView {
    pub initials: String,
    pub color: String,
}

/// One session in the activity list (design spec, *Session row*).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SessionView {
    pub id: String,
    /// "You" for this device's sessions, else the author's name (US00012 Q5).
    pub author: String,
    pub own: bool,
    pub avatar: AvatarView,
    /// "N files"; the window adds the start and last-save times.
    pub files_label: String,
    pub started_at: u64,
    pub last_saved: u64,
    /// Line 2: the summary title once published, else the files.
    pub line: String,
    /// The requested purpose, when one is recorded ("… +N more" for several).
    pub purpose: Option<String>,
    /// The stage, named once; "Discarded" for a discarded session.
    pub stage: &'static str,
    /// What screen readers hear instead of the track.
    pub stage_text: String,
    /// One entry per track segment: `working`, `synced`, `received` or
    /// `empty`. Empty for a discarded session, which shows a tag instead.
    pub segments: Vec<&'static str>,
    pub discarded: bool,
}

const DISCARDED: &str = "Discarded";

fn session_view(session: &SessionStatus, has_remote: bool, this_device: &str) -> SessionView {
    let own = session.device == this_device;
    let discarded = session.stage == Stage::Discarded;
    let stages = track(has_remote);
    let current = stages
        .iter()
        .position(|stage| *stage == session.stage)
        .unwrap_or_default();
    let segments = if discarded {
        Vec::new()
    } else {
        (0..stages.len())
            .map(|index| match index {
                _ if session.stage == Stage::Published => "synced",
                0 if !own => "received",
                index if index <= current => "working",
                _ => "empty",
            })
            .collect()
    };
    let stage = if discarded {
        DISCARDED
    } else {
        stage_label(session.stage)
    };
    let stage_text = if discarded {
        format!("Stage: {DISCARDED}")
    } else {
        format!("Stage: {stage}, {} of {}", current + 1, stages.len())
    };
    let purpose = session
        .purposes
        .first()
        .map(|first| match session.purposes.len() {
            1 => format!("Requested purpose: {first}"),
            n => format!("Requested purpose: {first} +{} more", n - 1),
        });
    SessionView {
        id: session.id.clone(),
        author: if own {
            YOU.to_owned()
        } else {
            session.author.name.clone()
        },
        own,
        avatar: AvatarView {
            initials: initials(&session.author.name),
            color: if own {
                "you".to_owned()
            } else {
                format!("c{}", avatar_color(&session.author.email))
            },
        },
        files_label: plural(session.files.len(), "file", "files"),
        started_at: session.started_at,
        last_saved: session.last_saved,
        line: session
            .title
            .clone()
            .unwrap_or_else(|| session.files.join(", ")),
        purpose,
        stage,
        stage_text,
        segments,
        discarded,
    }
}

/// The activity list (AC 5): every unsettled session, then the
/// [`SETTLED_SHOWN`] settled ones with the latest last save, each group
/// newest first. Settling records no time, so the last save ranks them.
pub fn activity(
    sessions: &[SessionStatus],
    has_remote: bool,
    this_device: &str,
) -> Vec<SessionView> {
    let newest_first = |list: &mut Vec<&SessionStatus>| {
        list.sort_by(|a, b| (b.last_saved, &b.id).cmp(&(a.last_saved, &a.id)))
    };
    let (mut done, mut open): (Vec<&SessionStatus>, Vec<&SessionStatus>) =
        sessions.iter().partition(|s| s.published || s.discarded);
    newest_first(&mut open);
    newest_first(&mut done);
    open.into_iter()
        .chain(done.into_iter().take(SETTLED_SHOWN))
        .map(|s| session_view(s, has_remote, this_device))
        .collect()
}

/// A file Mesh moved aside, as the window lists it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SavedCopyView {
    pub id: String,
    pub path: String,
    pub created_at: u64,
}

/// The Conflicts tab until conflict review arrives (US00012 Q2).
pub fn conflicts_placeholder(count: usize) -> String {
    match count {
        0 => "No unresolved conflicts.".to_owned(),
        n => format!(
            "{}. Review them with the command line until conflict review arrives.",
            plural(n, "unresolved conflict", "unresolved conflicts")
        ),
    }
}

/// One workspace as the status window renders it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WorkspaceView {
    pub id: usize,
    pub name: String,
    pub path: String,
    pub label: &'static str,
    /// Tone of the card: `attention`, `offline`, `working`, `synced` or `neutral`.
    pub tone: &'static str,
    /// The status glyph, see [`status_glyph`].
    pub glyph: &'static str,
    /// The engine's reason for attention or offline, or why it stopped.
    pub reason: Option<String>,
    /// The card's line under the label: the reason, or the state's sentence.
    pub body: Option<String>,
    /// The reassurance under the reason, where one is agreed (Q8).
    pub reassurance: Option<&'static str>,
    pub already_running: bool,
    pub retry: bool,
    /// The last successful sync; `None` with `sync_label` saying why.
    pub last_sync: Option<u64>,
    /// What the card says instead of a sync time.
    pub sync_label: Option<&'static str>,
    /// Whether the workspace has an open session.
    pub open_session: bool,
    /// The card's primary action: `open_conflicts`, `finish_session` or none.
    pub action: Option<&'static str>,
    /// **Finish session** as a secondary button: an open session whose card
    /// has no primary **Finish session**.
    pub finish_secondary: bool,
    pub conflict_count: usize,
    /// Accessible name of the Conflicts tab.
    pub conflicts_tab: String,
    pub conflicts_placeholder: String,
    pub sessions: Vec<SessionView>,
    /// Newest first.
    pub saved_copies: Vec<SavedCopyView>,
}

pub fn view(workspaces: &[Workspace]) -> Vec<WorkspaceView> {
    order(workspaces)
        .into_iter()
        .map(|w| {
            let status = w.status();
            let attention_reason = status
                .filter(|s| s.status == Summary::NeedsAttention)
                .and_then(|s| s.reason.as_deref());
            let open_session = w.has_open_session();
            let conflict_count = status.map_or(0, |s| s.unresolved_conflicts.len());
            let action = if attention_reason.is_some() && conflict_count > 0 {
                Some("open_conflicts")
            } else if open_session
                && attention_reason.is_some_and(|r| r.starts_with(RECEIVED_DRAFT_DEPENDENCY))
            {
                Some("finish_session")
            } else {
                None
            };
            let body = match status {
                Some(s) if matches!(s.status, Summary::NeedsAttention | Summary::Offline) => {
                    s.reason.clone()
                }
                Some(s) => working_body(s),
                None => w.reason().map(str::to_owned),
            };
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
                glyph: status_glyph(w.icon()),
                reason: w.reason().map(str::to_owned),
                body,
                reassurance: status.and_then(reassurance),
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
                action,
                finish_secondary: open_session && action != Some("finish_session"),
                conflict_count,
                conflicts_tab: format!("Conflicts, {conflict_count} unresolved"),
                conflicts_placeholder: conflicts_placeholder(conflict_count),
                sessions: status
                    .map(|s| activity(&s.sessions, s.has_remote, &s.device))
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
        assert_eq!(wiki.reassurance, Some(OFFLINE_NOTE));
        assert_eq!(wiki.body.as_deref(), Some("Could not reach the remote"));
        assert_eq!(wiki.glyph, "offline");
        assert_eq!((wiki.tone, wiki.retry), ("offline", false));
        let held = get("Held");
        assert!(held.already_running && held.retry);
        assert_eq!((held.tone, held.label), ("neutral", ALREADY_RUNNING));
        assert_eq!(held.glyph, "none");
        let broken = get("Broken");
        assert_eq!(broken.reason.as_deref(), Some("Not a Mesh workspace"));
        assert!(broken.retry && !broken.already_running);
        assert_eq!(
            (broken.tone, broken.glyph),
            ("attention", "needs-attention")
        );
        assert_eq!(broken.body.as_deref(), Some("Not a Mesh workspace"));
        let fine = get("Fine");
        assert_eq!(
            (fine.reason.clone(), fine.reassurance, fine.retry),
            (None, None, false)
        );
        assert_eq!(fine.body.as_deref(), Some(SYNCED_BODY));
        assert_eq!(fine.glyph, "up-to-date");
    }

    fn session(id: &str, stage: Stage, last_saved: u64) -> SessionStatus {
        SessionStatus {
            id: id.into(),
            author: dispatch_mesh::model::Identity {
                name: "Alice".into(),
                email: "alice@example.test".into(),
            },
            device: "device".into(),
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
        assert!(activity(&[], true, "device").is_empty());

        let mut sessions = vec![
            session("open-old", Stage::SavedLocally, 100),
            session("open-new", Stage::DraftUploaded, 900),
            session("finishing", Stage::WaitingForFinalization, 500),
        ];
        for n in 0..5 {
            sessions.push(session(&format!("done-{n}"), Stage::Published, 200 + n));
        }
        assert_eq!(
            ids(&activity(&sessions, true, "device")),
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
        let list = activity(&sessions, true, "device");
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

    #[test]
    fn activity_keeps_open_sessions_first_and_ranks_settled_ones_by_last_save() {
        // AC 5 as amended 2026-11-06: an old open session stays above a newer
        // published one, and a session settling late ranks by its last save.
        let mut sessions = vec![
            session("old-open", Stage::SavedLocally, 10),
            session("new-published", Stage::Published, 900),
        ];
        assert_eq!(
            ids(&activity(&sessions, true, "device")),
            ["old-open", "new-published"]
        );
        for n in 0..SETTLED_SHOWN as u64 {
            sessions.push(session(&format!("recent-{n}"), Stage::Published, 500 + n));
        }
        // Saved long ago, settled just now after a conflict cleared.
        sessions.push(session("settled-late", Stage::Published, 20));
        let list = activity(&sessions, true, "device");
        assert_eq!(list[0].id, "old-open");
        assert_eq!(list.len(), 1 + SETTLED_SHOWN);
        assert!(
            !ids(&list).contains(&"settled-late"),
            "ranked by its last save, it falls outside the ten"
        );
    }

    fn row(stage: Stage, has_remote: bool, device: &str) -> SessionView {
        let mut session = session("s", stage, 1);
        session.device = device.into();
        session_view(&session, has_remote, "device")
    }

    #[test]
    fn every_stage_fills_the_track_up_to_itself_and_is_named_once() {
        let own = |stage| {
            let view = row(stage, true, "device");
            (view.segments, view.stage, view.stage_text)
        };
        assert_eq!(
            own(Stage::SavedLocally),
            (
                vec!["working", "empty", "empty", "empty"],
                "Saved on this device",
                "Stage: Saved on this device, 1 of 4".to_owned()
            )
        );
        assert_eq!(
            own(Stage::DraftUploaded),
            (
                vec!["working", "working", "empty", "empty"],
                "Shared as draft",
                "Stage: Shared as draft, 2 of 4".to_owned()
            )
        );
        assert_eq!(
            own(Stage::WaitingForFinalization),
            (
                vec!["working", "working", "working", "empty"],
                "Finishing up",
                "Stage: Finishing up, 3 of 4".to_owned()
            )
        );
        assert_eq!(
            own(Stage::Published),
            (
                vec!["synced"; 4],
                "Added to shared history",
                "Stage: Added to shared history, 4 of 4".to_owned()
            )
        );
        let discarded = row(Stage::Discarded, true, "device");
        assert!(discarded.discarded && discarded.segments.is_empty());
        assert_eq!(
            (discarded.stage, discarded.stage_text.as_str()),
            ("Discarded", "Stage: Discarded")
        );
    }

    #[test]
    fn a_received_session_starts_with_a_pale_segment_and_names_its_author() {
        let view = row(Stage::DraftUploaded, true, "other-device");
        assert_eq!(view.segments, ["received", "working", "empty", "empty"]);
        assert_eq!((view.author.as_str(), view.own), ("Alice", false));
        assert_eq!(view.avatar.initials, "A");
        assert!(view.avatar.color.starts_with('c'), "{}", view.avatar.color);
        let published = row(Stage::Published, true, "other-device");
        assert_eq!(published.segments, vec!["synced"; 4], "published wins");
        let own = row(Stage::DraftUploaded, true, "device");
        assert_eq!((own.author.as_str(), own.own), ("You", true));
        assert_eq!(
            (own.avatar.initials.as_str(), own.avatar.color.as_str()),
            ("A", "you")
        );
    }

    #[test]
    fn a_workspace_without_a_remote_has_three_segments_and_no_draft_step() {
        let view = row(Stage::WaitingForFinalization, false, "device");
        assert_eq!(view.segments, ["working", "working", "empty"]);
        assert_eq!(view.stage_text, "Stage: Finishing up, 2 of 3");
        for stage in [
            Stage::SavedLocally,
            Stage::WaitingForFinalization,
            Stage::Published,
        ] {
            assert_eq!(row(stage, false, "device").segments.len(), 3);
        }
    }

    #[test]
    fn rows_show_title_or_files_the_file_count_and_one_purpose_line() {
        let mut received = session("bob", Stage::DraftUploaded, 300);
        received.author.name = "Bob".into();
        received.device = "bobs-device".into();
        received.files = vec!["a.md".into(), "b.md".into()];
        let view = session_view(&received, true, "device");
        assert_eq!(view.author, "Bob", "never the local user");
        assert_eq!(
            (view.line.as_str(), view.files_label.as_str()),
            ("a.md, b.md", "2 files")
        );
        assert_eq!((view.started_at, view.last_saved), (240, 300));
        assert_eq!(view.purpose, None);
        received.purposes = vec!["Explain setup".into()];
        assert_eq!(
            session_view(&received, true, "device").purpose.as_deref(),
            Some("Requested purpose: Explain setup")
        );
        received
            .purposes
            .extend(["Fix links".into(), "Add index".into()]);
        assert_eq!(
            session_view(&received, true, "device").purpose.as_deref(),
            Some("Requested purpose: Explain setup +2 more")
        );
        let published = session_view(&session("p", Stage::Published, 1), true, "device");
        assert_eq!(
            (published.line.as_str(), published.files_label.as_str()),
            ("Update note", "1 file")
        );
    }

    #[test]
    fn initials_take_the_first_and_last_word() {
        assert_eq!(initials("Alex Lindqvist"), "AL");
        assert_eq!(initials("Ada Byron King"), "AK");
        assert_eq!(initials("bob"), "B");
        assert_eq!(initials("  "), "?");
        assert_eq!(initials("Émile Zola"), "ÉZ");
    }

    #[test]
    fn avatar_colors_are_stable_per_email_and_spread_over_the_palette() {
        assert_eq!(
            avatar_color("Alice@Example.test"),
            avatar_color("alice@example.test")
        );
        let used: std::collections::BTreeSet<_> = (0..60)
            .map(|n| avatar_color(&format!("person{n}@example.test")))
            .collect();
        assert_eq!(used.len(), AVATAR_COLORS.len(), "{used:?}");
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

    fn attention(reason: &str, open: bool, edit: impl FnOnce(&mut Status)) -> WorkspaceView {
        let mut w = running(0, "A", Summary::NeedsAttention, Some(reason));
        if let WorkspaceState::Running(Some(snapshot)) = &mut w.state {
            snapshot.status.active_session = open.then(|| "s".into());
            edit(&mut snapshot.status);
        }
        view(&[w]).remove(0)
    }

    #[test]
    fn attention_reasons_get_their_action_and_reassurance() {
        let dependent = attention(
            "Edits depend on a received draft and cannot be attributed safely. Bytes are saved locally.",
            true,
            |_| {},
        );
        assert_eq!(dependent.action, Some("finish_session"));
        assert!(!dependent.finish_secondary, "Finish session is the primary");
        assert_eq!(dependent.reassurance, Some(DEPENDENT_REASSURANCE));
        let circular = attention(CIRCULAR_DEPENDENCY, true, |_| {});
        assert_eq!(circular.reassurance, Some(DEPENDENT_REASSURANCE));

        // US00005 AC 10: the card still says Mesh retries, now in one line.
        let failed = attention(
            "Synchronization waiting: remote rejected the update",
            false,
            |_| {},
        );
        assert_eq!(
            (failed.action, failed.reassurance),
            (None, Some(SYNC_REASSURANCE))
        );
        assert!(SYNC_REASSURANCE.contains("Mesh retries automatically."));

        let conflicts = attention("2 unresolved conflicts", true, |s| {
            s.unresolved_conflicts = ["a", "b"]
                .map(|id| dispatch_mesh::status::ConflictStatus {
                    id: id.into(),
                    path: format!("{id}.md"),
                    binary: false,
                })
                .to_vec()
        });
        assert_eq!(conflicts.action, Some("open_conflicts"));
        assert_eq!(conflicts.reassurance, Some(CONFLICTS_REASSURANCE));
        assert!(conflicts.finish_secondary, "Finish session stays available");
        assert_eq!(conflicts.conflict_count, 2);
        assert_eq!(conflicts.conflicts_tab, "Conflicts, 2 unresolved");
        assert_eq!(conflicts.body.as_deref(), Some("2 unresolved conflicts"));
        assert_eq!(conflicts.glyph, "needs-attention");

        let capture = attention("Local capture waiting: access denied", true, |_| {});
        assert_eq!(capture.reassurance, Some(CAPTURE_REASSURANCE));
        assert_eq!(capture.action, None);
        assert!(capture.finish_secondary, "Finish session stays available");

        let summary = attention("AI summary failed: quota exceeded", false, |s| {
            s.summary_failure = Some("AI summary failed: quota exceeded".into())
        });
        assert_eq!(summary.reassurance, Some(SUMMARY_REASSURANCE));

        for reason in [
            "Shared history was rewritten outside Mesh; synchronization paused",
            "Something nobody mapped",
        ] {
            let other = attention(reason, false, |_| {});
            assert_eq!((other.action, other.reassurance), (None, None), "{reason}");
            assert!(!other.finish_secondary);
        }
        let offline = view(&[running(0, "A", Summary::Offline, Some("timeout"))]).remove(0);
        assert_eq!(offline.reassurance, Some(OFFLINE_NOTE));
    }

    #[test]
    fn working_states_explain_themselves_and_offer_finish_session() {
        let state = |summary, edit: &dyn Fn(&mut Status)| {
            let mut w = running(0, "A", summary, None);
            if let WorkspaceState::Running(Some(snapshot)) = &mut w.state {
                edit(&mut snapshot.status);
            }
            view(&[w]).remove(0)
        };
        let open = |s: &mut Status| s.active_session = Some("s".into());
        let saved = state(Summary::SavedLocally, &open);
        assert_eq!(saved.body.as_deref(), Some(SAVED_BODY));
        assert!(saved.finish_secondary && saved.action.is_none());
        assert_eq!(saved.glyph, "working");
        let local = state(Summary::SavedLocally, &|s| {
            open(s);
            s.has_remote = false
        });
        assert_eq!(local.body.as_deref(), Some(SAVED_LOCAL_BODY));
        let draft = state(Summary::DraftUploaded, &open);
        assert_eq!(draft.body.as_deref(), Some(DRAFT_BODY));
        assert!(draft.finish_secondary);

        let finishing = |own: usize, other: usize| {
            state(Summary::WaitingForFinalization, &|s| {
                for n in 0..own {
                    s.sessions.push(session(
                        &format!("own{n}"),
                        Stage::WaitingForFinalization,
                        1,
                    ));
                }
                for n in 0..other {
                    let mut received = session(&format!("in{n}"), Stage::WaitingForFinalization, 1);
                    received.device = "other".into();
                    s.sessions.push(received);
                }
            })
            .body
            .unwrap()
        };
        assert_eq!(
            finishing(1, 2),
            "Adding your finished session to shared history."
        );
        assert_eq!(
            finishing(3, 0),
            "Adding your 3 finished sessions to shared history."
        );
        let synced = state(Summary::Synced, &|_| {});
        assert!(!synced.finish_secondary && synced.reassurance.is_none());
    }

    #[test]
    fn the_conflicts_tab_shows_a_placeholder_until_conflict_review() {
        assert_eq!(conflicts_placeholder(0), "No unresolved conflicts.");
        assert_eq!(
            conflicts_placeholder(1),
            "1 unresolved conflict. Review them with the command line until conflict review arrives."
        );
        let none = view(&[running(0, "A", Summary::Synced, None)]).remove(0);
        assert_eq!(
            (none.conflict_count, none.conflicts_tab.as_str()),
            (0, "Conflicts, 0 unresolved")
        );
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
