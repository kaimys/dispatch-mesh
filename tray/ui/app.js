// Status window. Renders the host's view model; all wording comes from
// `model.rs`, so the tray and the window always agree. Data is only ever
// inserted as text, never as markup. Layout follows the desktop app design
// spec (US00012).
"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const app = document.getElementById("app");
const announce = document.getElementById("announce");
let workspaces = [];
let selected = null; // workspace id shown in detail, with several workspaces
let lastAnnounced = "";
const finishing = new Set(); // workspace ids whose Finish session is under way
const errors = new Map(); // workspace id -> last failed action's message
const copiesOpen = new Set(); // workspace ids whose saved-copies list is expanded
const tabs = new Map(); // workspace id -> "activity" | "conflicts"
const conflictOpen = new Map(); // workspace id -> the conflict open in detail (US00006)
const notices = new Map(); // workspace id -> a dismissible line, like the line after a choice
let wide = false; // whether the window is in the widened detail layout
let lastView = ""; // identifies the view last drawn, to keep its scroll position

const SVG = "http://www.w3.org/2000/svg";

function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (key === "class") node.className = value;
    else if (key === "onclick") node.addEventListener("click", value);
    else if (key === "onkeydown") node.addEventListener("keydown", value);
    else node.setAttribute(key, value);
  }
  for (const child of children) {
    if (child !== null && child !== undefined) {
      node.append(typeof child === "string" ? document.createTextNode(child) : child);
    }
  }
  return node;
}

function svg(tag, attrs) {
  const node = document.createElementNS(SVG, tag);
  for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, value);
  return node;
}

// The app mark from the tray icon (design spec, *Tray icon*), without a badge;
// the neutral state's glyph (already running).
function mark(size, dim = false) {
  const root = svg("svg", { width: size, height: size, viewBox: "0 0 16 16", "aria-hidden": "true", class: "glyph" });
  const g = svg("g", { fill: "var(--mark)", stroke: "var(--mark)", opacity: dim ? "0.5" : "1" });
  g.append(svg("path", { d: "M4 4L12 4M4 4L4 12M12 4L4 12", "stroke-width": "1.4", fill: "none" }));
  for (const [cx, cy] of [[4, 4], [12, 4], [4, 12]]) {
    g.append(svg("circle", { cx, cy, r: "2.2", stroke: "none" }));
  }
  root.append(g);
  return root;
}

// A status glyph exported by the design (ui/glyphs), light or dark with the
// system theme. Its accessible name is the state label.
function statusGlyph(name, label, size) {
  if (name === "none") return mark(size, true);
  return el("picture", { class: "glyph" },
    el("source", { srcset: `glyphs/status-${name}-dark.svg`, media: "(prefers-color-scheme: dark)" }),
    el("img", { src: `glyphs/status-${name}-light.svg`, alt: label, width: size, height: size, class: "glyph" }));
}

function retryButton(ws) {
  return el("button", {
    class: "secondary",
    "data-key": `retry-${ws.id}`,
    onclick: () => invoke("retry", { id: ws.id }),
  }, "Retry");
}

// Times arrive as Unix seconds. Relative times are redrawn every 30 s.
function ago(seconds) {
  const minutes = Math.floor((Date.now() / 1000 - seconds) / 60);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} h ago`;
  return `on ${at(seconds)}`;
}

function at(seconds) {
  const date = new Date(seconds * 1000);
  const today = new Date().toDateString() === date.toDateString();
  return today
    ? date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : date.toLocaleString([], { dateStyle: "medium", timeStyle: "short" });
}

function syncLine(ws) {
  if (ws.sync_label) return ws.sync_label;
  if (ws.last_sync) return `Last synced ${ago(ws.last_sync)}`;
  return null;
}

async function finishSession(ws) {
  finishing.add(ws.id);
  errors.delete(ws.id);
  render();
  try {
    await invoke("finish_session", { id: ws.id });
  } catch (error) {
    errors.set(ws.id, `Couldn't finish the session: ${error}`);
  }
  finishing.delete(ws.id);
  render();
}

