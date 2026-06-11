use std::{
    borrow::Cow,
    env,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use arboard::{Clipboard, ImageData};
use base64::{engine::general_purpose::STANDARD, Engine};
use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba};
use serde::{Deserialize, Serialize};
use tauri::{
    menu::MenuBuilder,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State, WebviewWindow,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as AutostartExt};
use tauri_plugin_global_shortcut::{Builder as ShortcutBuilder, GlobalShortcutExt, ShortcutState};

const HISTORY_LIMIT: usize = 100;
const HISTORY_EVENT: &str = "clipboard-history-updated";
const MANAGER_SHOWN_EVENT: &str = "manager-shown";
const SETTINGS_OPENED_EVENT: &str = "settings-opened";
const DEFAULT_SHORTCUT: &str = "Ctrl+Shift+V";
const START_MINIMIZED_ARG: &str = "--hidden";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Clip {
    id: u64,
    clip_type: String,
    content: String,
    source: String,
    created_at: u64,
    pinned: bool,
    image: Option<String>,
}

struct ClipboardState {
    history: Arc<Mutex<Vec<Clip>>>,
    storage_path: PathBuf,
}

struct SettingsState {
    shortcut: Mutex<String>,
    storage_path: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AppSettings {
    shortcut: String,
}

fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn classify_text(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        "link".into()
    } else if trimmed.contains('\n')
        && (trimmed.contains("const ")
            || trimmed.contains("fn ")
            || trimmed.contains("function ")
            || trimmed.contains("class ")
            || trimmed.contains("=>")
            || trimmed.contains('{'))
    {
        "code".into()
    } else {
        "text".into()
    }
}

fn persist(path: &Path, history: &[Clip]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let json = serde_json::to_vec_pretty(history).map_err(|error| error.to_string())?;
    fs::write(path, json).map_err(|error| error.to_string())
}

fn load_history(path: &Path) -> Vec<Clip> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn load_settings(path: &Path) -> AppSettings {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| AppSettings {
            shortcut: DEFAULT_SHORTCUT.into(),
        })
}

fn show_manager(app: &AppHandle, open_settings: bool) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
        let _ = window.emit(
            if open_settings {
                SETTINGS_OPENED_EVENT
            } else {
                MANAGER_SHOWN_EVENT
            },
            (),
        );
    }
}

fn is_start_minimized_launch() -> bool {
    env::args().any(|arg| arg == START_MINIMIZED_ARG)
}

fn trim_history(history: &mut Vec<Clip>) {
    let mut unpinned_seen = 0;
    history.retain(|clip| {
        if clip.pinned {
            true
        } else {
            unpinned_seen += 1;
            unpinned_seen <= HISTORY_LIMIT
        }
    });
}

fn add_clip(app: &AppHandle, history: &Arc<Mutex<Vec<Clip>>>, storage_path: &Path, clip: Clip) {
    let snapshot = {
        let mut items = history.lock().expect("clipboard history lock poisoned");
        if items.first().is_some_and(|item| {
            item.clip_type == clip.clip_type
                && item.content == clip.content
                && item.image == clip.image
        }) {
            return;
        }
        let existing = items.iter().position(|item| {
            item.clip_type == clip.clip_type
                && item.content == clip.content
                && item.image == clip.image
        });
        let mut clip = clip;
        if let Some(index) = existing {
            clip.pinned = items[index].pinned;
            items.remove(index);
        }
        items.insert(0, clip);
        trim_history(&mut items);
        let _ = persist(storage_path, &items);
        items.clone()
    };
    let _ = app.emit(HISTORY_EVENT, snapshot);
}

