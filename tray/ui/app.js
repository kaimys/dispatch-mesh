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
        el("span", { class: "meta ellipsis" }, meta)),
      // A summary title wraps; a file list stays on one line.
      el("p", { class: session.line_is_title ? "line2 title" : "line2 ellipsis" }, session.line),
      session.line_is_title ? el("span", { class: "visually-hidden" }, session.files_text) : null,
      session.purpose ? el("p", { class: "purpose ellipsis" }, session.purpose) : null,
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
  }, ...(current === "conflicts"
    ? [el("p", { class: "placeholder" }, ws.conflicts_placeholder)]
    : activity(ws)));
  return [nav, panel];
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

function render() {
  const several = workspaces.length > 1;
  let content;
  if (workspaces.length === 0) content = empty();
  else if (!several) content = detail(workspaces[0], false);
  else {
    const ws = workspaces.find((w) => w.id === selected);
    content = ws ? detail(ws, true) : list();
  }
  const focusedId = document.activeElement && document.activeElement.dataset
    ? document.activeElement.dataset.key : undefined;
  app.replaceChildren(...content.filter(Boolean));
  app.setAttribute("aria-busy", "false");
  if (focusedId) {
    const again = app.querySelector(`[data-key="${focusedId}"]`);
    if (again) again.focus();
  }
  announceChanges();
}

listen("workspaces", (event) => {
  workspaces = event.payload;
  render();
});
// Keep "Last synced 2 min ago" and similar times current while visible.
setInterval(() => { if (!document.hidden && workspaces.length) render(); }, 30000);

invoke("workspaces").then((initial) => {
  workspaces = initial;
  render();
});