function finishButton(ws, primary) {
  const busy = finishing.has(ws.id);
  return el("button", {
    class: primary ? "primary" : "secondary",
    "data-key": `finish-${ws.id}`,
    "aria-label": `Finish session in ${ws.name}`,
    ...(busy ? { disabled: "", "aria-busy": "true" } : {}),
    onclick: () => finishSession(ws),
  }, busy ? "Finishing…" : "Finish session");
}

function showTab(ws, tab) {
  tabs.set(ws.id, tab);
  render();
  const again = app.querySelector(`[data-key="tab-${tab}-${ws.id}"]`);
  if (again) again.focus();
}

function openConflictsButton(ws) {
  return el("button", {
    class: "primary",
    "data-key": `open-conflicts-${ws.id}`,
    onclick: () => showTab(ws, "conflicts"),
  }, "Open conflicts");
}

function alreadyRunning(ws) {
  return el("section", { class: "card", "aria-labelledby": `state-${ws.id}` },
    el("div", { class: "card-main" },
      statusGlyph(ws.glyph, ws.label, 28),
      el("div", { class: "card-text" },
        el("p", { class: "state-label", id: `state-${ws.id}` }, ws.label),
        el("p", { class: "body" },
          `Another Dispatch Mesh — from the command line or a second tray app — is looking after ${ws.name}. ` +
          "Only one can at a time. Your files keep syncing through it."))),
    el("div", { class: "actions" },
      retryButton(ws),
      el("p", { class: "note" }, "Retry after closing the other one.")));
}

function card(ws) {
  if (ws.already_running) return alreadyRunning(ws);
  const sync = syncLine(ws);
  const buttons = [
    ws.action === "open_conflicts" ? openConflictsButton(ws) : null,
    ws.action === "finish_session" ? finishButton(ws, true) : null,
    ws.finish_secondary ? finishButton(ws, false) : null,
    ws.retry ? retryButton(ws) : null,
  ].filter(Boolean);
  return el("section", { class: `card ${ws.tone}`, "aria-labelledby": `state-${ws.id}` },
    el("div", { class: "card-main" },
      statusGlyph(ws.glyph, ws.label, 28),
      el("div", { class: "card-text" },
        el("p", { class: "state-label", id: `state-${ws.id}` }, ws.label),
        ws.body ? el("p", { class: "body" }, ws.body) : null,
        ws.reassurance ? el("p", { class: "reassurance" }, ws.reassurance) : null)),
    buttons.length || sync
      ? el("div", { class: "actions" }, ...buttons, sync ? el("p", { class: "sync" }, sync) : null)
      : null);
}

// One session row (design spec, *Session row*). The track is drawn for the
// eye only; screen readers hear the stage once, as "Stage: …, 2 of 4".
function sessionItem(session) {
  const meta = `${session.files_label} · started ${at(session.started_at)} · last saved ${ago(session.last_saved)}`;
  // The row's tooltip names the author and lists the changed files; screen
  // readers get the files as text, since they don't read tooltips.
  return el("li", { class: "session", title: session.tooltip },
    el("div", { class: `avatar ${session.avatar.color}`, "aria-hidden": "true" }, session.avatar.initials),
    el("div", { class: "session-body" },
      el("p", { class: "line1" },
        el("span", { class: "author ellipsis" }, session.author),
        // Clipped parts keep their own tooltip with the full text; the rest
        // of the row shows the author and changed files (review re-check F3).
        el("span", { class: "meta ellipsis", title: meta }, meta)),
      // A summary title wraps; a file list stays on one line.
      el("p", { class: session.line_is_title ? "line2 title" : "line2 ellipsis" }, session.line),
      session.line_is_title ? el("span", { class: "visually-hidden" }, session.files_text) : null,
      session.purpose ? el("p", { class: "purpose ellipsis", title: session.purpose }, session.purpose) : null,
      el("div", { class: "line3" },
        session.discarded
          ? el("span", { class: "tag", "aria-hidden": "true" }, session.stage)
          : el("span", { class: `track n${session.segments.length}`, "aria-hidden": "true" },
            ...session.segments.map((kind) => el("span", { class: `seg ${kind}` }))),
        session.discarded ? null : el("span", { class: "stage", "aria-hidden": "true" }, session.stage),
        el("span", { class: "visually-hidden" }, session.stage_text))));
}

