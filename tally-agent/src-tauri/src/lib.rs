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

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tokio::time::{interval, Duration};

use crate::checkpoint::{Checkpoint, CheckpointStore, ConnectionProfile, ProfileStore};
use crate::cloud_client::{CloudClient, HeartbeatPayload};
use crate::device_auth::DeviceAuthSession;
use crate::errors::{AgentError, ApiError, TallyError};
use crate::sync::{SyncOrchestrator, SyncResult};
use crate::tally_client::{TallyClient, TallyEndpoint};
use crate::tally_schema::{CompanyInfo, DiscoveredCompany};
use crate::vault::Vault;

const HEARTBEAT_INTERVAL_SECS: u64 = 45;
const DISCOVERY_INTERVAL_SECS: u64 = 45;
const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceAuthPublicSession {
    pub user_code: String,
    pub verification_uri: String,
    pub browser_opened: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CompanyItem {
    pub company_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company_guid: Option<String>,
    pub alter_id: u64,
    pub is_active_in_tally: bool,
    pub is_connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_successful_sync: Option<String>,
    pub last_known_alter_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub sync_in_progress: bool,
    pub backfill_complete: bool,
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
    pub profile_store: ProfileStore,
    pub checkpoint_store: CheckpointStore,
    pub active_device_session: Mutex<Option<DeviceAuthSession>>,
    pub discovered_companies: RwLock<Vec<DiscoveredCompany>>,
    pub active_tally_company: RwLock<Option<CompanyInfo>>,
    pub per_company_errors: RwLock<HashMap<String, String>>,
    pub syncing_connections: RwLock<HashMap<String, bool>>,
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

        let base_dir = checkpoint_path.parent().unwrap_or_else(|| std::path::Path::new("."));
        let profile_path = if checkpoint_path.file_name().and_then(|n| n.to_str()) == Some("profiles.json") {
            checkpoint_path.clone()
        } else {
            base_dir.join("profiles.json")
        };
        let profile_store = ProfileStore::new(profile_path);
        let profiles = profile_store.load_all().unwrap_or_default();
        let first_profile = profiles.into_iter().next();

        let (connection_id, company_guid, company_name, paired) = if let Some(ref p) = first_profile {
            (
                Some(p.connection_id.clone()),
                p.company_guid.clone(),
                Some(p.company_name.clone()),
                true,
            )
        } else {
            (None, None, None, Vault::is_paired())
        };

        let checkpoint_store = if let Some(ref cid) = connection_id {
            let store = CheckpointStore::for_connection_in_dir(base_dir, cid);
            let _ = store.migrate_legacy_if_needed(cid);
            store
        } else {
            CheckpointStore::new(checkpoint_path)
        };

        let token = if let Some(ref cid) = connection_id {
            Vault::get_token_for(cid).or_else(|_| Vault::get_token()).ok()
        } else {
            Vault::get_token().ok()
        };

        if let Some(ref t) = token {
            cloud = cloud.with_token(t);
        }

        let checkpoint = checkpoint_store.load().unwrap_or_default();

        let status = AgentStatus {
            paired,
            company_name: company_name.clone(),
            tally_reachable: false,
            sync_idle: true,
            last_successful_sync: checkpoint.last_successful_sync.clone(),
            last_known_alter_id: checkpoint.last_known_alter_id,
            backfill_complete: checkpoint.backfill_complete,
            ..Default::default()
        };

        let orch = SyncOrchestrator::new(tally, cloud, checkpoint_store.clone())
            .with_expected_profile(company_name, company_guid, connection_id);

        Ok(Self {
            tally_port,
            cloud_base_url,
            status: RwLock::new(status),
            orchestrator: Mutex::new(Some(orch)),
            profile_store,
            checkpoint_store,
            active_device_session: Mutex::new(None),
            discovered_companies: RwLock::new(Vec::new()),
            active_tally_company: RwLock::new(None),
            per_company_errors: RwLock::new(HashMap::new()),
            syncing_connections: RwLock::new(HashMap::new()),
        })
    }

    pub fn new_tally_client(&self) -> Result<TallyClient, AgentError> {
        let endpoint = TallyEndpoint::new("127.0.0.1", self.tally_port)?;
        Ok(TallyClient::new(endpoint)?)
    }

    pub fn new_cloud_client(&self) -> Result<CloudClient, ApiError> {
        match &self.cloud_base_url {
            Some(url) => CloudClient::from_base_url(url),
            None => CloudClient::new(),
        }
    }

    pub async fn check_tally_reachable(&self) -> bool {
        if let Ok(client) = self.new_tally_client() {
            client.ping().await.is_ok()
        } else {
            false
        }
    }

    /// Discovers companies from Tally and merges with saved connection profiles.
    pub async fn discover_and_merge_companies(&self) -> Result<Vec<CompanyItem>, AgentError> {
        let tally_client = match self.new_tally_client() {
            Ok(c) => c,
            Err(e) => return Err(e),
        };

        // Query active company and discovery list
        let active_res = tally_client.ping().await;
        let discovery_res = tally_client.discover_companies().await;

        let is_tally_reachable = active_res.is_ok();
        let active_company = active_res.ok();
        let discovered_raw = discovery_res.unwrap_or_default();

        // Update runtime caches
        {
            let mut active_cache = self.active_tally_company.write().await;
            *active_cache = active_company.clone();
        }
        {
            let mut disc_cache = self.discovered_companies.write().await;
            *disc_cache = discovered_raw.clone();
        }

        let profiles = self.profile_store.load_all().unwrap_or_default();
        let errors = self.per_company_errors.read().await.clone();
        let syncing_map = self.syncing_connections.read().await.clone();

        let mut items: Vec<CompanyItem> = Vec::new();
        let mut processed_names: std::collections::HashSet<String> = std::collections::HashSet::new();

        // 1. Process all saved connected profiles
        for profile in &profiles {
            let cid = &profile.connection_id;
            let cp_store = CheckpointStore::for_connection(cid);
            let checkpoint = cp_store.load().unwrap_or_default();

            // Match against active Tally company
            let is_active = if let Some(ref active) = active_company {
                if let Some(ref p_guid) = profile.company_guid {
                    if let Some(ref a_guid) = active.company_guid {
                        p_guid.eq_ignore_ascii_case(a_guid)
                    } else {
                        profile.company_name.eq_ignore_ascii_case(&active.company_name)
                    }
                } else {
                    profile.company_name.eq_ignore_ascii_case(&active.company_name)
                }
            } else {
                false
            };

            // Check if present in discovery list
            let in_discovery = discovered_raw.iter().any(|d| {
                if let (Some(ref pg), Some(ref dg)) = (&profile.company_guid, &d.company_guid) {
                    pg.eq_ignore_ascii_case(dg)
                } else {
                    profile.company_name.eq_ignore_ascii_case(&d.company_name)
                }
            });

            let is_syncing = syncing_map.get(cid).copied().unwrap_or(false);
            let err = errors.get(cid).cloned();

            let status = if !is_tally_reachable {
                "Offline".to_string()
            } else if is_syncing {
                "Syncing…".to_string()
            } else if let Some(ref e) = err {
                format!("Error: {}", e)
            } else if is_active {
                "Connected · Active".to_string()
            } else if in_discovery {
                "Connected · Inactive in Tally".to_string()
            } else {
                "Offline / Not Open".to_string()
            };

            items.push(CompanyItem {
                company_name: profile.company_name.clone(),
                company_guid: profile.company_guid.clone(),
                alter_id: checkpoint.last_known_alter_id,
                is_active_in_tally: is_active,
                is_connected: true,
                connection_id: Some(cid.clone()),
                status,
                last_successful_sync: checkpoint.last_successful_sync,
                last_known_alter_id: checkpoint.last_known_alter_id,
                last_error: err,
                sync_in_progress: is_syncing,
                backfill_complete: checkpoint.backfill_complete,
            });

            processed_names.insert(profile.company_name.to_ascii_lowercase());
        }

        // 2. Process discovered companies that are NOT yet connected
        for disc in &discovered_raw {
            if processed_names.contains(&disc.company_name.to_ascii_lowercase()) {
                continue;
            }

            let is_active = if let Some(ref active) = active_company {
                if let (Some(ref dg), Some(ref ag)) = (&disc.company_guid, &active.company_guid) {
                    dg.eq_ignore_ascii_case(ag)
                } else {
                    disc.company_name.eq_ignore_ascii_case(&active.company_name)
                }
            } else {
                false
            };

            let status = if is_active {
                "Available · Active".to_string()
            } else {
                "Available · Not Active".to_string()
            };

            items.push(CompanyItem {
                company_name: disc.company_name.clone(),
                company_guid: disc.company_guid.clone(),
                alter_id: disc.alter_id,
                is_active_in_tally: is_active,
                is_connected: false,
                connection_id: None,
                status,
                last_successful_sync: None,
                last_known_alter_id: 0,
                last_error: None,
                sync_in_progress: false,
                backfill_complete: false,
            });
        }

        // Update overall AgentStatus
        let any_paired = !profiles.is_empty();
        let mut status = self.status.write().await;
        status.paired = any_paired;
        status.tally_reachable = is_tally_reachable;
        if let Some(first) = items.iter().find(|i| i.is_connected) {
            status.company_name = Some(first.company_name.clone());
            status.last_known_alter_id = first.last_known_alter_id;
            status.last_successful_sync = first.last_successful_sync.clone();
            status.backfill_complete = first.backfill_complete;
        } else {
            status.company_name = None;
        }

        Ok(items)
    }

    /// Returns cached company list without hitting Tally (for frequent UI polling).
    pub async fn get_companies_cached(&self) -> Vec<CompanyItem> {
        let profiles = self.profile_store.load_all().unwrap_or_default();
        let discovered_raw = self.discovered_companies.read().await.clone();
        let active_company = self.active_tally_company.read().await.clone();
        let errors = self.per_company_errors.read().await.clone();
        let syncing_map = self.syncing_connections.read().await.clone();
        let is_tally_reachable = active_company.is_some();

        let mut items: Vec<CompanyItem> = Vec::new();
        let mut processed_names: std::collections::HashSet<String> = std::collections::HashSet::new();

        for profile in &profiles {
            let cid = &profile.connection_id;
            let cp_store = CheckpointStore::for_connection(cid);
            let checkpoint = cp_store.load().unwrap_or_default();

            let is_active = if let Some(ref active) = active_company {
                if let (Some(ref pg), Some(ref ag)) = (&profile.company_guid, &active.company_guid) {
                    pg.eq_ignore_ascii_case(ag)
                } else {
                    profile.company_name.eq_ignore_ascii_case(&active.company_name)
                }
            } else {
                false
            };

            let in_discovery = discovered_raw.iter().any(|d| {
                if let (Some(ref pg), Some(ref dg)) = (&profile.company_guid, &d.company_guid) {
                    pg.eq_ignore_ascii_case(dg)
                } else {
                    profile.company_name.eq_ignore_ascii_case(&d.company_name)
                }
            });

            let is_syncing = syncing_map.get(cid).copied().unwrap_or(false);
            let err = errors.get(cid).cloned();

            let status = if !is_tally_reachable {
                "Offline".to_string()
            } else if is_syncing {
                "Syncing…".to_string()
            } else if let Some(ref e) = err {
                format!("Error: {}", e)
            } else if is_active {
                "Connected · Active".to_string()
            } else if in_discovery {
                "Connected · Inactive in Tally".to_string()
            } else {
                "Offline / Not Open".to_string()
            };

            items.push(CompanyItem {
                company_name: profile.company_name.clone(),
                company_guid: profile.company_guid.clone(),
                alter_id: checkpoint.last_known_alter_id,
                is_active_in_tally: is_active,
                is_connected: true,
                connection_id: Some(cid.clone()),
                status,
                last_successful_sync: checkpoint.last_successful_sync,
                last_known_alter_id: checkpoint.last_known_alter_id,
                last_error: err,
                sync_in_progress: is_syncing,
                backfill_complete: checkpoint.backfill_complete,
            });

            processed_names.insert(profile.company_name.to_ascii_lowercase());
        }

        for disc in &discovered_raw {
            if processed_names.contains(&disc.company_name.to_ascii_lowercase()) {
                continue;
            }

            let is_active = if let Some(ref active) = active_company {
                if let (Some(ref dg), Some(ref ag)) = (&disc.company_guid, &active.company_guid) {
                    dg.eq_ignore_ascii_case(ag)
                } else {
                    disc.company_name.eq_ignore_ascii_case(&active.company_name)
                }
            } else {
                false
            };

            let status = if is_active {
                "Available · Active".to_string()
            } else {
                "Available · Not Active".to_string()
            };

            items.push(CompanyItem {
                company_name: disc.company_name.clone(),
                company_guid: disc.company_guid.clone(),
                alter_id: disc.alter_id,
                is_active_in_tally: is_active,
                is_connected: false,
                connection_id: None,
                status,
                last_successful_sync: None,
                last_known_alter_id: 0,
                last_error: None,
                sync_in_progress: false,
                backfill_complete: false,
            });
        }

        items
    }

    /// Refresh status on demand.
    pub async fn refresh_status(&self) {
        let _ = self.discover_and_merge_companies().await;
    }

    /// Initiates browser device login specifically for the selected company.
    /// Does NOT require that this company is currently active in Tally.
    pub async fn start_company_device_login(
        &self,
        company_name: &str,
        _company_guid: Option<&str>,
    ) -> Result<DeviceAuthPublicSession, AgentError> {
        // Verify that Tally is reachable
        let tally = self.new_tally_client()?;
        let _ = tally.ping().await?;

        let cloud = self.new_cloud_client()?;
        let session = crate::device_auth::initiate_device_auth(&cloud, company_name).await?;

        log::info!("[device_login] Opening browser to {}", &session.verification_uri);
        let browser_opened = opener::open(&session.verification_uri).is_ok();

        let public_info = DeviceAuthPublicSession {
            user_code: session.user_code.clone(),
            verification_uri: session.verification_uri.clone(),
            browser_opened,
        };

        {
            let mut active_session = self.active_device_session.lock().await;
            *active_session = Some(session);
        }

        Ok(public_info)
    }

    /// Polls device login approval and completes registration for this specific company.
    pub async fn poll_company_device_login(
        &self,
        company_name: &str,
        company_guid: Option<&str>,
    ) -> Result<String, AgentError> {
        let session = {
            let active = self.active_device_session.lock().await;
            active.clone().ok_or_else(|| AgentError::Other("No active device authorization session".into()))?
        };

        let cloud = self.new_cloud_client()?;
        let status = crate::device_auth::poll_until_complete(&cloud, &session).await?;

        // Clear active session
        {
            let mut active_session = self.active_device_session.lock().await;
            *active_session = None;
        }

        match status {
            crate::device_auth::DeviceAuthStatus::Approved { agent_token, connection_id } => {
                log::info!("[device_login] Approved for company '{}'! Setting up connection...", company_name);
                let cid = if connection_id.trim().is_empty() {
                    format!("conn-{}", chrono::Utc::now().timestamp_millis())
                } else {
                    connection_id.trim().to_string()
                };

                self.complete_pairing_for_company(
                    &cid,
                    &agent_token,
                    company_name,
                    company_guid,
                )
                .await
            }
            crate::device_auth::DeviceAuthStatus::Denied => {
                Err(AgentError::Api(crate::errors::ApiError::DeviceAuthDenied))
            }
            crate::device_auth::DeviceAuthStatus::Expired => {
                Err(AgentError::Api(crate::errors::ApiError::DeviceAuthExpired))
            }
            _ => Err(AgentError::Api(crate::errors::ApiError::InvalidResponse(
                "Device authorization concluded without approval".into(),
            ))),
        }
    }

    pub async fn complete_pairing_for_company(
        &self,
        connection_id: &str,
        token: &str,
        company_name: &str,
        company_guid: Option<&str>,
    ) -> Result<String, AgentError> {
        let profile = ConnectionProfile {
            connection_id: connection_id.to_string(),
            company_guid: company_guid.map(String::from),
            company_name: company_name.to_string(),
            paired_at: chrono::Utc::now().to_rfc3339(),
        };

        log::info!("[pair] Saving connection profile for '{}' (cid: {})...", company_name, connection_id);
        self.profile_store.save(&profile)?;

        if !token.is_empty() {
            log::info!("[pair] Storing agent token in vault for connection {}...", connection_id);
            Vault::store_token_for(connection_id, token)?;
        }
        let _ = Vault::store_connection_id(connection_id);

        let conn_checkpoint_store = CheckpointStore::for_connection(connection_id);
        let _ = conn_checkpoint_store.save(&Checkpoint {
            connection_id: Some(connection_id.to_string()),
            last_known_alter_id: 0,
            last_successful_sync: None,
            backfill_complete: false,
        });

        // NOTE: We do NOT extract accounting data during pairing.
        // Data extraction only runs when user clicks Sync Now or during scheduled extraction,
        // after strict GUID verification against Tally's active company.

        let _ = self.discover_and_merge_companies().await;
        Ok(company_name.to_string())
    }

    pub async fn cancel_device_login(&self) {
        let mut active = self.active_device_session.lock().await;
        *active = None;
    }

    /// Syncs ONLY the requested connected company, verifying active Tally company by GUID first.
    pub async fn sync_company(&self, connection_id: &str) -> Result<SyncResult, AgentError> {
        // Prevent concurrent syncs
        {
            let mut syncing = self.syncing_connections.write().await;
            if syncing.get(connection_id).copied().unwrap_or(false) {
                return Err(AgentError::SyncInProgress);
            }
            syncing.insert(connection_id.to_string(), true);
        }

        let result = self.do_sync_company(connection_id).await;

        {
            let mut syncing = self.syncing_connections.write().await;
            syncing.remove(connection_id);
        }

        // Update error or success tracking
        {
            let mut errors = self.per_company_errors.write().await;
            match &result {
                Ok(r) if r.success => {
                    errors.remove(connection_id);
                }
                Ok(r) => {
                    if let Some(ref msg) = r.error_message {
                        errors.insert(connection_id.to_string(), msg.clone());
                    }
                }
                Err(e) => {
                    errors.insert(connection_id.to_string(), e.to_string());
                }
            }
        }

        if let Err(AgentError::Api(ApiError::TokenRevoked)) = &result {
            self.handle_token_revoked_for_connection(connection_id).await;
        }

        result
    }

    async fn do_sync_company(&self, connection_id: &str) -> Result<SyncResult, AgentError> {
        let profile = self.profile_store.get_by_connection_id(connection_id)?
            .ok_or_else(|| AgentError::Other(format!("Connection profile not found for {}", connection_id)))?;

        let token = Vault::get_token_for(connection_id)
            .or_else(|_| Vault::get_token())
            .map_err(|_| AgentError::Api(ApiError::NotPaired))?;

        let tally = self.new_tally_client()?;
        let cloud = self.new_cloud_client()?.with_token(&token);
        let conn_checkpoint_store = CheckpointStore::for_connection(connection_id);

        let orch = SyncOrchestrator::new(tally, cloud, conn_checkpoint_store.clone())
            .with_expected_profile(
                Some(profile.company_name.clone()),
                profile.company_guid.clone(),
                Some(connection_id.to_string()),
            );

        let checkpoint = conn_checkpoint_store.load().unwrap_or_default();
        if checkpoint.backfill_complete {
            orch.run_delta_sync().await
        } else {
            orch.run_initial_backfill().await
        }
    }

    pub async fn disconnect_company(&self, connection_id: &str) -> Result<(), AgentError> {
        let _ = Vault::delete_token_for(connection_id);
        self.profile_store.delete_for_connection(connection_id)?;
        {
            let mut errors = self.per_company_errors.write().await;
            errors.remove(connection_id);
        }
        let _ = self.discover_and_merge_companies().await;
        Ok(())
    }

    // Legacy compatibility methods
    pub async fn pair(&self, code: &str) -> Result<String, AgentError> {
        let tally = self.new_tally_client()?;
        let company = tally.ping().await?;
        let cloud = self.new_cloud_client()?;
        let response = cloud.pair(code, &company.company_name).await?;
        let token = response.token()?;
        let cid = response.connection_id().unwrap_or_else(|| "default-connection".into());
        self.complete_pairing_for_company(
            &cid,
            &token,
            &company.company_name,
            company.company_guid.as_deref(),
        )
        .await
    }

    pub async fn start_device_login(&self) -> Result<DeviceAuthPublicSession, AgentError> {
        let tally = self.new_tally_client()?;
        let company = tally.ping().await?;
        self.start_company_device_login(&company.company_name, company.company_guid.as_deref()).await
    }

    pub async fn poll_device_login(&self) -> Result<String, AgentError> {
        let tally = self.new_tally_client()?;
        let company = tally.ping().await?;
        self.poll_company_device_login(&company.company_name, company.company_guid.as_deref()).await
    }

    pub async fn login_via_browser(&self) -> Result<String, AgentError> {
        let _ = self.start_device_login().await?;
        self.poll_device_login().await
    }

    pub async fn complete_pairing_with_token(
        &self,
        token: &str,
        company_name: &str,
        company_guid: Option<&str>,
        connection_id: Option<&str>,
    ) -> Result<String, AgentError> {
        let cid = connection_id.unwrap_or("conn-default");
        self.complete_pairing_for_company(cid, token, company_name, company_guid).await
    }

    pub async fn handle_token_revoked(&self) {
        log::warn!("Agent token revoked by backend. Clearing local authentication state.");
        let _ = Vault::delete_token();
        if let Ok(profiles) = self.profile_store.load_all() {
            for p in profiles {
                let _ = Vault::delete_token_for(&p.connection_id);
            }
        }
        if let Ok(cid) = Vault::get_connection_id() {
            let _ = Vault::delete_token_for(&cid);
        }
        let _ = self.profile_store.delete();
        let mut status = self.status.write().await;
        status.paired = false;
        status.company_name = None;
        status.last_error = Some("Agent token was revoked or expired. Please re-pair your connection.".into());
    }

    pub async fn handle_token_revoked_for_connection(&self, connection_id: &str) {
        log::warn!("Agent token revoked for connection {connection_id}. Clearing local credentials.");
        let _ = Vault::delete_token_for(connection_id);
        let _ = Vault::delete_token();
        let _ = self.profile_store.delete_for_connection(connection_id);
        let mut status = self.status.write().await;
        status.paired = false;
        status.company_name = None;
        status.last_error = Some("Agent token was revoked or expired. Please re-pair your connection.".into());
    }

    pub async fn sync_now(&self) -> Result<SyncResult, AgentError> {
        let tally = self.new_tally_client()?;
        let active = tally.ping().await?;
        let profiles = self.profile_store.load_all().unwrap_or_default();
        let matched_profile = profiles.iter().find(|p| {
            if let (Some(ref pg), Some(ref ag)) = (&p.company_guid, &active.company_guid) {
                pg.eq_ignore_ascii_case(ag)
            } else {
                p.company_name.eq_ignore_ascii_case(&active.company_name)
            }
        });

        if let Some(p) = matched_profile {
            self.sync_company(&p.connection_id).await
        } else if let Some(first) = profiles.first() {
            // Will fail closed with explicit company mismatch error
            self.sync_company(&first.connection_id).await
        } else {
            Err(AgentError::Api(ApiError::NotPaired))
        }
    }

    pub async fn disconnect(&self) -> Result<(), AgentError> {
        let profiles = self.profile_store.load_all().unwrap_or_default();
        for p in profiles {
            let _ = self.disconnect_company(&p.connection_id).await;
        }
        let _ = Vault::delete_token();
        let _ = Vault::delete_connection_id();
        let _ = self.profile_store.delete();
        Ok(())
    }

    pub async fn get_status(&self) -> AgentStatus {
        let _ = self.discover_and_merge_companies().await;
        self.status.read().await.clone()
    }
}

