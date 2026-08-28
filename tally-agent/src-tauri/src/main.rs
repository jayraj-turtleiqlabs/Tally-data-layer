// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use fininsight_tally_agent_lib::{spawn_heartbeat_loop, AgentState, AgentStatus};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, RunEvent, WindowEvent,
};

const DEFAULT_TALLY_PORT: u16 = 9000;

fn validate_startup_config(tally_port: u16) -> Result<(), String> {
    fininsight_tally_agent_lib::tally_client::TallyEndpoint::new("127.0.0.1", tally_port)
        .map_err(|e| format!("Refusing to start: {e}"))?;
    Ok(())
}

#[tauri::command]
async fn get_agent_status(state: tauri::State<'_, Arc<AgentState>>) -> Result<AgentStatus, String> {
    Ok(state.get_status().await)
}

#[tauri::command]
async fn pair_agent(
    code: String,
    state: tauri::State<'_, Arc<AgentState>>,
) -> Result<String, String> {
    state
        .pair(&code)
        .await
        .map_err(|e| {
            let msg = format!("{}", e);
            println!("[pair_agent] Error: {}", msg);
            msg
        })
}

#[tauri::command]
async fn sync_now(state: tauri::State<'_, Arc<AgentState>>) -> Result<(), String> {
    let result = state.sync_now().await.map_err(|e| e.to_string())?;
    if result.success {
        Ok(())
    } else {
        Err(result
            .error_message
            .unwrap_or_else(|| "Sync failed. Please try again.".into()))
    }
}

#[tauri::command]
async fn disconnect_agent(state: tauri::State<'_, Arc<AgentState>>) -> Result<(), String> {
    state.disconnect().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn show_window(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn main() {
    let tally_port = std::env::var("TALLY_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_TALLY_PORT);

    if let Err(e) = validate_startup_config(tally_port) {
        eprintln!("{e}");
        std::process::exit(1);
    }

    let agent_state = Arc::new(
        AgentState::new(tally_port).expect("Failed to initialize agent state"),
    );

    spawn_heartbeat_loop(agent_state.clone());

    tauri::Builder::default()
        .manage(agent_state)
        .invoke_handler(tauri::generate_handler![
            get_agent_status,
            pair_agent,
            sync_now,
            disconnect_agent,
            show_window,
        ])
        .setup(|app| {
            let _ = fininsight_tally_agent_lib::logging::init_logging();

            let show_i = MenuItem::with_id(app, "show", "Show FinInsight Agent", true, None::<&str>)?;
            let sync_i = MenuItem::with_id(app, "sync", "Sync Now", true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &sync_i, &quit_i])?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .tooltip("FinInsight Tally Agent")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "sync" => {
                        let state = app.state::<Arc<AgentState>>().inner().clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = state.sync_now().await;
                        });
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                })
                .build(app)?;

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if let RunEvent::WindowEvent {
                label,
                event: WindowEvent::CloseRequested { api, .. },
                ..
            } = event
            {
                if label == "main" {
                    api.prevent_close();
                    if let Some(w) = app_handle.get_webview_window("main") {
                        let _ = w.hide();
                    }
                }
            }
        });
}