function activity(ws) {
  return [
    ws.sessions.length
      ? el("ol", { class: "sessions", "aria-label": "Recent activity" }, ...ws.sessions.map(sessionItem))
      : el("p", { class: "muted" }, "No sessions yet. Edits you save here appear in this list."),
    savedCopies(ws),
  ];
}

function savedCopies(ws) {
  if (ws.saved_copies.length === 0) return null;
  const details = el("details", { class: "copies", ...(copiesOpen.has(ws.id) ? { open: "" } : {}) },
    el("summary", { "data-key": `copies-${ws.id}` }, `Saved copies · ${ws.saved_copies.length}`),
    el("p", { class: "muted" },
      "Mesh kept these versions aside before writing incoming changes over them. To use one, copy it back yourself."),
    el("ul", { class: "copy-list" }, ...ws.saved_copies.map((copy) =>
      el("li", { class: "copy" },
        el("span", { class: "path ellipsis", title: copy.path }, copy.path),
        el("span", { class: "muted" }, `kept ${at(copy.created_at)}`),
        el("button", {
          class: "secondary",
          "data-key": `show-${ws.id}-${copy.id}`,
          "aria-label": `Show the saved copy of ${copy.path} from ${at(copy.created_at)} in folder`,
          onclick: () => invoke("show_saved_copy", { id: ws.id, copy: copy.id })
            .catch((error) => { errors.set(ws.id, `${error}`); render(); }),
        }, "Show in folder")))));
  details.addEventListener("toggle", () => {
    if (details.open) copiesOpen.add(ws.id); else copiesOpen.delete(ws.id);
  });
  return details;
}

// The section nav: Activity and Conflicts as tabs (US00012 Q2). Left and right
// arrows move between them.
function sectionNav(ws) {
  const current = tabs.get(ws.id) || "activity";
  const tab = (key, label, extra = {}, ...children) => el("button", {
    role: "tab",
    id: `tab-${key}-${ws.id}`,
    "data-key": `tab-${key}-${ws.id}`,
    "aria-selected": String(current === key),
    "aria-controls": `panel-${ws.id}`,
    tabindex: current === key ? "0" : "-1",
    onclick: () => showTab(ws, key),
    onkeydown: (event) => {
      if (event.key === "ArrowRight" || event.key === "ArrowLeft") {
        event.preventDefault();
        showTab(ws, key === "activity" ? "conflicts" : "activity");
      }
    },
    ...extra,
  }, label, ...children);
  const nav = el("div", { class: "tabs", role: "tablist", "aria-label": "Sections" },
    tab("activity", "Activity"),
    tab("conflicts", "Conflicts", { "aria-label": ws.conflicts_tab },
      ws.conflict_count ? el("span", { class: "badge", "aria-hidden": "true" }, String(ws.conflict_count)) : null));
  const panel = el("div", {
    class: "panel",
    role: "tabpanel",
    id: `panel-${ws.id}`,
    "aria-labelledby": `tab-${current}-${ws.id}`,
  }, ...(current === "conflicts" ? conflictsPanel(ws) : activity(ws)));
  return [nav, panel];
}

// ---- Conflicts (US00006; design spec, *Conflicts*) -------------------------

// A time with the text that carries it: the host's text holds "{time}".
function stamp(stamped) {
  return stamped.time ? stamped.text.replace("{time}", at(stamped.time)) : stamped.text;
}

function when(seconds) {
  const today = new Date().toDateString() === new Date(seconds * 1000).toDateString();
  return today ? `today ${at(seconds)}` : at(seconds);
}

function fileIcon(image) {
  const root = svg("svg", { viewBox: "0 0 20 20", fill: "none", stroke: "currentColor", "stroke-width": "1.4", "stroke-linejoin": "round", "aria-hidden": "true" });
  if (image) {
    root.append(svg("rect", { x: "3", y: "4", width: "14", height: "12", rx: "1.5" }),
      svg("path", { d: "M3 14l4-4 3 3 3-3 4 4" }),
      svg("circle", { cx: "7", cy: "8", r: "1.2" }));
  } else {
    root.append(svg("path", { d: "M5 2h7l4 4v12H5zM12 2v4h4M8 10h5M8 13h5" }));
  }
  return root;
}

