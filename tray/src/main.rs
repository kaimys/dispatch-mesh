#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! `dispatch-mesh-tray`: keeps one or more Mesh workspaces syncing from the
//! notification area (US00001; ADR-0011, ADR-0012). The CLI stays a separate
//! console program.

mod tray;

use clap::Parser;
use dispatch_mesh_tray::{
    host::Host,
    model::{self, Workspace},
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
        let _ = window.set_focus();
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
            show_saved_copy
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
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
