// Status window. Renders the host's view model; all wording comes from
// `model.rs`, so the tray and the window always agree. Data is only ever
// inserted as text, never as markup.
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

const SVG = "http://www.w3.org/2000/svg";

function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (key === "class") node.className = value;
    else if (key === "onclick") node.addEventListener("click", value);
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

// The tray mark and corner badge (design deliverable 1), so the window's state
// glyph matches the icon. The badge shape, not its color, carries the state.
const BADGE = {
  working: "M12 10a2 2 0 1 1-2 2",
  offline: "M10.5 13.5l3-3",
  attention: "M12 10v2.3M12 14.1v0.05",
};

function glyph(tone, size = 20) {
  const root = svg("svg", { width: size, height: size, viewBox: "0 0 16 16", "aria-hidden": "true" });
  const color = "var(--text)";
  const mark = svg("g", { fill: color, stroke: color, opacity: tone === "offline" || tone === "neutral" ? "0.5" : "1" });
  mark.append(svg("path", { d: "M4 4L12 4M4 4L4 12M12 4L4 12", "stroke-width": "1.4", fill: "none" }));
  for (const [cx, cy] of [[4, 4], [12, 4], [4, 12]]) {
    mark.append(svg("circle", { cx, cy, r: "2.2", stroke: "none" }));
  }
  root.append(mark);
  if (BADGE[tone]) {
    root.append(svg("circle", { cx: 12, cy: 12, r: "4.6", fill: "var(--surface)" }));
    root.append(svg("circle", { cx: 12, cy: 12, r: "3.6", fill: `var(--${tone})` }));
    root.append(svg("path", { d: BADGE[tone], fill: "none", stroke: "var(--glyph)", "stroke-width": "1.3", "stroke-linecap": "round" }));
  }
  return root;
}

function retryButton(ws) {
  return el("button", {
    class: "secondary",
    "data-key": `retry-${ws.id}`,
    onclick: () => invoke("retry", { id: ws.id }),
  }, "Retry");
}

function alreadyRunning(ws) {
  return el("section", { class: "card", "aria-labelledby": `state-${ws.id}` },
    el("div", { class: "state", id: `state-${ws.id}` }, glyph("neutral"), ws.label),
    el("p", { class: "reason" },
      `Another Dispatch Mesh — from the command line or a second tray app — is looking after ${ws.name}. ` +
      "Only one can at a time. Your files keep syncing through it."),
    retryButton(ws),
    el("p", { class: "muted" }, "Retry after closing the other one."));
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

function card(ws) {
  if (ws.already_running) return alreadyRunning(ws);
  return el("section", { class: `card ${ws.tone}`, "aria-labelledby": `state-${ws.id}` },
    el("div", { class: "state", id: `state-${ws.id}` }, glyph(ws.tone), ws.label),
    ws.reason ? el("p", { class: "reason" }, ws.reason) : null,
    ws.note ? el("p", { class: "reason" }, ws.note) : null,
    ws.action === "finish_session" ? finishButton(ws, true) : null,
    ws.retry ? retryButton(ws) : null);
}

// The stage track: each step says in words whether it is done, current or to
// come, and draws a different mark for each, so color is never the only cue.
const STEP = { done: ["✓", "done"], current: ["●", "current step"], todo: ["○", "not yet"] };

function track(session) {
  if (session.steps.length === 0) return null;
  return el("ol", { class: "track", "aria-label": "Progress" }, ...session.steps.map((step) =>
    el("li", { class: `step ${step.state}` },
      el("span", { class: "mark", "aria-hidden": "true" }, STEP[step.state][0]),
      step.label,
      el("span", { class: "visually-hidden" }, ` (${STEP[step.state][1]})`))));
}

function sessionItem(session) {
  const heading = session.title || session.files.join(", ") || "No files";
  return el("li", { class: "session" },
    el("p", { class: "session-title" }, heading),
    el("p", { class: "stage" }, session.stage),
    track(session),
    session.title ? el("p", { class: "files" }, session.files.join(", ")) : null,
    ...session.purposes.map((purpose) => el("p", { class: "purpose" }, `Requested purpose: ${purpose}`)),
    el("p", { class: "muted" },
      `${session.author} · started ${at(session.started_at)} · last saved ${ago(session.last_saved)}`));
}

function activity(ws) {
  return el("section", { class: "activity", "aria-labelledby": `activity-${ws.id}` },
    el("div", { class: "section-head" },
      el("h2", { id: `activity-${ws.id}` }, "Recent activity"),
      ws.open_session && ws.action !== "finish_session" ? finishButton(ws, false) : null),
    ws.sessions.length
      ? el("ol", { class: "sessions" }, ...ws.sessions.map(sessionItem))
      : el("p", { class: "muted" }, "No sessions yet. Edits you save here appear in this list."));
}

function savedCopies(ws) {
  if (ws.saved_copies.length === 0) return null;
  const details = el("details", { class: "copies", ...(copiesOpen.has(ws.id) ? { open: "" } : {}) },
    el("summary", { "data-key": `copies-${ws.id}` }, `Saved copies · ${ws.saved_copies.length}`),
    el("p", { class: "muted" },
      "Mesh kept these versions aside before writing incoming changes over them. To use one, copy it back yourself."),
    el("ul", { class: "list" }, ...ws.saved_copies.map((copy) =>
      el("li", { class: "copy" },
        el("span", { class: "path" }, copy.path),
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

function detail(ws, several) {
  const sync = syncLine(ws);
  const error = errors.get(ws.id);
  return [
    several ? el("button", { class: "link", "data-key": "back", onclick: () => { selected = null; render(); } }, "‹ All workspaces") : null,
    el("header", {}, el("h1", {}, ws.name), el("p", { class: "path" }, ws.path),
      sync ? el("p", { class: "sync" }, sync) : null),
    card(ws),
    error ? el("p", { class: "error", role: "alert" }, error) : null,
    ws.already_running || ws.retry ? null : activity(ws),
    savedCopies(ws),
  ];
}

function list() {
  return [
    el("h2", {}, `All workspaces · ${workspaces.length}`),
    el("ul", { class: "list" }, ...workspaces.map((ws) =>
      el("li", {}, el("button", {
        class: `row ${ws.tone}`,
        "data-key": `row-${ws.id}`,
        onclick: () => { selected = ws.id; render(); },
      },
      glyph(ws.tone),
      el("span", { class: "name" }, ws.name),
      el("span", { class: "label" }, ws.reason && ws.tone === "attention" ? `${ws.label} · ${ws.reason}` : ws.label),
      el("span", { class: "path" }, ws.path))))),
  ];
}

function empty() {
  return [
    el("h1", {}, "No workspace yet"),
    el("p", {}, "Set up a folder with the command line, then start the tray with it:"),
    el("p", {}, el("code", {}, 'dispatch-mesh --workspace "<folder>" init --name "<your name>" --email "<your email>"')),
    el("p", {}, el("code", {}, 'dispatch-mesh-tray --workspace "<folder>"')),
    el("p", { class: "muted" }, "Adding workspaces from this window comes with guided setup."),
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