function noticeLine(ws) {
  const text = notices.get(ws.id);
  if (!text) return null;
  return el("div", { class: "notice", role: "status" },
    el("p", {}, text),
    el("button", {
      "data-key": `dismiss-${ws.id}`,
      "aria-label": "Dismiss this message",
      onclick: () => { notices.delete(ws.id); render(); },
    }, "×"));
}

function sideCard(side, other) {
  return el("div", { class: other ? "side-card other" : "side-card" },
    el("span", { class: "caps" }, side.title),
    el("span", { class: "who" }, side.author),
    side.saved ? el("span", { class: "when" }, `${side.verb} ${at(side.saved)}`) : null);
}

function conflictCard(ws, entry) {
  return el("li", { class: "conflict" },
    el("div", { class: "conflict-head" },
      fileIcon(entry.kind.startsWith("Image")),
      el("div", {},
        el("p", { class: "file" }, entry.path),
        el("p", { class: "kind" }, `${entry.kind} · recorded ${when(entry.recorded)}`))),
    el("div", { class: "sides" }, sideCard(entry.current, false), sideCard(entry.other, true)),
    el("button", {
      class: "secondary",
      "data-key": `review-${ws.id}-${entry.id}`,
      "aria-label": entry.review_label,
      onclick: () => { notices.delete(ws.id); openConflict(ws, entry.id); },
    }, "Review and choose"));
}

function conflictsPanel(ws) {
  const list = ws.conflicts;
  return [
    noticeLine(ws),
    el("h2", { class: "conflicts-heading" }, list.heading),
    el("p", { class: "conflicts-note" }, list.explanation),
    list.entries.length
      ? el("ul", { class: "conflict-list", "aria-label": list.heading }, ...list.entries.map((entry) => conflictCard(ws, entry)))
      : null,
  ];
}

async function openConflict(ws, id) {
  closeConflict(ws.id);
  const state = { id, view: null, error: null, busy: null, body: null, urls: [], closed: false };
  conflictOpen.set(ws.id, state);
  render();
  try {
    state.view = await invoke("conflict_detail", { id: ws.id, conflict: id });
  } catch (error) {
    if (state.closed) return;
    state.error = `${error}`;
    render();
    focusKey("conflict-back");
    return;
  }
  if (state.closed) return;
  render();
  focusKey("conflict-back");
  loadPreviews(ws.id, state);
}

function closeConflict(wsId) {
  const state = conflictOpen.get(wsId);
  if (!state) return;
  state.closed = true;
  for (const url of state.urls) URL.revokeObjectURL(url);
  conflictOpen.delete(wsId);
}

function focusKey(key) {
  const node = app.querySelector(`[data-key="${key}"]`);
  if (node) node.focus();
}

function backToList(ws) {
  const id = conflictOpen.get(ws.id)?.id;
  closeConflict(ws.id);
  tabs.set(ws.id, "conflicts");
  render();
  focusKey(`review-${ws.id}-${id}`);
}

// Image previews arrive as raw bytes by conflict id and side, never a path.
async function loadPreviews(wsId, state) {
  for (const side of ["current", "other"]) {
    const panel = state.view[side];
    if (!panel.preview) continue;
    state.body = state.body || conflictBody(state);
    const frame = state.body.querySelector(`[data-preview="${side}"]`);
    if (!frame) continue;
    try {
      const data = await invoke("conflict_preview", { id: wsId, conflict: state.id, side });
      if (state.closed) return;
      const url = URL.createObjectURL(new Blob([data], { type: panel.preview.mime }));
      state.urls.push(url);
      const image = frame.querySelector("img");
      if (image) {
        image.addEventListener("load", () => frame.querySelector("p")?.remove());
        image.src = url;
      }
    } catch (error) {
      if (state.closed) return;
      const note = frame.querySelector("p");
      if (note) note.textContent = `Couldn't load the preview: ${error}`;
    }
  }
}