fn encode_image(image: ImageData<'_>) -> Result<String, String> {
    let buffer = ImageBuffer::<Rgba<u8>, _>::from_raw(
        image.width as u32,
        image.height as u32,
        image.bytes.into_owned(),
    )
    .ok_or("Clipboard image has invalid RGBA data")?;
    let mut png = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(buffer)
        .write_to(&mut png, ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    Ok(format!(
        "data:image/png;base64,{}",
        STANDARD.encode(png.into_inner())
    ))
}

fn decode_image(data_url: &str) -> Result<ImageData<'static>, String> {
    let encoded = data_url
        .split_once(',')
        .map(|(_, value)| value)
        .ok_or("Invalid image data URL")?;
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|error| error.to_string())?;
    let image = image::load_from_memory(&bytes)
        .map_err(|error| error.to_string())?
        .into_rgba8();
    let (width, height) = image.dimensions();
    Ok(ImageData {
        width: width as usize,
        height: height as usize,
        bytes: Cow::Owned(image.into_raw()),
    })
}

fn start_clipboard_watcher(app: AppHandle, history: Arc<Mutex<Vec<Clip>>>, storage_path: PathBuf) {
    thread::spawn(move || {
        let Ok(mut clipboard) = Clipboard::new() else {
            return;
        };
        let mut last_seen = String::new();

        loop {
            if let Ok(text) = clipboard.get_text() {
                let signature = format!("text:{text}");
                if !text.trim().is_empty() && signature != last_seen {
                    last_seen = signature;
                    add_clip(
                        &app,
                        &history,
                        &storage_path,
                        Clip {
                            id: timestamp_ms(),
                            clip_type: classify_text(&text),
                            content: text,
                            source: "Clipboard hệ thống".into(),
                            created_at: timestamp_ms(),
                            pinned: false,
                            image: None,
                        },
                    );
                }
            } else if let Ok(image) = clipboard.get_image() {
                let signature = (
                    image.width,
                    image.height,
                    image.bytes.iter().take(4096).fold(0_u64, |hash, byte| {
                        hash.wrapping_mul(31).wrapping_add(*byte as u64)
                    }),
                );
                let signature = format!("image:{}:{}:{}", signature.0, signature.1, signature.2);
                if signature != last_seen {
                    last_seen = signature;
                    if let Ok(data_url) = encode_image(image) {
                        add_clip(
                            &app,
                            &history,
                            &storage_path,
                            Clip {
                                id: timestamp_ms(),
                                clip_type: "image".into(),
                                content: "Hình ảnh từ clipboard".into(),
                                source: "Clipboard hệ thống".into(),
                                created_at: timestamp_ms(),
                                pinned: false,
                                image: Some(data_url),
                            },
                        );
                    }
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
    });
}

#[tauri::command]
fn get_history(state: State<'_, ClipboardState>) -> Vec<Clip> {
    state
        .history
        .lock()
        .expect("clipboard history lock poisoned")
        .clone()
}

#[tauri::command]
fn copy_clip(id: u64, state: State<'_, ClipboardState>) -> Result<(), String> {
    let clip = state
        .history
        .lock()
        .map_err(|error| error.to_string())?
        .iter()
        .find(|clip| clip.id == id)
        .cloned()
        .ok_or("Không tìm thấy mục clipboard")?;
    let mut clipboard = Clipboard::new().map_err(|error| error.to_string())?;
    if let Some(image) = clip.image {
        clipboard
            .set_image(decode_image(&image)?)
            .map_err(|error| error.to_string())
    } else {
        clipboard
            .set_text(clip.content)
            .map_err(|error| error.to_string())
    }
}

fn update_history<F>(state: &ClipboardState, update: F) -> Result<Vec<Clip>, String>
where
    F: FnOnce(&mut Vec<Clip>),
{
    let mut history = state.history.lock().map_err(|error| error.to_string())?;
    update(&mut history);
    persist(&state.storage_path, &history)?;
    Ok(history.clone())
}

#[tauri::command]
fn toggle_pin(id: u64, state: State<'_, ClipboardState>) -> Result<Vec<Clip>, String> {
    update_history(&state, |history| {
        if let Some(clip) = history.iter_mut().find(|clip| clip.id == id) {
            clip.pinned = !clip.pinned;
        }
    })
}

#[tauri::command]
fn delete_clip(id: u64, state: State<'_, ClipboardState>) -> Result<Vec<Clip>, String> {
    update_history(&state, |history| history.retain(|clip| clip.id != id))
}

#[tauri::command]
fn clear_unpinned(state: State<'_, ClipboardState>) -> Result<Vec<Clip>, String> {
    update_history(&state, |history| history.retain(|clip| clip.pinned))
}

#[tauri::command]
fn hide_window(window: WebviewWindow) -> Result<(), String> {
    window.hide().map_err(|error| error.to_string())
}

#[tauri::command]
fn get_autostart_enabled(app: AppHandle) -> Result<bool, String> {
    app.autolaunch()
        .is_enabled()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn set_autostart_enabled(app: AppHandle, enabled: bool) -> Result<bool, String> {
    let autostart = app.autolaunch();
    if enabled {
        autostart.enable().map_err(|error| error.to_string())?;
    } else {
        autostart.disable().map_err(|error| error.to_string())?;
    }
    autostart.is_enabled().map_err(|error| error.to_string())
}

#[tauri::command]
fn get_shortcut(state: State<'_, SettingsState>) -> String {
    state
        .shortcut
        .lock()
        .expect("settings lock poisoned")
        .clone()
}

#[tauri::command]
fn set_shortcut(
    shortcut: String,
    app: AppHandle,
    state: State<'_, SettingsState>,
) -> Result<(), String> {
    let shortcut = shortcut.trim().to_string();
    if shortcut.is_empty() {
        return Err("Phím tắt không được để trống".into());
    }
    if !shortcut.contains('+') {
        return Err("Phím tắt phải gồm ít nhất một phím bổ trợ".into());
    }

    let mut current = state.shortcut.lock().map_err(|error| error.to_string())?;
    if *current == shortcut {
        return Ok(());
    }

    app.global_shortcut()
        .register(shortcut.as_str())
        .map_err(|error| format!("Không thể đăng ký phím tắt: {error}"))?;
    let _ = app.global_shortcut().unregister(current.as_str());

    let settings = AppSettings {
        shortcut: shortcut.clone(),
    };
    if let Some(parent) = state.storage_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(
        &state.storage_path,
        serde_json::to_vec_pretty(&settings).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    *current = shortcut;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![START_MINIMIZED_ARG]),
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            ShortcutBuilder::new()
                .with_handler(|app, _, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    if let Some(window) = app.get_webview_window("main") {
                        if window.is_visible().unwrap_or(false) {
                            let _ = window.hide();
                        } else {
                            show_manager(app, false);
                        }
                    }
                })
                .build(),
        )
        .setup(|app| {
            let app_data = app.path().app_data_dir()?;
            let storage_path = app_data.join("clipboard-history.json");
            let history = Arc::new(Mutex::new(load_history(&storage_path)));
            app.manage(ClipboardState {
                history: history.clone(),
                storage_path: storage_path.clone(),
            });
            start_clipboard_watcher(app.handle().clone(), history, storage_path);

            let settings_path = app_data.join("settings.json");
            let settings = load_settings(&settings_path);
            let shortcut = if app
                .global_shortcut()
                .register(settings.shortcut.as_str())
                .is_ok()
            {
                settings.shortcut
            } else {
                app.global_shortcut().register(DEFAULT_SHORTCUT)?;
                DEFAULT_SHORTCUT.into()
            };
            app.manage(SettingsState {
                shortcut: Mutex::new(shortcut),
                storage_path: settings_path,
            });

            let menu = MenuBuilder::new(app)
                .text("show", "Mở Clipboard")
                .text("settings", "Cài đặt phím tắt")
                .separator()
                .text("quit", "Thoát")
                .build()?;
            TrayIconBuilder::new()
                .icon(
                    app.default_window_icon()
                        .cloned()
                        .expect("application icon missing"),
                )
                .tooltip("Clipboard Manager")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => show_manager(app, false),
                    "settings" => show_manager(app, true),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if matches!(
                        event,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        }
                    ) {
                        show_manager(tray.app_handle(), false);
                    }
                })
                .build(app)?;
            if !is_start_minimized_launch() {
                show_manager(app.handle(), false);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_history,
            copy_clip,
            toggle_pin,
            delete_clip,
            clear_unpinned,
            hide_window,
            get_autostart_enabled,
            set_autostart_enabled,
            get_shortcut,
            set_shortcut
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
