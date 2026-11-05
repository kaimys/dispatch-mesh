//! The notification-area icon, its tooltip and menu (US00001).

use dispatch_mesh_tray::model::{self, Icon, Workspace};
use std::{sync::Mutex, time::Duration};
use tauri::{
    AppHandle, Manager,
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

const TRAY: &str = "main";

/// Rendered by `scripts/tray_icons.py` from the Claude Design geometry.
macro_rules! sizes {
    ($state:literal, $theme:literal) => {
        [
            (
                16,
                include_bytes!(concat!("../icons/tray/", $state, "-", $theme, "-16.png"))
                    as &[u8],
            ),
            (
                20,
                include_bytes!(concat!("../icons/tray/", $state, "-", $theme, "-20.png"))
                    as &[u8],
            ),
            (
                24,
                include_bytes!(concat!("../icons/tray/", $state, "-", $theme, "-24.png"))
                    as &[u8],
            ),
            (
                32,
                include_bytes!(concat!("../icons/tray/", $state, "-", $theme, "-32.png"))
                    as &[u8],
            ),
        ]
    };
}

type Sizes = [(u32, &'static [u8]); 4];

static ICONS: [(&str, bool, Sizes); 8] = [
    ("synced", false, sizes!("synced", "light")),
    ("synced", true, sizes!("synced", "dark")),
    ("working", false, sizes!("working", "light")),
    ("working", true, sizes!("working", "dark")),
    ("offline", false, sizes!("offline", "light")),
    ("offline", true, sizes!("offline", "dark")),
    ("attention", false, sizes!("attention", "light")),
    ("attention", true, sizes!("attention", "dark")),
];

/// What the icon currently shows, so unchanged updates cost nothing.
#[derive(Clone, Copy, PartialEq)]
struct Look {
    icon: Icon,
    dark_taskbar: bool,
    px: u32,
}

#[derive(Default)]
struct TrayState {
    look: Option<Look>,
    hint: bool,
    tooltip: String,
    icon: Option<Icon>,
}

fn image(look: Look) -> tauri::Result<Image<'static>> {
    let (_, _, sizes) = ICONS
        .iter()
        .find(|(stem, dark, _)| *stem == look.icon.file_stem() && *dark == look.dark_taskbar)
        .expect("every icon is in the table");
    let bytes = sizes
        .iter()
        .find(|(px, _)| *px == look.px)
        .map_or(sizes[0].1, |(_, bytes)| bytes);
    Image::from_bytes(bytes).map(|image| image.to_owned())
}

/// The notification area is 16 px at 100 % scaling; pick the matching render.
fn icon_px(app: &AppHandle) -> u32 {
    let scale = app
        .primary_monitor()
        .ok()
        .flatten()
        .map_or(1.0, |monitor| monitor.scale_factor());
    match scale {
        s if s < 1.125 => 16,
        s if s < 1.375 => 20,
        s if s < 1.75 => 24,
        _ => 32,
    }
}

/// The taskbar's own theme, which can differ from the apps' theme.
#[cfg(windows)]
fn dark_taskbar() -> bool {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value = wide("SystemUsesLightTheme");
    let mut data: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: both strings are NUL-terminated and `data` is a DWORD of `size` bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    status == 0 && data == 0
}

#[cfg(not(windows))]
fn dark_taskbar() -> bool {
    false
}

fn menu(app: &AppHandle, hint: bool) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, "open", "Open status window", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "quit", "Quit Dispatch Mesh", true, None::<&str>)?,
        ],
    )?;
    if hint {
        // Quit never prompts; this answers "will I lose anything?" beforehand.
        menu.append(&MenuItem::with_id(
            app,
            "quit-hint",
            "Your edits stay saved. The session continues next start.",
            false,
            None::<&str>,
        )?)?;
    }
    Ok(menu)
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    app.manage(Mutex::new(TrayState::default()));
    let look = Look {
        icon: Icon::Working,
        dark_taskbar: dark_taskbar(),
        px: icon_px(app),
    };
    TrayIconBuilder::with_id(TRAY)
        .icon(image(look)?)
        .tooltip("Dispatch Mesh — Starting")
        .menu(&menu(app, false)?)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => crate::show_window(app),
            "quit" => crate::quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                crate::show_window(tray.app_handle());
            }
        })
        .build(app)?;
    let state = app.state::<Mutex<TrayState>>();
    state.lock().unwrap().look = Some(look);
    Ok(())
}

/// Apply the workspaces' states to the icon, tooltip and menu.
pub fn update(app: &AppHandle, workspaces: &[Workspace]) {
    let state = app.state::<Mutex<TrayState>>();
    let mut state = state.lock().unwrap();
    state.icon = Some(model::aggregate(workspaces));
    state.tooltip = model::tooltip(workspaces);
    let hint = model::any_open_session(workspaces);
    let Some(tray) = app.tray_by_id(TRAY) else {
        return;
    };
    let _ = tray.set_tooltip(Some(&state.tooltip));
    if hint != state.hint
        && let Ok(menu) = menu(app, hint)
        && tray.set_menu(Some(menu)).is_ok()
    {
        state.hint = hint;
    }
    refresh_icon(app, &mut state);
}

fn refresh_icon(app: &AppHandle, state: &mut TrayState) {
    let Some(icon) = state.icon else { return };
    let look = Look {
        icon,
        dark_taskbar: dark_taskbar(),
        px: icon_px(app),
    };
    if state.look == Some(look) {
        return;
    }
    if let (Some(tray), Ok(image)) = (app.tray_by_id(TRAY), image(look))
        && tray.set_icon(Some(image)).is_ok()
    {
        state.look = Some(look);
    }
}

/// Follow taskbar theme and scaling changes. A registry read every few
/// seconds is cheap, and Windows offers no event for a hidden-window app.
pub fn watch_theme(app: &AppHandle) {
    let app = app.clone();
    std::thread::Builder::new()
        .name("mesh-tray-theme".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(5));
                let state = app.state::<Mutex<TrayState>>();
                refresh_icon(&app, &mut state.lock().unwrap());
            }
        })
        .expect("theme watcher thread");
}

pub fn hide(app: &AppHandle) {
    if let Some(tray) = app.tray_by_id(TRAY) {
        let _ = tray.set_visible(false);
    }
}