async function choose(ws, state, choice) {
  state.busy = choice;
  state.error = null;
  render();
  try {
    const done = await invoke("resolve_conflict", { id: ws.id, conflict: state.id, choice });
    if (state.closed) return;
    notices.set(ws.id, done.confirmation);
    if (done.next) {
      await openConflict(ws, done.next);
    } else {
      closeConflict(ws.id);
      tabs.set(ws.id, "conflicts");
      render();
      focusKey(`tab-conflicts-${ws.id}`);
    }
  } catch (error) {
    if (state.closed) return;
    state.busy = null;
    state.error = `${error}`;
    render();
    // The buttons follow the latest state: a later save shows its note.
    refreshNote(ws.id, state);
  }
}

// Read the open conflict again for a save made after it was recorded, so the
// note and the buttons follow the folder. Only the note changes; the versions
// shown stay as they were loaded.
async function refreshNote(wsId, state) {
  if (state.closed || state.busy !== null || !state.view || state.refreshing) return;
  state.refreshing = true;
  try {
    const fresh = await invoke("conflict_detail", { id: wsId, conflict: state.id });
    const before = state.view.note ? state.view.note.time : null;
    const after = fresh.note ? fresh.note.time : null;
    if (!state.closed && state.busy === null && before !== after) {
      state.view.note = fresh.note;
      state.view.can_choose = fresh.can_choose;
      render();
    }
  } catch (error) {
    // A conflict that is gone from the list is handled by render().
  }
  state.refreshing = false;
}

function codeColumn(panel, lines) {
  return el("div", { class: "code", tabindex: "0", role: "region", "aria-label": `${panel.title}, ${lines.length} lines` },
    ...lines.map((line) => el("div", { class: line.changed ? "line changed" : "line" },
      el("span", { class: "num", "aria-hidden": "true" }, String(line.number)),
      line.changed
        ? el("span", { class: "mark", role: "img", "aria-label": panel.differs_label }, "≠")
        : el("span", { class: "mark" }),
      el("span", {}, line.text))));
}

function versionColumn(view, side, other) {
  const panel = view[side];
  const body = view.body;
  let content;
  if (panel.placeholder) {
    content = el("p", { class: "placeholder-box" }, stamp(panel.placeholder));
  } else if (body.kind === "text") {
    content = codeColumn(panel, body[side]);
  } else {
    content = [
      el("div", { class: "preview" },
        el("div", { class: "frame", "data-preview": side },
          panel.preview
            ? el("img", { alt: `Preview of the ${panel.title.toLowerCase()} of ${view.path}` })
            : null,
          el("p", {}, panel.preview ? "Loading preview…" : (panel.no_preview || "")))),
      el("dl", { class: "facts" },
        ...panel.facts.flatMap((fact) => [el("dt", {}, fact.label), el("dd", {}, stamp(fact.value))])),
    ];
  }
  return el("section", { class: other ? "version other" : "version", "aria-label": panel.title },
    el("div", { class: "version-head" },
      el("div", { class: "version-title" },
        el("span", { class: "caps" }, panel.title),
        el("span", { class: "tag" }, panel.badge)),
      el("p", { class: "byline" },
        panel.avatar ? el("span", { class: `avatar ${panel.avatar.color}`, "aria-hidden": "true" }, panel.avatar.initials) : null,
        el("span", {}, stamp(panel.byline))),
      el("p", { class: "purpose-line" }, panel.purpose)),
    ...(Array.isArray(content) ? content : [content]));
}

