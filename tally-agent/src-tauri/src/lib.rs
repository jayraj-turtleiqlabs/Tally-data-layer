pub mod checkpoint;
pub mod cloud_client;
pub mod errors;
pub mod logging;
pub mod redact;
pub mod sync;
pub mod tally_client;
pub mod tally_envelope;
pub mod tally_schema;
pub mod vault;

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{Mutex, RwLock};
use tokio::time::{interval, Duration};

use crate::checkpoint::{Checkpoint, CheckpointStore};
use crate::cloud_client::{CloudClient, HeartbeatPayload};
use crate::errors::AgentError;
use crate::redact::redact;
use crate::sync::{SyncOrchestrator, SyncResult};
use crate::tally_client::{TallyClient, TallyEndpoint};
use crate::vault::Vault;

const HEARTBEAT_INTERVAL_SECS: u64 = 45;
const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize, Default)]
pub struct AgentStatus {
    pub paired: bool,
    pub company_name: Option<String>,
    pub tally_reachable: bool,
    pub sync_idle: bool,
    pub sync_in_progress: bool,
    pub last_successful_sync: Option<String>,
    pub last_error: Option<String>,
    pub last_known_alter_id: u64,
    pub backfill_complete: bool,
}

pub struct AgentState {
    pub tally_port: u16,
    pub status: RwLock<AgentStatus>,
    pub orchestrator: Mutex<Option<SyncOrchestrator>>,
    pub checkpoint_store: CheckpointStore,
}

impl AgentState {
    pub fn new(tally_port: u16) -> Result<Self, AgentError> {
        let endpoint = TallyEndpoint::new("127.0.0.1", tally_port)?;
        let tally = TallyClient::new(endpoint)?;
        let cloud = CloudClient::new()?;
        let checkpoint_store = CheckpointStore::new(CheckpointStore::default_path());
        let checkpoint = checkpoint_store.load()?;

        let status = AgentStatus {
            paired: Vault::is_paired(),
            tally_reachable: false,
            sync_idle: true,
            last_successful_sync: checkpoint.last_successful_sync.clone(),
            last_known_alter_id: checkpoint.last_known_alter_id,
            backfill_complete: checkpoint.backfill_complete,
            ..Default::default()
        };

        Ok(Self {
            tally_port,
            status: RwLock::new(status),
            orchestrator: Mutex::new(Some(SyncOrchestrator::new(
                tally,
                cloud,
                checkpoint_store.clone(),
            ))),
            checkpoint_store,
        })
    }

    pub async fn refresh_status(&self) {
        let paired = Vault::is_paired();
        let checkpoint = self.checkpoint_store.load().unwrap_or_default();
        let tally_reachable = self.check_tally_reachable().await;

        let mut status = self.status.write().await;
        status.paired = paired;
        status.tally_reachable = tally_reachable;
        status.last_known_alter_id = checkpoint.last_known_alter_id;
        status.backfill_complete = checkpoint.backfill_complete;
        status.last_successful_sync = checkpoint.last_successful_sync;
    }

    async fn check_tally_reachable(&self) -> bool {
        let endpoint = match TallyEndpoint::new("127.0.0.1", self.tally_port) {
            Ok(e) => e,
            Err(_) => return false,
        };
        let client = match TallyClient::new(endpoint) {
            Ok(c) => c,
            Err(_) => return false,
        };
        client.ping().await.is_ok()
    }

    pub async fn pair(&self, code: &str) -> Result<String, AgentError> {
        let endpoint = TallyEndpoint::new("127.0.0.1", self.tally_port)?;
        let tally = TallyClient::new(endpoint)?;
        let company = tally.ping().await?;

        let cloud = CloudClient::new()?;
        let response = cloud
            .pair(code, &company.company_name)
            .await
            .map_err(|_| AgentError::Api(crate::errors::ApiError::PairingFailed))?;

        Vault::store_token(&response.agent_token)?;

        {
            let mut status = self.status.write().await;
            status.paired = true;
            status.company_name = Some(response.company_name.clone());
            status.last_error = None;
        }

        // Rebuild orchestrator with fresh clients
        {
            let endpoint = TallyEndpoint::new("127.0.0.1", self.tally_port)?;
            let tally = TallyClient::new(endpoint)?;
            let cloud = CloudClient::new()?;
            let mut orch = self.orchestrator.lock().await;
            *orch = Some(SyncOrchestrator::new(
                tally,
                cloud,
                self.checkpoint_store.clone(),
            ));
        }

        // Automatic initial backfill after pairing
        self.run_backfill_internal().await?;

        Ok(response.company_name)
    }

