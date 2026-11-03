//! Automated Periodic Background Sync and Reminder Engine.
//! Checks sync intervals, executes scheduled company syncs, and triggers interactive
//! reminder dialogs if auto-sync fails or Tally is not running.

use std::sync::Arc;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::time::{interval, Duration};
use tauri::{Emitter, Manager};

use crate::AgentState;

const AUTO_SYNC_TICK_SECS: u64 = 15;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SyncReminderState {
    pub show_reminder: bool,
    pub reason: Option<String>,
    pub company_name: Option<String>,
    pub connection_id: Option<String>,
    pub last_attempt_at: Option<String>,
    pub last_successful_sync: Option<String>,
}

/// Spawns the background auto-sync worker loop.
pub fn spawn_auto_sync_loop(app_handle: tauri::AppHandle, state: Arc<AgentState>) {
    tauri::async_runtime::spawn(async move {
        let mut ticker = interval(Duration::from_secs(AUTO_SYNC_TICK_SECS));

        // Initial grace period on startup before the first auto-sync check (20s)
        tokio::time::sleep(Duration::from_secs(20)).await;

        loop {
            ticker.tick().await;

            let settings = match state.settings_store.load() {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("[auto_sync] Failed to load settings: {e}");
                    continue;
                }
            };

            if !settings.auto_sync_enabled || settings.auto_sync_interval_mins == 0 {
                continue;
            }

            let profiles = state.profile_store.load_all().unwrap_or_default();
            if profiles.is_empty() {
                continue;
            }

            let interval_duration = chrono::Duration::minutes(settings.auto_sync_interval_mins as i64);
            let now = Utc::now();

            for profile in &profiles {
                let cid = &profile.connection_id;
                let cp_store = state.checkpoint_store_for_connection(cid);
                let checkpoint = cp_store.load().unwrap_or_default();

                let is_due = match &checkpoint.last_successful_sync {
                    Some(last_str) => match DateTime::parse_from_rfc3339(last_str) {
                        Ok(last_dt) => (now - last_dt.with_timezone(&Utc)) >= interval_duration,
                        Err(_) => true,
                    },
                    None => true, // Never synced yet
                };

                if !is_due {
                    continue;
                }

                log::info!(
                    "[auto_sync] Auto-sync is due for company='{}' (interval: {}m)",
                    profile.company_name,
                    settings.auto_sync_interval_mins
                );

                // Step 1: Check if Tally is reachable
                let is_reachable = state.check_tally_reachable().await;
                if !is_reachable {
                    log::warn!(
                        "[auto_sync] Cannot auto-sync '{}': Tally is not running on port {}",
                        profile.company_name,
                        state.tally_port
                    );

                    let reminder = SyncReminderState {
                        show_reminder: true,
                        reason: Some("Tally is not open. Please start Tally and open your company.".to_string()),
                        company_name: Some(profile.company_name.clone()),
                        connection_id: Some(cid.clone()),
                        last_attempt_at: Some(now.to_rfc3339()),
                        last_successful_sync: checkpoint.last_successful_sync.clone(),
                    };

                    trigger_reminder(&app_handle, &state, reminder).await;
                    break;
                }

                // Step 2: Check if company is currently active in Tally
                let active_company = {
                    let active_guard = state.active_tally_company.read().await;
                    active_guard.clone()
                };

                let is_active = match active_company {
                    Some(ref active) => crate::matches_company(
                        profile.company_guid.as_deref(),
                        &profile.company_name,
                        active.company_guid.as_deref(),
                        &active.company_name,
                    ),
                    None => false,
                };

                if !is_active {
                    log::warn!(
                        "[auto_sync] Cannot auto-sync '{}': company is not currently loaded/active in Tally",
                        profile.company_name
                    );

                    let reminder = SyncReminderState {
                        show_reminder: true,
                        reason: Some(format!(
                            "Company '{}' is not open in Tally. Please open it in Tally to sync.",
                            profile.company_name
                        )),
                        company_name: Some(profile.company_name.clone()),
                        connection_id: Some(cid.clone()),
                        last_attempt_at: Some(now.to_rfc3339()),
                        last_successful_sync: checkpoint.last_successful_sync.clone(),
                    };

                    trigger_reminder(&app_handle, &state, reminder).await;
                    break;
                }

                // Step 3: Run the actual sync
                log::info!("[auto_sync] Starting background sync for connection_id='{}'...", cid);
                let sync_result = state.sync_company(cid).await;

                match sync_result {
                    Ok(res) if res.success => {
                        log::info!("[auto_sync] Successfully auto-synced company '{}'!", profile.company_name);
                        state.clear_sync_reminder().await;
                        let _ = app_handle.emit("sync-reminder-changed", ());
                    }
                    Ok(res) => {
                        let err_msg = res.error_message.unwrap_or_else(|| "Sync failed without specific error message".into());
                        log::error!("[auto_sync] Sync attempt reported failure: {}", err_msg);

                        let reminder = SyncReminderState {
                            show_reminder: true,
                            reason: Some(format!("Auto-sync failed: {err_msg}")),
                            company_name: Some(profile.company_name.clone()),
                            connection_id: Some(cid.clone()),
                            last_attempt_at: Some(now.to_rfc3339()),
                            last_successful_sync: checkpoint.last_successful_sync.clone(),
                        };

                        trigger_reminder(&app_handle, &state, reminder).await;
                        break;
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        log::error!("[auto_sync] Sync attempt failed with error: {}", err_msg);

                        let reminder = SyncReminderState {
                            show_reminder: true,
                            reason: Some(format!("Auto-sync failed: {err_msg}")),
                            company_name: Some(profile.company_name.clone()),
                            connection_id: Some(cid.clone()),
                            last_attempt_at: Some(now.to_rfc3339()),
                            last_successful_sync: checkpoint.last_successful_sync.clone(),
                        };

                        trigger_reminder(&app_handle, &state, reminder).await;
                        break;
                    }
                }
            }
        }
    });
}

async fn trigger_reminder(
    app_handle: &tauri::AppHandle,
    state: &AgentState,
    reminder: SyncReminderState,
) {
    let settings = state.settings_store.load().unwrap_or_default();

    // Store in runtime state
    state.set_sync_reminder(reminder.clone()).await;

    // Check snooze
    if state.settings_store.is_snoozed(&settings) {
        log::debug!("[auto_sync] Reminder is currently snoozed. Suppressing window popup.");
        let _ = app_handle.emit("sync-reminder-changed", ());
        return;
    }

    // Bring window to front so user sees the reminder
    if let Some(w) = app_handle.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }

    let _ = app_handle.emit("sync-reminder-changed", ());
}