// The comparison itself, built once per opened conflict: a re-render for a new
// snapshot reuses it, so long files and loaded previews are not rebuilt.
function conflictBody(state) {
  const view = state.view;
  let strip = null;
  let toggle = null;
  if (view.base) {
    strip = el("div", { class: "base-strip", id: "base-strip", hidden: "" },
      el("div", { class: "head" },
        el("span", { class: "caps" }, view.base.label),
        el("span", {}, stamp(view.base.text))),
      view.base.lines.length
        ? el("div", { class: "base-text", tabindex: "0", role: "region", "aria-label": view.base.label },
          ...view.base.lines.map((line) => el("div", {}, line.text || " ")))
        : null);
    toggle = el("button", {
      class: "secondary",
      "data-key": "toggle-base",
      "aria-expanded": "false",
      "aria-controls": "base-strip",
    }, "Show common base");
    toggle.addEventListener("click", () => {
      const show = strip.hasAttribute("hidden");
      strip.toggleAttribute("hidden", !show);
      toggle.setAttribute("aria-expanded", String(show));
      toggle.textContent = show ? "Hide common base" : "Show common base";
    });
  }
  return el("div", { class: "detail-body" },
    el("div", { class: "detail-head" },
      el("div", {},
        el("h1", { class: "detail-path" }, view.path),
        el("p", { class: "detail-summary" }, view.summary)),
      toggle),
    strip,
    view.body.kind === "metadata" ? el("p", { class: "detail-summary" }, view.body.reason) : null,
    el("div", { class: "columns" }, versionColumn(view, "current", false), versionColumn(view, "other", true)));
}

function conflictDetail(ws, state) {
  const view = state.view;
  const top = el("div", { class: "top" },
    el("button", { class: "crumb", "data-key": "conflict-back", onclick: () => backToList(ws) },
      el("span", { "aria-hidden": "true" }, "‹"), "Conflicts"),
    view ? el("span", { class: "crumb of" }, `/ ${view.position}`) : null,
    el("span", { class: "workspace ellipsis" }, ws.name));
  if (!view) {
    return [el("div", { class: "detail" }, top,
      el("div", { class: "content" },
        state.error
          ? el("p", { class: "error", role: "alert" }, state.error)
          : el("p", { class: "muted" }, "Loading the conflict…")))];
  }
  if (!state.body) state.body = conflictBody(state);
  const busy = state.busy !== null;
  const button = (key, choice, label, primary) => el("button", {
    class: primary ? "primary" : "secondary",
    "data-key": key,
    ...(busy || !view.can_choose ? { disabled: "" } : {}),
    ...(state.busy === choice ? { "aria-busy": "true" } : {}),
    ...(view.note ? { "aria-describedby": "later-edit" } : {}),
    onclick: () => choose(ws, state, choice),
  }, state.busy === choice ? "Choosing…" : label);
  return [el("div", { class: "detail" }, top,
    el("div", { class: "content" },
      noticeLine(ws),
      state.body,
      view.note ? el("p", { class: "notice", id: "later-edit" }, stamp(view.note)) : null,
      state.error ? el("p", { class: "error", role: "alert" }, state.error) : null),
    el("div", { class: "footer" },
      el("p", { class: "reassure" }, view.footer),
      el("div", { class: "buttons" },
        button("use-other", "other", view.use_label, false),
        button("keep-current", "current", view.keep_label, true))))];
}

function topRow(several, ws) {
  if (!ws) {
    return el("div", { class: "top" },
      el("span", { class: "name" }, `All workspaces · ${workspaces.length}`));
  }
  // No app mark: the native title bar already shows it (design spec).
  return el("div", { class: "top" },
    several
      ? el("button", {
        class: "switcher",
        "data-key": "back",
        "aria-label": `${ws.name}. Show all workspaces`,
        onclick: () => { selected = null; render(); },
      }, el("span", { class: "name ellipsis" }, ws.name), el("span", { class: "chevron", "aria-hidden": "true" }, "▾"))
      : el("span", { class: "name ellipsis" }, ws.name));
}

function detail(ws, several) {
  const open = conflictOpen.get(ws.id);
  if (open) return conflictDetail(ws, open);
  const error = errors.get(ws.id);
  const running = !(ws.already_running || ws.retry);
  return [
    topRow(several, ws),
    el("div", { class: "content" },
      el("p", { class: "path ellipsis", title: ws.path }, ws.path),
      card(ws),
      error ? el("p", { class: "error", role: "alert" }, error) : null,
      ...(running ? sectionNav(ws) : [])),
  ];
}

