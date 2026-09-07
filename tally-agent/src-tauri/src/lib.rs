pub mod checkpoint;
pub mod cloud_client;
pub mod device_auth;
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
use crate::device_auth::DeviceAuthSession;
use crate::errors::{AgentError, ApiError};
use crate::redact::redact;
use crate::sync::{SyncOrchestrator, SyncResult};
use crate::tally_client::{TallyClient, TallyEndpoint};
use crate::vault::Vault;

const HEARTBEAT_INTERVAL_SECS: u64 = 45;
const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize)]
pub struct DeviceAuthPublicSession {
    pub user_code: String,
    pub verification_uri: String,
    pub browser_opened: bool,
}

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
    pub cloud_base_url: Option<String>,
    pub status: RwLock<AgentStatus>,
    pub orchestrator: Mutex<Option<SyncOrchestrator>>,
    pub checkpoint_store: CheckpointStore,
    pub active_device_session: Mutex<Option<DeviceAuthSession>>,
}

impl AgentState {
    pub fn new(tally_port: u16) -> Result<Self, AgentError> {
        Self::with_options(tally_port, None, CheckpointStore::default_path())
    }

    pub fn with_options(
        tally_port: u16,
        cloud_base_url: Option<String>,
        checkpoint_path: std::path::PathBuf,
    ) -> Result<Self, AgentError> {
        let endpoint = TallyEndpoint::new("127.0.0.1", tally_port)?;
        let tally = TallyClient::new(endpoint)?;
        let mut cloud = match &cloud_base_url {
            Some(url) => CloudClient::from_base_url(url)?,
            None => CloudClient::new()?,
        };
        if let Ok(token) = Vault::get_token() {
            cloud = cloud.with_token(&token);
        }
        let checkpoint_store = CheckpointStore::new(checkpoint_path);
        let checkpoint = checkpoint_store.load().unwrap_or_default();

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
            cloud_base_url,
            status: RwLock::new(status),
            orchestrator: Mutex::new(Some(SyncOrchestrator::new(
                tally,
                cloud,
                checkpoint_store.clone(),
            ))),
            checkpoint_store,
            active_device_session: Mutex::new(None),
        })
    }

    pub fn new_cloud_client(&self) -> Result<CloudClient, ApiError> {
        match &self.cloud_base_url {
            Some(url) => CloudClient::from_base_url(url),
            None => CloudClient::new(),
        }
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

    pub async fn complete_pairing_with_token(
        &self,
        token: &str,
        company_name: &str,
    ) -> Result<String, AgentError> {
        log::debug!("[pair] Storing agent token in vault...");
        Vault::store_token(token)?;
        log::debug!("[pair] Token stored");

        // Reset checkpoint on fresh pairing so a fresh initial backfill is guaranteed
        let _ = self.checkpoint_store.save(&Checkpoint::default());

        {
            let mut status = self.status.write().await;
            status.paired = true;
            status.company_name = Some(company_name.to_string());
            status.last_error = None;
            status.backfill_complete = false;
        }

        // Rebuild orchestrator with fresh clients
        {
            let endpoint = TallyEndpoint::new("127.0.0.1", self.tally_port)?;
            let tally = TallyClient::new(endpoint)?;
            let cloud = self.new_cloud_client()?.with_token(token);
            let mut orch = self.orchestrator.lock().await;
            *orch = Some(
                SyncOrchestrator::new(tally, cloud, self.checkpoint_store.clone())
                    .with_expected_company(Some(company_name.to_string())),
            );
        }

        log::info!("[pair] Running initial backfill...");
        // Automatic initial backfill after pairing
        self.run_backfill_internal().await?;
        log::info!("[pair] Backfill complete");

        Ok(company_name.to_string())
    }

    pub async fn pair(&self, code: &str) -> Result<String, AgentError> {
        log::debug!("[pair] Step 1: Creating TallyEndpoint on 127.0.0.1:{}", self.tally_port);
        let endpoint = TallyEndpoint::new("127.0.0.1", self.tally_port)?;
        let tally = TallyClient::new(endpoint)?;

        log::debug!("[pair] Step 2: Pinging local Tally...");
        let company = match tally.ping().await {
            Ok(c) => {
                log::info!("[pair] Step 2 OK: Tally responded, company='{}'", c.company_name);
                c
            }
            Err(e) => {
                log::error!("[pair] Step 2 FAILED: Tally ping error: {:?}", e);
                return Err(e.into());
            }
        };

        log::debug!("[pair] Step 3: Creating CloudClient...");
        let cloud = self.new_cloud_client()?;

        log::debug!("[pair] Step 4: Sending pair request to cloud with code='{}'", &code[..code.len().min(4)]);
        let response = match cloud.pair(code, &company.company_name).await {
            Ok(r) => {
                log::info!("[pair] Step 4 OK: Pairing succeeded, got agent_token");
                r
            }
            Err(e) => {
                log::error!("[pair] Step 4 FAILED: Cloud pair error: {:?}", e);
                return Err(AgentError::Api(crate::errors::ApiError::PairingFailed));
            }
        };

        let token = response.token()?;
        self.complete_pairing_with_token(&token, &company.company_name).await
    }

    pub async fn start_device_login(&self) -> Result<DeviceAuthPublicSession, AgentError> {
        log::debug!("[device_login] Step 1: Checking Tally reachable...");
        let endpoint = TallyEndpoint::new("127.0.0.1", self.tally_port)?;
        let tally = TallyClient::new(endpoint)?;
        let _ = tally.ping().await?;

        log::debug!("[device_login] Step 2: Requesting device authorization session...");
        let cloud = self.new_cloud_client()?;
        let session = crate::device_auth::initiate_device_auth(&cloud).await?;

        log::info!("[device_login] Step 3: Opening system browser to {}", &session.verification_uri);
        let browser_opened = opener::open(&session.verification_uri).is_ok();
        if !browser_opened {
            log::warn!("System browser could not be launched automatically");
        }

        let public_info = DeviceAuthPublicSession {
            user_code: session.user_code.clone(),
            verification_uri: session.verification_uri.clone(),
            browser_opened,
        };

        {
            let mut active = self.active_device_session.lock().await;
            *active = Some(session);
        }

        Ok(public_info)
    }

    pub async fn poll_device_login(&self) -> Result<String, AgentError> {
        let session = {
            let active = self.active_device_session.lock().await;
            active.clone().ok_or_else(|| AgentError::Other("No active device authorization session".into()))?
        };

        let endpoint = TallyEndpoint::new("127.0.0.1", self.tally_port)?;
        let tally = TallyClient::new(endpoint)?;
        let company = tally.ping().await?;

        let cloud = self.new_cloud_client()?;
        log::debug!("[device_login] Step 4: Polling backend for user approval...");
        let status = crate::device_auth::poll_until_complete(&cloud, &session).await?;

        // Clear active session
        {
            let mut active = self.active_device_session.lock().await;
            *active = None;
        }

        match status {
            crate::device_auth::DeviceAuthStatus::Approved { agent_token, .. } => {
                log::info!("[device_login] Step 5: Device approved! Setting up connection...");
                self.complete_pairing_with_token(&agent_token, &company.company_name).await
            }
            crate::device_auth::DeviceAuthStatus::Denied => {
                log::warn!("[device_login] Device authorization was denied");
                Err(AgentError::Api(crate::errors::ApiError::DeviceAuthDenied))
            }
            crate::device_auth::DeviceAuthStatus::Expired => {
                log::warn!("[device_login] Device authorization expired");
                Err(AgentError::Api(crate::errors::ApiError::DeviceAuthExpired))
            }
            _ => Err(AgentError::Api(crate::errors::ApiError::InvalidResponse(
                "Device authorization concluded without approval".into(),
            ))),
        }
    }

    pub async fn cancel_device_login(&self) {
        let mut active = self.active_device_session.lock().await;
        *active = None;
    }

    pub async fn login_via_browser(&self) -> Result<String, AgentError> {
        let _ = self.start_device_login().await?;
        self.poll_device_login().await
    }

    pub async fn disconnect(&self) -> Result<(), AgentError> {
        Vault::delete_token()?;
        let _ = self.checkpoint_store.save(&Checkpoint::default());

        let mut status = self.status.write().await;
        *status = AgentStatus::default();

        Ok(())
    }

    pub async fn handle_token_revoked(&self) {
        log::warn!("Agent token has been revoked or expired. Clearing credentials and prompting re-pairing.");
        let _ = Vault::delete_token();
        let _ = self.checkpoint_store.save(&Checkpoint::default());

        let mut status = self.status.write().await;
        status.paired = false;
        status.company_name = None;
        status.last_error = Some("Agent token was revoked or expired. Please re-connect.".into());
        status.sync_in_progress = false;
        status.sync_idle = true;
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

        match &result {
            Err(AgentError::Api(ApiError::TokenRevoked)) => {
                self.handle_token_revoked().await;
                return Err(AgentError::Api(ApiError::TokenRevoked));
            }
            _ => {}
        }

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
                    status.last_error = r.error_message.clone().or_else(|| Some("Sync completed with issues.".into()));
                }
                Err(e) => {
                    let err_msg = match e {
                        AgentError::Tally(crate::errors::TallyError::ConnectionRefused(_)) => {
                            format!("Tally is not reachable on port {}. Please ensure Tally is running.", self.tally_port)
                        }
                        AgentError::Tally(crate::errors::TallyError::NoActiveCompany) => {
                            "No company is currently open in Tally. Please open your company in Tally.".into()
                        }
                        AgentError::Tally(crate::errors::TallyError::CompanyMismatch { current, expected }) => {
                            format!("Active company in Tally ('{}') does not match paired company ('{}').", current, expected)
                        }
                        AgentError::Api(ApiError::Network(net_err)) => {
                            format!("Network unreachable: {}. Please check your internet connection.", net_err)
                        }
                        AgentError::Api(ApiError::TokenRevoked) => {
                            "Agent authorization was revoked or expired. Please re-pair.".into()
                        }
                        other => format!("{}", other),
                    };
                    status.last_error = Some(err_msg);
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
            let token = match Vault::get_token() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let cloud = match state.new_cloud_client() {
                Ok(c) => c.with_token(&token),
                Err(_) => continue,
            };

            let payload = HeartbeatPayload {
                tally_reachable,
                agent_version: AGENT_VERSION.to_string(),
                last_known_alter_id: checkpoint.last_known_alter_id,
            };

            match cloud.heartbeat(&payload).await {
                Ok(resp) => {
                    // Dashboard-triggered sync: poll flag piggybacked on heartbeat
                    if resp.is_sync_requested() && !state.orchestrator.lock().await.as_ref().map(|o| o.is_sync_in_progress()).unwrap_or(false) {
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
                            Err(AgentError::Api(ApiError::TokenRevoked)) => {
                                log::warn!("Dashboard-triggered sync stopped due to revoked token");
                            }
                            Err(e) => {
                                log::error!("Dashboard-triggered sync failed: {}", redact(&e.to_string()));
                            }
                        }
                    }
                }
                Err(ApiError::TokenRevoked) => {
                    log::warn!("Heartbeat returned 401/403 (token revoked/expired). Resetting pairing.");
                    state.handle_token_revoked().await;
                }
                Err(e) => {
                    log::debug!("Heartbeat error: {:?}", e);
                }
            }
        }
    });
}
