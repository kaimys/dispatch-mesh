//! What the Conflicts tab and the conflict detail say (US00006).
//!
//! Pure functions over the engine's conflict list (`ConflictStatus`) and one
//! conflict's detail (`ConflictDetail`): wording, which view applies, the line
//! marks of a text comparison and which sides get an image preview. Times
//! travel as Unix seconds; the window formats them and replaces [`TIME`] in a
//! text with the value that goes with it.

use super::{AvatarView, YOU, avatar_color, initials, plural};
use dispatch_mesh::{
    conflicts::ConflictDetail,
    git::CommitInfo,
    status::{ConflictSide, ConflictStatus, Contributor},
};
use serde::Serialize;
use similar::{DiffOp, TextDiff};
use std::time::{Duration, Instant};

/// A text conflict is compared in the window only when every present side is
/// at most this big (Q6).
pub const COMPARE_LIMIT: usize = 1024 * 1024;
/// An image side gets a preview up to this size (Q5).
pub const PREVIEW_LIMIT: usize = 10 * 1024 * 1024;
/// The line diff gives up after this long, so one odd file cannot hang the window.
pub const DIFF_DEADLINE: Duration = Duration::from_millis(200);

/// Where the window puts a time: it replaces this in a text with the time
/// sent beside it.
pub const TIME: &str = "{time}";

pub const SHARED_HISTORY: &str = "Shared history";
pub const CURRENT_TITLE: &str = "Current version";
pub const OTHER_TITLE: &str = "Other version";
pub const CURRENT_SHOWN: &str = "Current version · shown";
pub const OTHER_KEPT: &str = "Other version · kept";
pub const IN_THE_FOLDER: &str = "In the folder now";
pub const KEPT_NOT_SHOWN: &str = "Kept, not shown";
pub const REVIEW_AND_CHOOSE: &str = "Review and choose";
pub const COMMON_BASE: &str = "Common base";
pub const TOO_LARGE: &str = "Too large to compare here";
pub const MARKS_UNAVAILABLE: &str = "Changes could not be marked here";
pub const NONE_RECORDED: &str = "Requested purpose: none recorded";
pub const FOOTER_TEXT: &str = "Both versions stay in history whichever you choose.";
pub const FOOTER_WHOLE_FILE: &str = "Whole-file choice only. Both versions stay in history.";
pub const NO_CONFLICTS: &str = "No unresolved conflicts.";
pub const NO_CONFLICTS_NOTE: &str =
    "When two people change the same file, both versions are kept and the conflict is listed here.";
pub const LIST_EXPLANATION: &str = "Two people changed the same file. Nothing was overwritten: both versions are kept, and the current version stays in the folder until you choose. Other files keep syncing.";
pub const IMAGES_CANT_BE_COMBINED: &str = "Images can't be combined. Choose which whole file stays in the folder; the other version stays kept.";
pub const FILES_CANT_BE_COMBINED: &str = "These files can't be combined. Choose which whole file stays in the folder; the other version stays kept.";
pub const RESOLVED_ELSEWHERE: &str = "This conflict was resolved elsewhere.";
pub const NO_PREVIEW: &str = "No preview for this file type.";
pub const PREVIEW_TOO_LARGE: &str = "Too large to preview here.";
const DIFFERS_FROM_OTHER: &str = "Differs from the other version";
const DIFFERS_FROM_CURRENT: &str = "Differs from the current version";

/// A text that carries a time the window formats, see [`TIME`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Stamped {
    pub text: String,
    pub time: Option<u64>,
}

impl Stamped {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            time: None,
        }
    }

    fn at(text: impl Into<String>, time: u64) -> Self {
        Self {
            text: text.into(),
            time: Some(time),
        }
    }
}

// ---- Images --------------------------------------------------------------

/// The image formats that get a preview. SVG is never one of them: it can
/// carry script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Image {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl Image {
    pub fn mime(self) -> &'static str {
        match self {
            Image::Png => "image/png",
            Image::Jpeg => "image/jpeg",
            Image::Gif => "image/gif",
            Image::Webp => "image/webp",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Image::Png => "PNG image",
            Image::Jpeg => "JPEG image",
            Image::Gif => "GIF image",
            Image::Webp => "WebP image",
        }
    }
}

/// The image format `bytes` really are, by magic number; the file name is not
/// trusted.
pub fn sniff_image(bytes: &[u8]) -> Option<Image> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(Image::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Image::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(Image::Gif)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(Image::Webp)
    } else {
        None
    }
}

/// The image format to preview: a known format, at most [`PREVIEW_LIMIT`].
pub fn previewable(bytes: &[u8]) -> Option<Image> {
    sniff_image(bytes).filter(|_| bytes.len() <= PREVIEW_LIMIT)
}

/// Whether a path's extension names an image the list may call an image. The
/// list has no bytes to sniff, so this only chooses a label.
fn image_extension(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, extension)| {
        ["png", "jpg", "jpeg", "gif", "webp", "svg"].contains(&extension.to_lowercase().as_str())
    })
}

// ---- Who -----------------------------------------------------------------

fn name_of(contributor: &Contributor, this_device: &str) -> String {
    if contributor.device == this_device {
        YOU.to_owned()
    } else {
        contributor.author.name.clone()
    }
}

