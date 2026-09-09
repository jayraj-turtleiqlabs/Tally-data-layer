// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use fininsight_tally_agent_lib::{
    spawn_heartbeat_loop, AgentState, AgentStatus, CompanyItem, DeviceAuthPublicSession,
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, RunEvent, WindowEvent,
};

static IS_QUITTING: AtomicBool = AtomicBool::new(false);

const DEFAULT_TALLY_PORT: u16 = 9000;

fn validate_startup_config(tally_port: u16) -> Result<(), String> {
    fininsight_tally_agent_lib::tally_client::TallyEndpoint::new("127.0.0.1", tally_port)
        .map_err(|e| format!("Refusing to start: {e}"))?;
    Ok(())
}

#[tauri::command]
async fn get_companies(state: tauri::State<'_, Arc<AgentState>>) -> Result<Vec<CompanyItem>, String> {
    Ok(state.get_companies_cached().await)
}

#[tauri::command]
async fn refresh_companies(state: tauri::State<'_, Arc<AgentState>>) -> Result<Vec<CompanyItem>, String> {
    state.discover_and_merge_companies().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn initiate_company_connection(
    company_name: String,
    company_guid: Option<String>,
    state: tauri::State<'_, Arc<AgentState>>,
) -> Result<DeviceAuthPublicSession, String> {
    state
        .start_company_device_login(&company_name, company_guid.as_deref())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn poll_company_connection(
    company_name: String,
    company_guid: Option<String>,
    state: tauri::State<'_, Arc<AgentState>>,
) -> Result<String, String> {
    state
        .poll_company_device_login(&company_name, company_guid.as_deref())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn sync_company(
    connection_id: String,
    state: tauri::State<'_, Arc<AgentState>>,
) -> Result<(), String> {
    let result = state.sync_company(&connection_id).await.map_err(|e| e.to_string())?;
    if result.success {
        Ok(())
    } else {
        Err(result.error_message.unwrap_or_else(|| "Sync failed. Please try again.".into()))
    }
}

#[tauri::command]
async fn disconnect_company(
    connection_id: String,
    state: tauri::State<'_, Arc<AgentState>>,
) -> Result<(), String> {
    state.disconnect_company(&connection_id).await.map_err(|e| e.to_string())
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
            log::error!("[pair_agent] Error: {}", msg);
            msg
        })
}

#[tauri::command]
async fn initiate_device_login(
    state: tauri::State<'_, Arc<AgentState>>,
) -> Result<DeviceAuthPublicSession, String> {
    state
        .start_device_login()
        .await
        .map_err(|e| {
            let msg = format!("{}", e);
            log::error!("[initiate_device_login] Error: {}", msg);
            msg
        })
}

#[tauri::command]
async fn poll_device_login(
    state: tauri::State<'_, Arc<AgentState>>,
) -> Result<String, String> {
    state
        .poll_device_login()
        .await
        .map_err(|e| {
            let msg = format!("{}", e);
            log::error!("[poll_device_login] Error: {}", msg);
            msg
        })
}

#[tauri::command]
async fn cancel_device_login(
    state: tauri::State<'_, Arc<AgentState>>,
) -> Result<(), String> {
    state.cancel_device_login().await;
    Ok(())
}

#[tauri::command]
async fn login_via_browser(
    state: tauri::State<'_, Arc<AgentState>>,
) -> Result<String, String> {
    state
        .login_via_browser()
        .await
        .map_err(|e| {
            let msg = format!("{}", e);
            log::error!("[login_via_browser] Error: {}", msg);
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
        let _ = window.unminimize();
        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn main() {
    let _ = fininsight_tally_agent_lib::logging::init_logging();

    let tally_port = std::env::var("TALLY_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_TALLY_PORT);

    log::info!(
        "FinInsight Tally Agent v{} started (Tally port: {})",
        env!("CARGO_PKG_VERSION"),
        tally_port
    );

    if let Err(e) = validate_startup_config(tally_port) {
        log::error!("{e}");
        std::process::exit(1);
    }

    let agent_state = Arc::new(
        AgentState::new(tally_port).expect("Failed to initialize agent state"),
    );

    spawn_heartbeat_loop(agent_state.clone());

    tauri::Builder::default()
        .manage(agent_state)
        .invoke_handler(tauri::generate_handler![
            get_companies,
            refresh_companies,
            initiate_company_connection,
            poll_company_connection,
            sync_company,
            disconnect_company,
            get_agent_status,
            pair_agent,
            initiate_device_login,
            poll_device_login,
            cancel_device_login,
            login_via_browser,
            sync_now,
            disconnect_agent,
            show_window,
        ])
        .setup(|app| {
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
                            let _ = w.unminimize();
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
                        IS_QUITTING.store(true, Ordering::SeqCst);
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| match event {
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                    | TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } => {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.unminimize();
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    _ => {}
                })
                .build(app)?;

            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if let RunEvent::WindowEvent {
                label,
                event: win_event,
                ..
            } = event
            {
                if label == "main" {
                    match win_event {
                        WindowEvent::CloseRequested { api, .. } => {
                            if !IS_QUITTING.load(Ordering::SeqCst) {
                                api.prevent_close();
                                if let Some(w) = app_handle.get_webview_window("main") {
                                    let _ = w.hide();
                                }
                            }
                        }
                        WindowEvent::Resized(_) => {
                            if let Some(w) = app_handle.get_webview_window("main") {
                                if let Ok(true) = w.is_minimized() {
                                    let _ = w.hide();
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        });
}
