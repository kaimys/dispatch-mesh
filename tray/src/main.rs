#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! `dispatch-mesh-tray`: keeps one or more Mesh workspaces syncing from the
//! notification area (US00001; ADR-0011, ADR-0012). The CLI stays a separate
//! console program.

mod tray;

use clap::Parser;
use dispatch_mesh_tray::{
    host::Host,
    model::{
        self, Workspace,
        conflicts::{Choice, ConflictDetailView, Resolved},
    },
};
use std::{path::PathBuf, sync::Arc};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WindowEvent};
use tauri_plugin_notification::NotificationExt;

#[derive(Parser)]
#[command(
    version,
    about = "Dispatch Mesh tray: keeps your workspaces syncing from the notification area"
)]
struct Args {
    /// A workspace folder to run. Repeat for several.
    #[arg(long = "workspace", value_name = "FOLDER")]
    workspaces: Vec<PathBuf>,
}

struct Tray {
    host: Arc<Host>,
}

#[tauri::command]
fn workspaces(state: State<Tray>) -> Vec<model::WorkspaceView> {
    model::view(&state.host.workspaces())
}

#[tauri::command]
fn retry(state: State<Tray>, id: usize) {
    // Starting takes the runner lock and sets up the watcher; keep it off the UI thread.
    let host = Arc::clone(&state.host);
    std::thread::spawn(move || host.retry(id));
}

/// Close the workspace's open session (US00005). Waits for the engine's
/// current operation on a worker thread; the window learns the outcome from
/// the next snapshot, or from the error.
#[tauri::command]
async fn finish_session(state: State<'_, Tray>, id: usize) -> Result<(), String> {
    let host = Arc::clone(&state.host);
    tauri::async_runtime::spawn_blocking(move || host.finish(id))
        .await
        .map_err(|error| error.to_string())?
        .map(|_| ())
        .map_err(|error| format!("{error:#}"))
}

/// One unresolved conflict for the detail view (US00006). Reads committed
/// state, so it never waits for the engine; still off the UI thread, since it
/// runs Git and reads the versions.
#[tauri::command]
async fn conflict_detail(
    state: State<'_, Tray>,
    id: usize,
    conflict: String,
) -> Result<ConflictDetailView, String> {
    let host = Arc::clone(&state.host);
    tauri::async_runtime::spawn_blocking(move || host.conflict_detail(id, &conflict))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("{error:#}"))
}

/// The bytes of one side of a conflict, only for an image the window may
/// preview. They travel raw (an `ArrayBuffer` in the page), keyed by ids; the
/// page never names a path.
#[tauri::command]
async fn conflict_preview(
    state: State<'_, Tray>,
    id: usize,
    conflict: String,
    side: String,
) -> Result<tauri::ipc::Response, String> {
    let host = Arc::clone(&state.host);
    tauri::async_runtime::spawn_blocking(move || host.conflict_preview(id, &conflict, &side))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("{error:#}"))?
        .map(|(_, bytes)| tauri::ipc::Response::new(bytes))
        .ok_or_else(|| "There is no preview for this version.".to_owned())
}

/// Keep the current version or use the other one, like `dispatch-mesh
/// conflicts resolve`. Waits for the engine's current operation on a worker
/// thread. The engine's refusal comes back as the error.
#[tauri::command]
async fn resolve_conflict(
    state: State<'_, Tray>,
    id: usize,
    conflict: String,
    choice: String,
) -> Result<Resolved, String> {
    let choice = Choice::parse(&choice).ok_or("Unknown choice.")?;
    let host = Arc::clone(&state.host);
    tauri::async_runtime::spawn_blocking(move || host.resolve_conflict(id, &conflict, choice))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("{error:#}"))
}

/// The size the window had before conflict detail widened it, to restore on
/// Back.
#[derive(Default)]
struct Compact(std::sync::Mutex<Option<tauri::PhysicalSize<u32>>>);

/// The detail layout's inner size in logical pixels (US00006, design spec).
const DETAIL_SIZE: (f64, f64) = (1000.0, 760.0);

/// Widen the window for conflict detail, or restore its compact size. A
/// maximized window is left alone either way (see [`show_window`]); a widened
/// one is kept inside the work area like the window at launch.
#[tauri::command]
fn set_layout(window: tauri::WebviewWindow, memory: State<Compact>, layout: String) {
    if window.is_maximized().unwrap_or(true) {
        return;
    }
    let mut remembered = memory.0.lock().unwrap_or_else(|e| e.into_inner());
    match layout.as_str() {
        "detail" => {
            let (Ok(Some(monitor)), Ok(position), Ok(outer), Ok(inner), Ok(scale)) = (
                window.current_monitor(),
                window.outer_position(),
                window.outer_size(),
                window.inner_size(),
                window.scale_factor(),
            ) else {
                return;
            };
            remembered.get_or_insert(inner);
            let area = monitor.work_area();
            let fit = model::widen(
                (inner.width, inner.height),
                (outer.width - inner.width, outer.height - inner.height),
                (position.x, position.y),
                (
                    (DETAIL_SIZE.0 * scale).round() as u32,
                    (DETAIL_SIZE.1 * scale).round() as u32,
                ),
                (area.position.x, area.position.y),
                (area.size.width, area.size.height),
            );
            let _ = window.set_size(tauri::PhysicalSize::new(fit.inner.0, fit.inner.1));
            let _ =
                window.set_position(tauri::PhysicalPosition::new(fit.position.0, fit.position.1));
        }
        _ => {
            if let Some(size) = remembered.take() {
                let _ = window.set_size(size);
            }
        }
    }
}