/// The distinct people behind a side, newest save first; "You" for this
/// device's contributions (US00012 Q5), never a device.
fn people(side: &ConflictSide, this_device: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for contributor in &side.contributors {
        let name = name_of(contributor, this_device);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// "Alice", "Alice +1" for several, "Shared history" when nobody contributed.
fn who(side: &ConflictSide, this_device: &str) -> String {
    match people(side, this_device).as_slice() {
        [] => SHARED_HISTORY.to_owned(),
        [only] => only.clone(),
        [first, rest @ ..] => format!("{first} +{}", rest.len()),
    }
}

fn saved(side: &ConflictSide) -> Option<u64> {
    side.contributors.iter().map(|c| c.last_saved).max()
}

/// Requested purposes of all contributors, in order, without repeats.
fn purposes(side: &ConflictSide) -> Vec<&str> {
    let mut all: Vec<&str> = Vec::new();
    for purpose in side.contributors.iter().flat_map(|c| &c.purposes) {
        if !all.contains(&purpose.as_str()) {
            all.push(purpose);
        }
    }
    all
}

fn purpose_line(side: &ConflictSide) -> String {
    match purposes(side).as_slice() {
        [] => NONE_RECORDED.to_owned(),
        [only] => format!("Requested purpose: {only}"),
        [first, rest @ ..] => format!("Requested purpose: {first} +{} more", rest.len()),
    }
}

fn avatar(name: &str, email: &str, own: bool) -> AvatarView {
    AvatarView {
        initials: initials(name),
        color: if own {
            "you".to_owned()
        } else {
            format!("c{}", avatar_color(email))
        },
    }
}

fn side_avatar(side: &ConflictSide, this_device: &str) -> Option<AvatarView> {
    side.contributors
        .first()
        .map(|c| avatar(&c.author.name, &c.author.email, c.device == this_device))
}

// ---- The list ------------------------------------------------------------

/// One side of a conflict on a list card (frame 3d).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SideCard {
    /// "Current version · shown" or "Other version · kept".
    pub title: &'static str,
    pub author: String,
    /// The last save; `None` for shared history, which the list does not look up.
    pub saved: Option<u64>,
    /// "saved", or "deleted" for a side that removed the file.
    pub verb: &'static str,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConflictEntry {
    pub id: String,
    pub path: String,
    /// "Text", "Image · whole-file choice" or "File · whole-file choice".
    pub kind: &'static str,
    pub recorded: u64,
    pub current: SideCard,
    pub other: SideCard,
    /// The button's accessible name.
    pub review_label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConflictList {
    /// "N unresolved conflicts", or "No unresolved conflicts."
    pub heading: String,
    /// The line under the heading: the explanation, or the empty state's note.
    pub explanation: &'static str,
    pub entries: Vec<ConflictEntry>,
    /// What the window says when the conflict it had open is gone from the list.
    pub resolved_elsewhere: &'static str,
}

/// The order the list shows, and "n of N" and "next" follow: oldest first.
pub fn ordered(conflicts: &[ConflictStatus]) -> Vec<&ConflictStatus> {
    let mut list: Vec<_> = conflicts.iter().collect();
    list.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
    list
}

fn card(title: &'static str, side: &ConflictSide, this_device: &str) -> SideCard {
    SideCard {
        title,
        author: who(side, this_device),
        saved: saved(side),
        verb: if side.deleted { "deleted" } else { "saved" },
    }
}

pub fn list(conflicts: &[ConflictStatus], this_device: &str) -> ConflictList {
    let entries: Vec<ConflictEntry> = ordered(conflicts)
        .into_iter()
        .map(|c| ConflictEntry {
            id: c.id.clone(),
            path: c.path.clone(),
            kind: match (c.binary, image_extension(&c.path)) {
                (false, _) => "Text",
                (true, true) => "Image · whole-file choice",
                (true, false) => "File · whole-file choice",
            },
            recorded: c.created_at,
            current: card(CURRENT_SHOWN, &c.current, this_device),
            other: card(OTHER_KEPT, &c.other, this_device),
            review_label: format!("{REVIEW_AND_CHOOSE}: {}", c.path),
        })
        .collect();
    let (heading, explanation) = match entries.len() {
        0 => (NO_CONFLICTS.to_owned(), NO_CONFLICTS_NOTE),
        n => (
            plural(n, "unresolved conflict", "unresolved conflicts"),
            LIST_EXPLANATION,
        ),
    };
    ConflictList {
        heading,
        explanation,
        entries,
        resolved_elsewhere: RESOLVED_ELSEWHERE,
    }
}

// ---- The detail ----------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Line {
    pub number: usize,
    pub text: String,
    pub changed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Fact {
    pub label: &'static str,
    pub value: Stamped,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Preview {
    pub mime: &'static str,
}

/// One version's panel in the detail (frames 3e, 3f).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SidePanel {
    /// "Current version" or "Other version".
    pub title: &'static str,
    /// "In the folder now" or "Kept, not shown".
    pub badge: &'static str,
    pub avatar: Option<AvatarView>,
    /// "Alice · saved {time}", or "Shared history".
    pub byline: Stamped,
    /// "Requested purpose: …" or "none recorded".
    pub purpose: String,
    /// "Deleted by Bob · {time}", shown instead of content and a preview.
    pub placeholder: Option<Stamped>,
    /// Saved by, saved, size, type and purpose; for the binary view only.
    pub facts: Vec<Fact>,
    /// An image preview the window may fetch for this side.
    pub preview: Option<Preview>,
    /// Why a side of the binary view has no preview.
    pub no_preview: Option<&'static str>,
    /// The accessible name of a changed line's "≠" mark in this column.
    pub differs_label: &'static str,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BaseStrip {
    pub label: &'static str,
    /// "Before both changes", with where and when it is known from.
    pub text: Stamped,
    /// The base itself; empty when the file did not exist before.
    pub lines: Vec<Line>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Body {
    /// Two columns of numbered lines with their changed lines marked.
    Text {
        current: Vec<Line>,
        other: Vec<Line>,
        /// Why no line is marked, when the diff gave up.
        marks_unavailable: Option<&'static str>,
    },
    /// A text conflict too large to compare: metadata and whole-file choice.
    Metadata { reason: &'static str },
    /// A binary file: metadata, previews where possible, whole-file choice.
    Binary,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConflictDetailView {
    pub id: String,
    pub path: String,
    /// The breadcrumb's "1 of 2".
    pub position: String,
    pub summary: String,
    pub current: SidePanel,
    pub other: SidePanel,
    /// Only for a comparison: the strip behind **Show common base**.
    pub base: Option<BaseStrip>,
    pub body: Body,
    /// The note shown once the file was saved again after the conflict.
    pub note: Option<Stamped>,
    /// Whether the buttons work. Off while [`note`](Self::note) applies.
    pub can_choose: bool,
    pub keep_label: String,
    pub use_label: String,
    pub footer: &'static str,
}

fn lines_of(text: &str) -> Vec<Line> {
    text.split_inclusive('\n')
        .enumerate()
        .map(|(index, line)| Line {
            number: index + 1,
            text: line.trim_end_matches(['\r', '\n']).to_owned(),
            changed: false,
        })
        .collect()
}

/// The two versions as numbered lines, with the differing lines marked.
/// `None` for the marks when the diff ran out of time.
fn compare(current: &str, other: &str) -> (Vec<Line>, Vec<Line>, bool) {
    let started = Instant::now();
    let diff = TextDiff::configure()
        .deadline(started + DIFF_DEADLINE)
        .diff_lines(current, other);
    let timed_out = started.elapsed() >= DIFF_DEADLINE;
    let number = |slices: Vec<&str>| -> Vec<Line> {
        slices
            .into_iter()
            .enumerate()
            .map(|(index, line)| Line {
                number: index + 1,
                text: line.trim_end_matches(['\r', '\n']).to_owned(),
                changed: false,
            })
            .collect()
    };
    let mut left = number(diff.iter_old_slices().collect());
    let mut right = number(diff.iter_new_slices().collect());
    if !timed_out {
        let mark = |lines: &mut Vec<Line>, from: usize, count: usize| {
            for line in lines.iter_mut().skip(from).take(count) {
                line.changed = true;
            }
        };
        for op in diff.ops() {
            match *op {
                DiffOp::Equal { .. } => {}
                DiffOp::Delete {
                    old_index, old_len, ..
                } => mark(&mut left, old_index, old_len),
                DiffOp::Insert {
                    new_index, new_len, ..
                } => mark(&mut right, new_index, new_len),
                DiffOp::Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } => {
                    mark(&mut left, old_index, old_len);
                    mark(&mut right, new_index, new_len);
                }
            }
        }
    }
    (left, right, timed_out)
}

fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    match bytes {
        0..=1023 => plural(bytes as usize, "byte", "bytes"),
        b if (b as f64) < KB * KB => format!("{:.1} KB", b as f64 / KB),
        b => format!("{:.1} MB", b as f64 / (KB * KB)),
    }
}

fn type_label(path: &str, bytes: &[u8]) -> String {
    if let Some(image) = sniff_image(bytes) {
        return image.label().to_owned();
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_lowercase();
    match path.rsplit_once('.') {
        Some((_, extension)) if extension.eq_ignore_ascii_case("svg") => "SVG image".to_owned(),
        _ if head.contains("<svg") => "SVG image".to_owned(),
        Some((_, extension)) if !extension.is_empty() => {
            format!("{} file", extension.to_uppercase())
        }
        _ => "File".to_owned(),
    }
}

/// The button suffix naming a side: its author, "+N" for several, "shared
/// history" with no contributing session, "deleted by <author>" for a deletion.
fn suffix(side: &ConflictSide, this_device: &str) -> String {
    if side.shared_history {
        "shared history".to_owned()
    } else if side.deleted {
        format!("deleted by {}", who(side, this_device))
    } else {
        who(side, this_device)
    }
}

#[allow(clippy::too_many_arguments)]
fn panel(
    title: &'static str,
    badge: &'static str,
    side: &ConflictSide,
    bytes: &Option<Vec<u8>>,
    history: Option<&CommitInfo>,
    path: &str,
    facts: bool,
    this_device: &str,
) -> SidePanel {
    // A current version from shared history is known by its commit, when the
    // engine could name one.
    let by_history = side.shared_history.then_some(history).flatten();
    let name = match by_history {
        Some(commit) => commit.author.name.clone(),
        None => who(side, this_device),
    };
    let when = by_history.map(|commit| commit.time).or_else(|| saved(side));
    let byline = match (by_history, when) {
        (Some(_), Some(time)) => {
            Stamped::at(format!("{name} · shared history · saved {TIME}"), time)
        }
        (None, Some(time)) => Stamped::at(format!("{name} · saved {TIME}"), time),
        (_, None) => Stamped::plain(name.clone()),
    };
    let placeholder = side.deleted.then(|| match when {
        Some(time) => Stamped::at(format!("Deleted by {name} · {TIME}"), time),
        None => Stamped::plain(format!("Deleted by {name}")),
    });
    let content = bytes.as_deref().filter(|_| !side.deleted);
    let mut rows = Vec::new();
    if facts && !side.deleted {
        rows.push(Fact {
            label: "Saved by",
            value: Stamped::plain(name.clone()),
        });
        if let Some(time) = when {
            rows.push(Fact {
                label: "Saved",
                value: Stamped::at(TIME, time),
            });
        }
        rows.push(Fact {
            label: "Size",
            value: Stamped::plain(human_size(side.size)),
        });
        if let Some(bytes) = content {
            rows.push(Fact {
                label: "Type",
                value: Stamped::plain(type_label(path, bytes)),
            });
        }
        rows.push(Fact {
            label: "Purpose",
            value: Stamped::plain(match purposes(side).as_slice() {
                [] => "None recorded".to_owned(),
                all => all.join(", "),
            }),
        });
    }
    SidePanel {
        title,
        badge,
        avatar: match by_history {
            Some(commit) => Some(avatar(&commit.author.name, &commit.author.email, false)),
            None => side_avatar(side, this_device),
        },
        byline,
        purpose: purpose_line(side),
        placeholder,
        facts: rows,
        preview: content
            .filter(|_| facts)
            .and_then(previewable)
            .map(|image| Preview { mime: image.mime() }),
        no_preview: content
            .filter(|bytes| facts && previewable(bytes).is_none())
            .map(|bytes| match sniff_image(bytes) {
                Some(_) => PREVIEW_TOO_LARGE,
                None => NO_PREVIEW,
            }),
        differs_label: if title == CURRENT_TITLE {
            DIFFERS_FROM_OTHER
        } else {
            DIFFERS_FROM_CURRENT
        },
    }
}

/// A name inside a sentence: "You" reads "you" there.
fn mid_sentence(name: &str) -> String {
    if name == YOU {
        name.to_lowercase()
    } else {
        name.to_owned()
    }
}

fn summary(status: &ConflictStatus, binary_view: bool, image: bool, this_device: &str) -> String {
    let current = who(&status.current, this_device);
    let other = who(&status.other, this_device);
    if binary_view {
        return if image {
            IMAGES_CANT_BE_COMBINED
        } else {
            FILES_CANT_BE_COMBINED
        }
        .to_owned();
    }
    let ending = "Pick the version to keep in the folder.";
    if status.other.deleted {
        format!(
            "{other} deleted this file while {} changed it. {ending}",
            mid_sentence(&current)
        )
    } else if status.current.deleted {
        format!(
            "{current} deleted this file while {} changed it. {ending}",
            mid_sentence(&other)
        )
    } else {
        format!("{current} and {other} both changed this file. {ending}")
    }
}

/// The detail of one conflict, `n` of `of` in the list order.
pub fn detail_view(
    detail: &ConflictDetail,
    this_device: &str,
    (n, of): (usize, usize),
) -> ConflictDetailView {
    let status = &detail.status;
    let versions = [&detail.current, &detail.other];
    let too_large = !status.binary
        && versions
            .iter()
            .any(|v| v.as_ref().is_some_and(|bytes| bytes.len() > COMPARE_LIMIT));
    let text_view = !status.binary && !too_large;
    let image = [&detail.current, &detail.other].iter().any(|v| {
        v.as_deref()
            .is_some_and(|bytes| sniff_image(bytes).is_some())
    });

    let current = panel(
        CURRENT_TITLE,
        IN_THE_FOLDER,
        &status.current,
        &detail.current,
        detail.current_history.as_ref(),
        &status.path,
        !text_view,
        this_device,
    );
    let other = panel(
        OTHER_TITLE,
        KEPT_NOT_SHOWN,
        &status.other,
        &detail.other,
        None,
        &status.path,
        !text_view,
        this_device,
    );

    let (body, base) = if text_view {
        let text = |bytes: &Option<Vec<u8>>| {
            bytes
                .as_deref()
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        };
        let (current_lines, other_lines, unavailable) =
            match (text(&detail.current), text(&detail.other)) {
                // A deletion has nothing to compare: its side shows a
                // placeholder and the other side's lines stay unmarked.
                (Some(current), Some(other)) => compare(&current, &other),
                (current, other) => (
                    current.as_deref().map(lines_of).unwrap_or_default(),
                    other.as_deref().map(lines_of).unwrap_or_default(),
                    false,
                ),
            };
        let strip = match (&detail.base, &detail.base_history) {
            (None, _) => BaseStrip {
                label: COMMON_BASE,
                text: Stamped::plain("Before both changes · the file did not exist"),
                lines: Vec::new(),
            },
            (Some(bytes), Some(commit)) => BaseStrip {
                label: COMMON_BASE,
                text: Stamped::at(
                    format!("Before both changes · shared history, {TIME}"),
                    commit.time,
                ),
                lines: lines_of(&String::from_utf8_lossy(bytes)),
            },
            (Some(bytes), None) => BaseStrip {
                label: COMMON_BASE,
                text: Stamped::plain("Before both changes"),
                lines: lines_of(&String::from_utf8_lossy(bytes)),
            },
        };
        (
            Body::Text {
                current: current_lines,
                other: other_lines,
                marks_unavailable: unavailable.then_some(MARKS_UNAVAILABLE),
            },
            Some(strip),
        )
    } else if too_large {
        (Body::Metadata { reason: TOO_LARGE }, None)
    } else {
        (Body::Binary, None)
    };

    let note = detail.edited_after.map(|time| {
        Stamped::at(
            format!(
                "You saved this file again at {TIME}, after the conflict was recorded. Your edit is safe. Choosing a version is possible once the file matches the current version again."
            ),
            time,
        )
    });
    ConflictDetailView {
        id: status.id.clone(),
        path: status.path.clone(),
        position: format!("{n} of {of}"),
        summary: summary(status, status.binary, image, this_device),
        current,
        other,
        base,
        body,
        can_choose: note.is_none(),
        note,
        keep_label: format!(
            "Keep current version ({})",
            suffix(&status.current, this_device)
        ),
        use_label: format!("Use other version ({})", suffix(&status.other, this_device)),
        footer: if text_view {
            FOOTER_TEXT
        } else {
            FOOTER_WHOLE_FILE
        },
    }
}

// ---- Choosing ------------------------------------------------------------

/// Which version stays in the folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Current,
    Other,
}

impl Choice {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "current" => Some(Choice::Current),
            "other" => Some(Choice::Other),
            _ => None,
        }
    }

    /// The engine's `incoming` flag: the other version is the one it calls so.
    pub fn incoming(self) -> bool {
        self == Choice::Other
    }
}

/// The line the window shows after a choice (Q8).
pub fn confirmation(path: &str, choice: Choice) -> String {
    let what = match choice {
        Choice::Current => "kept the current version",
        Choice::Other => "used the other version",
    };
    format!(
        "{path}: {what}. Both versions stay kept; your choice is shared with collaborators on the next sync."
    )
}

/// The conflict to open after `resolved`: the next one in list order, wrapping
/// to the first that remains; `None` when no other conflict is left. `ids` is
/// the list in [`ordered`] order, including the resolved one.
pub fn next_after(resolved: &str, ids: &[&str]) -> Option<String> {
    let at = ids.iter().position(|id| *id == resolved)?;
    ids[at + 1..]
        .iter()
        .chain(ids[..at].iter())
        .next()
        .map(|id| (*id).to_owned())
}

/// What a successful choice sends back to the window.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Resolved {
    pub confirmation: String,
    /// The conflict to open next; `None` returns to the list.
    pub next: Option<String>,
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use dispatch_mesh::model::Identity;

    const THIS: &str = "this-device";

    fn person(name: &str) -> Identity {
        Identity {
            name: name.into(),
            email: format!("{}@example.test", name.to_lowercase()),
        }
    }

    fn contributor(name: &str, device: &str, saved: u64, purposes: &[&str]) -> Contributor {
        Contributor {
            author: person(name),
            device: device.into(),
            last_saved: saved,
            purposes: purposes.iter().map(|p| (*p).to_owned()).collect(),
        }
    }

    fn side(contributors: Vec<Contributor>, size: u64) -> ConflictSide {
        ConflictSide {
            shared_history: contributors.is_empty(),
            contributors,
            deleted: false,
            size,
        }
    }

    /// A conflict with Alice's version in the folder and Bob's kept aside.
    pub(crate) fn conflict(id: &str, path: &str, binary: bool) -> ConflictStatus {
        ConflictStatus {
            id: id.into(),
            path: path.into(),
            binary,
            created_at: 1_000,
            current: side(vec![contributor("Alice", "alice-device", 900, &[])], 10),
            other: side(vec![contributor("Bob", "bob-device", 950, &[])], 10),
        }
    }

    const RETENTION: &str = "# Data retention\n\nHow long we keep exports from the shared drive.\n\nretention: 60 days\nowner: Operations\n";

    fn detail_of(
        status: ConflictStatus,
        current: Option<&[u8]>,
        other: Option<&[u8]>,
        base: Option<&[u8]>,
    ) -> ConflictDetail {
        ConflictDetail {
            status,
            base: base.map(<[u8]>::to_vec),
            current: current.map(<[u8]>::to_vec),
            other: other.map(<[u8]>::to_vec),
            edited_after: None,
            current_history: None,
            base_history: None,
        }
    }

    fn retention() -> ConflictDetail {
        detail_of(
            conflict("c1", "policies/retention.md", false),
            Some(RETENTION.as_bytes()),
            Some(RETENTION.replace("60", "90").as_bytes()),
            Some(RETENTION.replace("60", "30").as_bytes()),
        )
    }

    fn text_lines(view: &ConflictDetailView) -> (&[Line], &[Line]) {
        match &view.body {
            Body::Text { current, other, .. } => (current, other),
            body => panic!("not a comparison: {body:?}"),
        }
    }

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];

    #[test]
    fn list_entries_name_the_path_kind_time_and_each_side() {
        let mut text = conflict("t", "policies/retention.md", false);
        text.current.contributors = vec![contributor("Alice", THIS, 900, &[])];
        let mut image = conflict("i", "assets/logo.png", true);
        image.created_at = 1_001;
        let mut other_file = conflict("f", "data.bin", true);
        other_file.created_at = 1_002;
        let list = list(&[text, image, other_file], THIS);
        assert_eq!(list.heading, "3 unresolved conflicts");
        assert_eq!(list.explanation, LIST_EXPLANATION);
        let kinds: Vec<_> = list.entries.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            [
                "Text",
                "Image · whole-file choice",
                "File · whole-file choice"
            ]
        );
        let first = &list.entries[0];
        assert_eq!(first.recorded, 1_000);
        assert_eq!(first.current.title, "Current version · shown");
        assert_eq!(first.other.title, "Other version · kept");
        assert_eq!(
            (first.current.author.as_str(), first.current.saved),
            ("You", Some(900)),
            "this device's contribution reads You"
        );
        assert_eq!(first.other.author, "Bob");
        assert_eq!(first.current.verb, "saved");
        assert_eq!(
            first.review_label,
            "Review and choose: policies/retention.md"
        );
    }

    #[test]
    fn list_names_shared_history_several_contributors_and_a_deleter() {
        let mut c = conflict("a", "a.md", false);
        c.current = side(Vec::new(), 4);
        c.other.deleted = true;
        let entry = &list(&[c], THIS).entries[0];
        assert_eq!(entry.current.author, "Shared history");
        assert_eq!(entry.current.saved, None, "the list never looks it up");
        assert_eq!(entry.other.verb, "deleted");

        let mut several = conflict("b", "b.md", false);
        several.current = side(
            vec![
                contributor("Alice", "d1", 900, &[]),
                contributor("Carol", "d2", 800, &[]),
                contributor("Alice", "d3", 700, &[]),
                contributor("Dave", "d4", 600, &[]),
            ],
            4,
        );
        let entry = &list(&[several], THIS).entries[0];
        assert_eq!(entry.current.author, "Alice +2", "distinct people only");
        assert_eq!(entry.current.saved, Some(900), "the newest save");
    }

    #[test]
    fn an_empty_list_says_so_and_a_single_conflict_is_singular() {
        let none = list(&[], THIS);
        assert_eq!(none.heading, "No unresolved conflicts.");
        assert_eq!(none.explanation, NO_CONFLICTS_NOTE);
        assert!(none.entries.is_empty());
        let one = list(&[conflict("a", "a.md", false)], THIS);
        assert_eq!(one.heading, "1 unresolved conflict");
    }

    #[test]
    fn the_list_runs_oldest_first_and_next_follows_it() {
        let mut late = conflict("a", "a.md", false);
        late.created_at = 3_000;
        let mut early = conflict("z", "z.md", false);
        early.created_at = 1_000;
        let mut middle = conflict("m", "m.md", false);
        middle.created_at = 2_000;
        let all = [late, early, middle];
        let ids: Vec<&str> = ordered(&all).iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["z", "m", "a"]);
        assert_eq!(next_after("z", &ids).as_deref(), Some("m"), "at the start");
        assert_eq!(next_after("m", &ids).as_deref(), Some("a"), "in the middle");
        assert_eq!(
            next_after("a", &ids).as_deref(),
            Some("z"),
            "at the end it wraps to what remains"
        );
        assert_eq!(next_after("z", &["z"]), None, "nothing else is left");
        assert_eq!(next_after("missing", &ids), None);
    }

    #[test]
    fn the_confirmation_says_what_was_chosen_and_that_both_versions_stay() {
        assert_eq!(
            confirmation("policies/retention.md", Choice::Current),
            "policies/retention.md: kept the current version. Both versions stay kept; your choice is shared with collaborators on the next sync."
        );
        assert!(confirmation("a.md", Choice::Other).starts_with("a.md: used the other version."));
        assert_eq!(Choice::parse("current"), Some(Choice::Current));
        assert_eq!(Choice::parse("other"), Some(Choice::Other));
        assert_eq!(Choice::parse("incoming"), None);
        assert!(Choice::Other.incoming() && !Choice::Current.incoming());
    }

    #[test]
    fn a_text_conflict_compares_the_versions_and_marks_the_changed_lines() {
        let view = detail_view(&retention(), THIS, (1, 2));
        assert_eq!(view.position, "1 of 2");
        assert_eq!(
            view.summary,
            "Alice and Bob both changed this file. Pick the version to keep in the folder."
        );
        let (current, other) = text_lines(&view);
        assert_eq!(current.len(), 6);
        let changed = |lines: &[Line]| -> Vec<usize> {
            lines
                .iter()
                .filter(|l| l.changed)
                .map(|l| l.number)
                .collect()
        };
        assert_eq!(changed(current), [5]);
        assert_eq!(changed(other), [5]);
        assert_eq!(current[4].text, "retention: 60 days");
        assert_eq!(other[4].text, "retention: 90 days");
        assert_eq!(current[0].text, "# Data retention");
        assert_eq!(view.current.badge, "In the folder now");
        assert_eq!(view.other.badge, "Kept, not shown");
        assert_eq!(view.current.differs_label, "Differs from the other version");
        assert_eq!(view.other.differs_label, "Differs from the current version");
        assert_eq!(view.current.purpose, "Requested purpose: none recorded");
        assert_eq!(view.footer, FOOTER_TEXT);
        assert!(
            view.current.facts.is_empty(),
            "metadata is for the binary view"
        );
    }

    #[test]
    fn inserted_and_removed_lines_are_marked_on_their_own_side_only() {
        let detail = detail_of(
            conflict("c", "list.md", false),
            Some(b"a\nb\nc\n"),
            Some(b"a\nc\nd\n"),
            None,
        );
        let view = detail_view(&detail, THIS, (1, 1));
        let (current, other) = text_lines(&view);
        fn marked(lines: &[Line]) -> Vec<&str> {
            lines
                .iter()
                .filter(|l| l.changed)
                .map(|l| l.text.as_str())
                .collect()
        }
        assert_eq!(marked(current), ["b"]);
        assert_eq!(marked(other), ["d"]);
    }

    #[test]
    fn the_base_comes_with_its_history_when_it_is_known() {
        let mut detail = retention();
        let view = detail_view(&detail, THIS, (1, 1));
        let base = view.base.as_ref().unwrap();
        assert_eq!(base.label, "Common base");
        assert_eq!(base.text, Stamped::plain("Before both changes"));
        assert_eq!(base.lines.len(), 6);
        assert!(base.lines[4].text.contains("30 days"));

        detail.base_history = Some(CommitInfo {
            author: person("Alice"),
            time: 500,
            blob: None,
        });
        let view = detail_view(&detail, THIS, (1, 1));
        assert_eq!(
            view.base.unwrap().text,
            Stamped::at("Before both changes · shared history, {time}", 500)
        );

        let added = detail_of(conflict("n", "new.md", false), Some(b"x"), Some(b"y"), None);
        let view = detail_view(&added, THIS, (1, 1));
        assert!(view.base.unwrap().lines.is_empty());
    }

    #[test]
    fn a_current_version_from_shared_history_is_named_by_its_commit_or_not_at_all() {
        let mut detail = retention();
        detail.status.current = side(Vec::new(), 10);
        let view = detail_view(&detail, THIS, (1, 1));
        assert_eq!(view.current.byline, Stamped::plain("Shared history"));
        assert_eq!(view.keep_label, "Keep current version (shared history)");
        assert_eq!(
            view.summary,
            "Shared history and Bob both changed this file. Pick the version to keep in the folder."
        );

        detail.current_history = Some(CommitInfo {
            author: person("Alice"),
            time: 700,
            blob: Some("0".repeat(40)),
        });
        let view = detail_view(&detail, THIS, (1, 1));
        assert_eq!(
            view.current.byline,
            Stamped::at("Alice · shared history · saved {time}", 700)
        );
        assert!(view.current.avatar.is_some());
        assert_eq!(view.keep_label, "Keep current version (shared history)");
    }

    #[test]
    fn buttons_name_the_authors_and_several_contributors_read_plus_n() {
        let mut detail = retention();
        let view = detail_view(&detail, THIS, (1, 1));
        assert_eq!(view.keep_label, "Keep current version (Alice)");
        assert_eq!(view.use_label, "Use other version (Bob)");

        detail.status.current.contributors = vec![
            contributor("Alice", THIS, 900, &[]),
            contributor("Carol", "d2", 800, &[]),
        ];
        let view = detail_view(&detail, THIS, (1, 1));
        assert_eq!(view.keep_label, "Keep current version (You +1)");
        assert!(view.current.byline.text.starts_with("You +1 · saved"));
    }

    #[test]
    fn a_purpose_is_shown_per_side_with_more_counted() {
        let mut detail = retention();
        detail.status.other.contributors[0].purposes = vec!["Shorten retention".into()];
        let view = detail_view(&detail, THIS, (1, 1));
        assert_eq!(view.other.purpose, "Requested purpose: Shorten retention");
        detail.status.other.contributors[0].purposes = vec![
            "Shorten retention".into(),
            "Fix typo".into(),
            "Rename".into(),
        ];
        let view = detail_view(&detail, THIS, (1, 1));
        assert_eq!(
            view.other.purpose,
            "Requested purpose: Shorten retention +2 more"
        );
    }

    #[test]
    fn a_deleted_side_shows_a_placeholder_and_names_the_deleter_on_its_button() {
        let mut detail = retention();
        detail.status.other.deleted = true;
        detail.other = None;
        let view = detail_view(&detail, THIS, (1, 1));
        assert_eq!(
            view.other.placeholder,
            Some(Stamped::at("Deleted by Bob · {time}", 950))
        );
        assert_eq!(view.use_label, "Use other version (deleted by Bob)");
        assert_eq!(view.keep_label, "Keep current version (Alice)");
        assert_eq!(
            view.summary,
            "Bob deleted this file while Alice changed it. Pick the version to keep in the folder."
        );
        let (current, other) = text_lines(&view);
        assert!(
            current.iter().all(|l| !l.changed),
            "no diff against a deletion"
        );
        assert!(other.is_empty());

        let mut detail = retention();
        detail.status.current.deleted = true;
        detail.current = None;
        let view = detail_view(&detail, THIS, (1, 1));
        assert!(view.current.placeholder.is_some());
        assert_eq!(view.keep_label, "Keep current version (deleted by Alice)");

        // "You" is lower-cased inside a sentence.
        let mut detail = retention();
        detail.status.current.contributors[0].device = THIS.into();
        detail.status.other.deleted = true;
        detail.other = None;
        let view = detail_view(&detail, THIS, (1, 1));
        assert_eq!(
            view.summary,
            "Bob deleted this file while you changed it. Pick the version to keep in the folder."
        );
    }

    #[test]
    fn exactly_one_megabyte_is_compared_and_one_byte_more_is_not() {
        let mut exact = vec![b'a'; COMPARE_LIMIT - 1];
        exact.push(b'\n');
        let other = vec![b'b'; COMPARE_LIMIT];
        let detail = detail_of(
            conflict("c", "big.md", false),
            Some(&exact),
            Some(&other),
            None,
        );
        assert!(matches!(
            detail_view(&detail, THIS, (1, 1)).body,
            Body::Text { .. }
        ));

        let mut over = exact.clone();
        over.push(b'x');
        let detail = detail_of(
            conflict("c", "big.md", false),
            Some(&over),
            Some(&other),
            None,
        );
        let view = detail_view(&detail, THIS, (1, 1));
        assert_eq!(view.body, Body::Metadata { reason: TOO_LARGE });
        assert_eq!(view.footer, FOOTER_WHOLE_FILE, "both choices remain");
        assert!(view.can_choose);
        assert!(view.base.is_none());
        let facts: Vec<_> = view.current.facts.iter().map(|f| f.label).collect();
        assert_eq!(facts, ["Saved by", "Saved", "Size", "Type", "Purpose"]);
    }

    #[test]
    fn a_later_save_shows_the_note_and_disables_both_choices() {
        let mut detail = retention();
        let view = detail_view(&detail, THIS, (1, 1));
        assert!(view.can_choose && view.note.is_none());

        detail.edited_after = Some(1_200);
        let view = detail_view(&detail, THIS, (1, 1));
        assert!(!view.can_choose);
        assert_eq!(
            view.note,
            Some(Stamped::at(
                "You saved this file again at {time}, after the conflict was recorded. Your edit is safe. Choosing a version is possible once the file matches the current version again.",
                1_200
            ))
        );
    }

    #[test]
    fn a_binary_conflict_lists_metadata_and_previews_only_known_images() {
        let mut status = conflict("b", "assets/logo.png", true);
        status.current.size = PNG.len() as u64;
        status.other.size = 3;
        let detail = detail_of(status, Some(PNG), Some(b"<svg/>"), None);
        let view = detail_view(&detail, THIS, (2, 2));
        assert_eq!(view.body, Body::Binary);
        assert_eq!(view.summary, IMAGES_CANT_BE_COMBINED);
        assert_eq!(
            view.footer,
            "Whole-file choice only. Both versions stay in history."
        );
        assert!(view.base.is_none());
        assert_eq!(view.current.preview, Some(Preview { mime: "image/png" }));
        assert_eq!(view.other.preview, None, "an SVG is never previewed");
        assert_eq!(view.other.no_preview, Some(NO_PREVIEW));
        let fact = |panel: &SidePanel, label: &str| -> String {
            let fact = panel.facts.iter().find(|f| f.label == label).unwrap();
            fact.value.text.clone()
        };
        assert_eq!(fact(&view.current, "Saved by"), "Alice");
        assert_eq!(fact(&view.current, "Type"), "PNG image");
        assert_eq!(fact(&view.current, "Size"), "12 bytes");
        assert_eq!(fact(&view.current, "Purpose"), "None recorded");
        assert_eq!(fact(&view.other, "Type"), "SVG image");
        assert_eq!(fact(&view.other, "Saved"), TIME);
        assert_eq!(view.keep_label, "Keep current version (Alice)");
    }

    #[test]
    fn a_binary_file_that_is_not_an_image_says_files_not_images() {
        let detail = detail_of(
            conflict("b", "data.bin", true),
            Some(&[0, 1, 2]),
            Some(&[0, 3]),
            None,
        );
        let view = detail_view(&detail, THIS, (1, 1));
        assert_eq!(view.summary, FILES_CANT_BE_COMBINED);
        assert_eq!(
            view.current
                .facts
                .iter()
                .find(|f| f.label == "Type")
                .unwrap()
                .value
                .text,
            "BIN file"
        );
        assert!(view.current.preview.is_none());
    }

    #[test]
    fn an_image_over_ten_megabytes_is_not_previewed() {
        let mut big = PNG.to_vec();
        big.resize(PREVIEW_LIMIT, 0);
        assert!(previewable(&big).is_some(), "exactly 10 MB is previewed");
        big.push(0);
        assert!(previewable(&big).is_none(), "one byte more is not");
        assert!(sniff_image(&big).is_some());
        let detail = detail_of(conflict("b", "big.png", true), Some(&big), Some(PNG), None);
        let view = detail_view(&detail, THIS, (1, 1));
        assert_eq!(view.current.preview, None);
        assert_eq!(view.current.no_preview, Some(PREVIEW_TOO_LARGE));
        assert_eq!(view.other.preview.map(|p| p.mime), Some("image/png"));
    }

    #[test]
    fn previews_are_decided_by_magic_bytes_not_by_names() {
        assert_eq!(sniff_image(PNG), Some(Image::Png));
        assert_eq!(
            sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]),
            Some(Image::Jpeg)
        );
        assert_eq!(sniff_image(b"GIF89a\x01\x00"), Some(Image::Gif));
        assert_eq!(sniff_image(b"GIF87a\x01\x00"), Some(Image::Gif));
        assert_eq!(sniff_image(b"RIFF\x24\0\0\0WEBPVP8 "), Some(Image::Webp));
        assert_eq!(sniff_image(b"RIFF\x24\0\0\0WAVEfmt "), None);
        assert_eq!(
            sniff_image(b"<svg xmlns='http://www.w3.org/2000/svg'/>"),
            None
        );
        assert_eq!(sniff_image(b"just text renamed to logo.png"), None);
        assert_eq!(sniff_image(b""), None);
        assert_eq!(Image::Png.mime(), "image/png");
        assert_eq!(Image::Jpeg.mime(), "image/jpeg");
        assert_eq!(Image::Gif.mime(), "image/gif");
        assert_eq!(Image::Webp.mime(), "image/webp");
    }

    #[test]
    fn no_conflict_wording_uses_engine_terms_or_saved_copy() {
        let mut deleted = retention();
        deleted.status.other.deleted = true;
        deleted.other = None;
        let mut shared = retention();
        shared.status.current = side(Vec::new(), 1);
        shared.edited_after = Some(5);
        let image = detail_of(
            conflict("b", "logo.png", true),
            Some(PNG),
            Some(&[0, 1]),
            None,
        );
        let mut texts = Vec::new();
        for detail in [retention(), deleted, shared, image] {
            texts.push(serde_json::to_value(detail_view(&detail, THIS, (1, 2))).unwrap());
        }
        let all = [conflict("a", "a.md", false), conflict("b", "b.png", true)];
        texts.push(serde_json::to_value(list(&all, THIS)).unwrap());
        texts.push(serde_json::to_value(confirmation("a.md", Choice::Current)).unwrap());
        texts.push(serde_json::to_value(RESOLVED_ELSEWHERE).unwrap());
        fn strings(value: &serde_json::Value, into: &mut Vec<String>) {
            match value {
                serde_json::Value::String(text) => into.push(text.to_lowercase()),
                serde_json::Value::Array(items) => items.iter().for_each(|v| strings(v, into)),
                serde_json::Value::Object(map) => map.values().for_each(|v| strings(v, into)),
                _ => {}
            }
        }
        let mut all_text = Vec::new();
        texts.iter().for_each(|v| strings(v, &mut all_text));
        assert!(!all_text.is_empty());
        for text in &all_text {
            assert!(!text.contains("saved copy"), "{text}");
            for word in text.split(|c: char| !c.is_alphanumeric()) {
                assert!(
                    !["ours", "theirs", "accepted", "incoming", "head"].contains(&word),
                    "{word} in {text}"
                );
            }
        }
    }

    #[test]
    fn a_pathological_file_cannot_hang_the_comparison() {
        // Many distinct lines on both sides, interleaved: a worst case for an
        // exact diff. The view still arrives, within the deadline plus slack.
        let current: String = (0..40_000).map(|i| format!("a{}\n", i * 2)).collect();
        let other: String = (0..40_000).map(|i| format!("b{}\n", i * 3)).collect();
        let detail = detail_of(
            conflict("c", "worst.md", false),
            Some(current.as_bytes()),
            Some(other.as_bytes()),
            None,
        );
        let started = Instant::now();
        let view = detail_view(&detail, THIS, (1, 1));
        assert!(started.elapsed() < Duration::from_secs(5));
        let (left, right) = text_lines(&view);
        assert_eq!((left.len(), right.len()), (40_000, 40_000));
    }
}
