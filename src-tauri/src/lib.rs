use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use peerfold_core::Node;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

struct Quit(AtomicBool);

#[tauri::command]
fn state(node: State<'_, Node>) -> peerfold_core::Snapshot {
    node.snapshot()
}

#[tauri::command]
fn set_name(name: String, node: State<'_, Node>) -> Result<(), String> {
    node.set_name(&name).map_err(|err| err.to_string())
}

#[tauri::command]
fn set_paused(paused: bool, node: State<'_, Node>) -> Result<(), String> {
    node.set_paused(paused).map_err(|err| err.to_string())
}

#[tauri::command]
fn trust(device_id: String, node: State<'_, Node>) -> Result<(), String> {
    node.trust(&device_id).map_err(|err| err.to_string())
}

#[tauri::command]
fn reject(device_id: String, node: State<'_, Node>) -> Result<(), String> {
    node.reject(&device_id).map_err(|err| err.to_string())
}

#[tauri::command]
fn forget(device_id: String, node: State<'_, Node>) -> Result<(), String> {
    node.forget(&device_id).map_err(|err| err.to_string())
}

#[tauri::command]
fn create_share(path: String, node: State<'_, Node>) -> Result<(), String> {
    node.create_share(std::path::Path::new(&path))
        .map(|_| ())
        .map_err(|err| err.to_string())
}

#[tauri::command]
fn attach_share(share_id: String, path: String, node: State<'_, Node>) -> Result<(), String> {
    node.attach_share(&share_id, std::path::Path::new(&path))
        .map(|_| ())
        .map_err(|err| err.to_string())
}

#[tauri::command]
fn remove_share(share_id: String, node: State<'_, Node>) -> Result<(), String> {
    node.remove_share(&share_id).map_err(|err| err.to_string())
}

#[tauri::command]
fn rescan(share_id: String, node: State<'_, Node>) -> Result<(), String> {
    node.rescan(&share_id).map_err(|err| err.to_string())
}

#[tauri::command]
fn versions(share_id: String, node: State<'_, Node>) -> Result<Vec<peerfold_core::VersionInfo>, String> {
    node.versions(&share_id).map_err(|err| err.to_string())
}

#[tauri::command]
fn restore(share_id: String, rel_path: String, hash: String, node: State<'_, Node>) -> Result<(), String> {
    node.restore(&share_id, &rel_path, &hash)
        .map_err(|err| err.to_string())
}

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let quitting = Arc::new(Quit(AtomicBool::new(false)));
    let quitting_setup = Arc::clone(&quitting);
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(quitting)
        .setup(move |app| {
            let data_dir = app.path().app_data_dir()?;
            let node = Node::start(data_dir)?;
            let handle = app.handle().clone();
            node.set_on_change(move || {
                let _ = handle.emit("state", ());
            });
            if let Some(window) = app.get_webview_window("main") {
                let quitting = Arc::clone(&quitting_setup);
                let hide = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        if !quitting.0.load(Ordering::Relaxed) {
                            api.prevent_close();
                            let _ = hide.hide();
                        }
                    }
                });
            }
            let show = tauri::menu::MenuItem::with_id(app, "show", "Открыть", true, None::<&str>)?;
            let quit = tauri::menu::MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
            let menu = tauri::menu::Menu::with_items(app, &[&show, &quit])?;
            let mut tray = tauri::tray::TrayIconBuilder::new()
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "show" => show_main(app),
                    "quit" => {
                        if let Some(flag) = app.try_state::<Arc<Quit>>() {
                            flag.0.store(true, Ordering::Relaxed);
                        }
                        app.exit(0);
                    }
                    _ => {}
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            let _ = tray.tooltip("PeerFold").build(app)?;
            app.manage(node);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            state,
            set_name,
            set_paused,
            trust,
            reject,
            forget,
            create_share,
            attach_share,
            remove_share,
            rescan,
            versions,
            restore
        ])
        .run(tauri::generate_context!())
        .expect("PeerFold не запустился");
}