pub fn spawn_heartbeat_loop(state: Arc<AgentState>) {
    tauri::async_runtime::spawn(async move {
        let mut discovery_ticker = interval(Duration::from_secs(DISCOVERY_INTERVAL_SECS));
        let mut heartbeat_ticker = interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));

        loop {
            tokio::select! {
                _ = discovery_ticker.tick() => {
                    let _ = state.discover_and_merge_companies().await;
                }
                _ = heartbeat_ticker.tick() => {
                    let active_tally = state.active_tally_company.read().await.clone();
                    let tally_reachable = active_tally.is_some();

                    let profiles = state.profile_store.load_all().unwrap_or_default();
                    if profiles.is_empty() {
                        continue;
                    }

                    // Send heartbeat only for the currently active company in Tally (if connected)
                    if let Some(ref active) = active_tally {
                        if let Some(profile) = profiles.iter().find(|p| {
                            if let (Some(ref pg), Some(ref ag)) = (&p.company_guid, &active.company_guid) {
                                pg.eq_ignore_ascii_case(ag)
                            } else {
                                p.company_name.eq_ignore_ascii_case(&active.company_name)
                            }
                        }) {
                            let cid = &profile.connection_id;
                            let token = match Vault::get_token_for(cid).or_else(|_| Vault::get_token()) {
                                Ok(t) => t,
                                Err(_) => continue,
                            };

                            let cp_store = CheckpointStore::for_connection(cid);
                            let checkpoint = cp_store.load().unwrap_or_default();

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
                                    if resp.is_sync_requested() {
                                        log::info!("Dashboard requested sync via heartbeat for connection {}", cid);
                                        let _ = state.sync_company(cid).await;
                                    }
                                }
                                Err(ApiError::TokenRevoked) => {
                                    log::warn!("Token revoked for connection {}", cid);
                                    let _ = state.disconnect_company(cid).await;
                                }
                                Err(e) => {
                                    log::debug!("Heartbeat error: {:?}", e);
                                }
                            }
                        }
                    }
                }
            }
        }
    });
}