    pub async fn disconnect(&self) -> Result<(), AgentError> {
        Vault::delete_token()?;
        let _ = self.checkpoint_store.save(&Checkpoint::default());

        let mut status = self.status.write().await;
        *status = AgentStatus::default();

        Ok(())
    }

    pub async fn sync_now(&self) -> Result<SyncResult, AgentError> {
        let orch_guard = self.orchestrator.lock().await;
        let orch = orch_guard
            .as_ref()
            .ok_or_else(|| AgentError::Other("Orchestrator not initialized".into()))?;

        {
            let mut status = self.status.write().await;
            status.sync_in_progress = true;
            status.sync_idle = false;
            status.last_error = None;
        }

        let checkpoint = match self.checkpoint_store.load() {
            Ok(cp) => cp,
            Err(e) => {
                let mut status = self.status.write().await;
                status.sync_in_progress = false;
                status.sync_idle = true;
                status.last_error = Some("Failed to load checkpoint store.".into());
                return Err(AgentError::Checkpoint(e));
            }
        };

        let result = if checkpoint.backfill_complete {
            orch.run_delta_sync().await
        } else {
            orch.run_initial_backfill().await
        };

        {
            let mut status = self.status.write().await;
            status.sync_in_progress = false;
            status.sync_idle = true;

            match &result {
                Ok(r) if r.success => {
                    status.last_successful_sync = self
                        .checkpoint_store
                        .load()
                        .ok()
                        .and_then(|c| c.last_successful_sync);
                    status.last_known_alter_id = r.new_alter_id.unwrap_or(status.last_known_alter_id);
                    status.backfill_complete = self
                        .checkpoint_store
                        .load()
                        .map(|c| c.backfill_complete)
                        .unwrap_or(false);
                    status.last_error = None;
                }
                Ok(r) => {
                    status.last_error = r.error_message.clone().or_else(|| Some("Sync failed. Please try again.".into()));
                }
                Err(e) => {
                    status.last_error = Some("Sync failed. Please try again.".into());
                    let _ = e;
                }
            }
        }

        result
    }

    async fn run_backfill_internal(&self) -> Result<SyncResult, AgentError> {
        let orch_guard = self.orchestrator.lock().await;
        let orch = orch_guard
            .as_ref()
            .ok_or_else(|| AgentError::Other("Orchestrator not initialized".into()))?;

        {
            let mut status = self.status.write().await;
            status.sync_in_progress = true;
        }

        let result = orch.run_initial_backfill().await;

        {
            let mut status = self.status.write().await;
            status.sync_in_progress = false;
            if let Ok(ref r) = result {
                if r.success {
                    status.backfill_complete = true;
                    status.last_successful_sync = self
                        .checkpoint_store
                        .load()
                        .ok()
                        .and_then(|c| c.last_successful_sync);
                }
            }
        }

        result
    }

    pub async fn get_status(&self) -> AgentStatus {
        self.refresh_status().await;
        self.status.read().await.clone()
    }
}

pub fn spawn_heartbeat_loop(state: Arc<AgentState>) {
    tauri::async_runtime::spawn(async move {
        let mut ticker = interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));

        loop {
            ticker.tick().await;

            if !Vault::is_paired() {
                continue;
            }

            let tally_reachable = state.check_tally_reachable().await;
            {
                let mut status = state.status.write().await;
                status.tally_reachable = tally_reachable;
            }

            let checkpoint = state.checkpoint_store.load().unwrap_or_default();
            let cloud = match CloudClient::new() {
                Ok(c) => c,
                Err(_) => continue,
            };

            let payload = HeartbeatPayload {
                tally_reachable,
                agent_version: AGENT_VERSION.to_string(),
                last_known_alter_id: checkpoint.last_known_alter_id,
            };

            if let Ok(resp) = cloud.heartbeat(&payload).await {
                // Dashboard-triggered sync: poll flag piggybacked on heartbeat
                if resp.sync_requested && !state.orchestrator.lock().await.as_ref().map(|o| o.is_sync_in_progress()).unwrap_or(false) {
                    log::info!("Dashboard requested sync via heartbeat poll flag");
                    match state.sync_now().await {
                        Ok(res) => {
                            if !res.success {
                                let err_msg = res.error_message.unwrap_or_else(|| "Sync failed".to_string());
                                log::warn!("Dashboard-triggered sync completed with failure: {}", redact(&err_msg));
                            } else {
                                log::info!(
                                    "Dashboard-triggered sync succeeded: {} records pushed",
                                    res.records_pushed
                                );
                            }
                        }
                        Err(e) => {
                            log::error!("Dashboard-triggered sync failed: {}", redact(&e.to_string()));
                        }
                    }
                }
            }
        }
    });
}