/// Open Explorer with a saved copy selected. The window names the copy by
/// id; the host resolves it, so no path ever comes from the page.
#[tauri::command]
fn show_saved_copy(state: State<Tray>, id: usize, copy: String) -> Result<(), String> {
    let location = state
        .host
        .saved_copy_location(id, &copy)
        .ok_or("This saved copy is no longer listed.")?;
    tauri_plugin_opener::reveal_item_in_dir(location).map_err(|error| error.to_string())
}

/// The tray menu's Finish-session item for workspace `id`.
pub fn finish_from_menu(app: &AppHandle, id: usize) {
    if let Some(state) = app.try_state::<Tray>() {
        let host = Arc::clone(&state.host);
        // Failures need no dialog: the session stays open and the window shows it.
        std::thread::spawn(move || {
            let _ = host.finish(id);
        });
    }
}

fn on_change(app: &AppHandle, workspaces: &[Workspace]) {
    tray::update(app, workspaces);
    let _ = app.emit("workspaces", model::view(workspaces));
}

pub fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        // Fit only restored geometry: a maximized window's outer rectangle
        // includes Windows' invisible borders and always looks too large,
        // and resizing or moving it would undo the maximize (review re-check
        // F1). The fit runs after `unminimize`, because a minimized window
        // reports a parking position far off screen.
        if !window.is_maximized().unwrap_or(true) {
            fit_to_work_area(&window);
        }
        let _ = window.set_focus();
    }
}

/// The window opens at 440 × 640 (US00012 Q7). On a screen shorter than
/// that, or where it would run past the work area's bottom, it is shortened
/// and moved up so it stays whole (see [`model::fit_vertically`]).
fn fit_to_work_area(window: &tauri::WebviewWindow) {
    let (Ok(Some(monitor)), Ok(position), Ok(outer), Ok(inner)) = (
        window.current_monitor(),
        window.outer_position(),
        window.outer_size(),
        window.inner_size(),
    ) else {
        return;
    };
    let area = monitor.work_area();
    let fit = model::fit_vertically(
        position.y,
        outer.height,
        outer.height - inner.height,
        area.position.y,
        area.size.height,
    );
    if let Some(height) = fit.inner_height {
        // `set_size` sets the inner size; the title bar and frame stay.
        let _ = window.set_size(tauri::PhysicalSize::new(inner.width, height));
    }
    if let Some(y) = fit.y {
        let _ = window.set_position(tauri::PhysicalPosition::new(position.x, y));
    }
}

/// Remove the icon at once, then stop every engine after its final capture.
pub fn quit(app: &AppHandle) {
    tray::hide(app);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
    let app = app.clone();
    std::thread::spawn(move || {
        if let Some(state) = app.try_state::<Tray>() {
            state.host.quit();
        }
        app.exit(0);
    });
}

const SETTINGS: &str = "tray-settings.json";

/// The first time the window is closed, say once that Mesh keeps running.
fn first_close(app: &AppHandle) {
    let Ok(dir) = app.path().app_config_dir() else {
        return;
    };
    let file = dir.join(SETTINGS);
    let mut settings: serde_json::Value = std::fs::read(&file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if settings["first_close_seen"] == true {
        return;
    }
    let _ = app
        .notification()
        .builder()
        .title("Dispatch Mesh is still running")
        .body("Closing this window doesn't stop Mesh. It keeps syncing from the tray.")
        .show();
    settings["first_close_seen"] = true.into();
    let _ =
        std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&file, settings.to_string()));
}

fn main() {
    let args = Args::parse();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            workspaces,
            retry,
            finish_session,
            show_saved_copy,
            conflict_detail,
            conflict_preview,
            resolve_conflict,
            set_layout
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            app.manage(Compact::default());
            tray::create(&handle)?;
            let listener = handle.clone();
            let host = Host::new(args.workspaces.clone(), move |workspaces| {
                on_change(&listener, &workspaces)
            })?;
            app.manage(Tray {
                host: Arc::clone(&host),
            });
            std::thread::spawn(move || host.start_all());
            tray::watch_theme(&handle);
            show_window(&handle);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Closing only hides the window; Quit is the way to stop Mesh.
                api.prevent_close();
                let _ = window.hide();
                first_close(window.app_handle());
            }
        })
        .build(tauri::generate_context!())
        .expect("Dispatch Mesh tray failed to start");
    app.run(|_, event| {
        if let RunEvent::ExitRequested {
            api, code: None, ..
        } = event
        {
            // No window open is not a reason to stop syncing.
            api.prevent_exit();
        }
    });
}