function list() {
  return [
    topRow(true, null),
    el("div", { class: "content" },
      el("ul", { class: "list" }, ...workspaces.map((ws) =>
        el("li", {}, el("button", {
          class: `row ${ws.tone}`,
          "data-key": `row-${ws.id}`,
          onclick: () => { selected = ws.id; render(); },
        },
        statusGlyph(ws.glyph, ws.label, 22),
        el("span", { class: "name" }, ws.name),
        el("span", { class: "label" }, ws.reason && ws.tone === "attention" ? `${ws.label} · ${ws.reason}` : ws.label),
        el("span", { class: "path ellipsis", title: ws.path }, ws.path)))))),
  ];
}

function empty() {
  return [
    el("div", { class: "top" }, el("span", { class: "name" }, "Dispatch Mesh")),
    el("div", { class: "content" },
      el("h1", { class: "empty-title" }, "No workspace yet"),
      el("p", {}, "Set up a folder with the command line, then start the tray with it:"),
      el("p", {}, el("code", {}, 'dispatch-mesh --workspace "<folder>" init --name "<your name>" --email "<your email>"')),
      el("p", {}, el("code", {}, 'dispatch-mesh-tray --workspace "<folder>"')),
      el("p", { class: "muted" }, "Adding workspaces from this window comes with guided setup.")),
  ];
}

function announceChanges() {
  const text = workspaces
    .map((ws) => `${ws.name}: ${ws.label}${ws.reason ? `. ${ws.reason}` : ""}`)
    .join(". ");
  if (text !== lastAnnounced) {
    lastAnnounced = text;
    announce.textContent = text;
  }
}

// The workspace the window shows in detail, if any.
function shownWorkspace() {
  if (workspaces.length === 1) return workspaces[0];
  return workspaces.find((w) => w.id === selected) || null;
}

// A conflict that left the list while open was settled elsewhere.
function closeGoneConflicts() {
  for (const [wsId, state] of [...conflictOpen]) {
    const ws = workspaces.find((w) => w.id === wsId);
    if (!ws) {
      closeConflict(wsId);
    } else if (state.busy === null && !ws.conflicts.entries.some((entry) => entry.id === state.id)) {
      closeConflict(wsId);
      notices.set(wsId, ws.conflicts.resolved_elsewhere);
      tabs.set(wsId, "conflicts");
    }
  }
}

function render() {
  closeGoneConflicts();
  const several = workspaces.length > 1;
  const shown = shownWorkspace();
  let content;
  if (workspaces.length === 0) content = empty();
  else if (!several) content = detail(workspaces[0], false);
  else content = shown ? detail(shown, true) : list();
  // The window widens for conflict detail and returns to its compact size on Back.
  const open = shown ? conflictOpen.get(shown.id) : null;
  if (Boolean(open) !== wide) {
    wide = Boolean(open);
    invoke("set_layout", { layout: wide ? "detail" : "compact" }).catch(() => {});
  }
  const viewKey = `${shown ? shown.id : "-"}|${shown ? tabs.get(shown.id) : ""}|${open ? open.id + (open.view ? "+" : "") : ""}`;
  const scrollY = viewKey === lastView ? window.scrollY : 0;
  lastView = viewKey;
  const focusedId = document.activeElement && document.activeElement.dataset
    ? document.activeElement.dataset.key : undefined;
  app.replaceChildren(...content.filter(Boolean));
  window.scrollTo(0, scrollY);
  app.setAttribute("aria-busy", "false");
  if (focusedId) {
    const again = app.querySelector(`[data-key="${focusedId}"]`);
    if (again) again.focus();
  }
  announceChanges();
}

// Escape leaves the open conflict, as Back does, unless a choice is under way.
document.addEventListener("keydown", (event) => {
  if (event.key !== "Escape") return;
  const ws = shownWorkspace();
  const state = ws && conflictOpen.get(ws.id);
  if (state && state.busy === null) backToList(ws);
});

listen("workspaces", (event) => {
  workspaces = event.payload;
  render();
  for (const [wsId, state] of conflictOpen) refreshNote(wsId, state);
});
// Keep "Last synced 2 min ago" and similar times current while visible.
setInterval(() => { if (!document.hidden && workspaces.length) render(); }, 30000);

invoke("workspaces").then((initial) => {
  workspaces = initial;
  render();
});
