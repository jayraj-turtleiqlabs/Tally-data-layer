//! One-way HTTPS client — agent only pushes data outward, never reads backend data.

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::errors::ApiError;
use crate::redact::redact;
use crate::vault::Vault;

/// Baked in at compile time via FININSIGHT_API_BASE env var.
pub fn api_base_url() -> &'static str {
    option_env!("FININSIGHT_API_BASE").unwrap_or("http://localhost:3000")
}

#[derive(Debug, Clone, Serialize)]
pub struct PairRequest {
    pub pairing_code: String,
    pub tally_company_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PairResponse {
    pub agent_token: String,
    pub company_name: String,
    pub connection_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncBatchPayload {
    pub batch_index: u32,
    pub total_batches: u32,
    pub entity_type: String,
    pub records: Vec<serde_json::Value>,
    pub alter_id_high: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct InitialSyncPayload {
    pub batches: Vec<SyncBatchPayload>,
    pub final_alter_id: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeltaSyncPayload {
    pub records: Vec<serde_json::Value>,
    pub alter_id_high: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct HeartbeatPayload {
    pub tally_reachable: bool,
    pub agent_version: String,
    pub last_known_alter_id: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HeartbeatResponse {
    /// Dashboard may set this flag for the agent to poll — optional piggyback.
    pub sync_requested: bool,
}

pub struct CloudClient {
    http: Client,
    base_url: String,
    bearer_token: Option<String>,
}

impl CloudClient {
    pub fn new() -> Result<Self, ApiError> {
        let http = Client::builder()
            .build()
            .map_err(|e| ApiError::Network(e.to_string()))?;
        Ok(Self {
            http,
            base_url: api_base_url().trim_end_matches('/').to_string(),
            bearer_token: None,
        })
    }

    pub fn with_base_url(mut self, url: &str) -> Self {
        self.base_url = url.trim_end_matches('/').to_string();
        self
    }

    pub fn with_token(mut self, token: &str) -> Self {
        self.bearer_token = Some(token.to_string());
        self
    }

    pub fn from_base_url(url: &str) -> Result<Self, ApiError> {
        let http = Client::builder()
            .build()
            .map_err(|e| ApiError::Network(e.to_string()))?;
        Ok(Self {
            http,
            base_url: url.trim_end_matches('/').to_string(),
            bearer_token: None,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    async fn bearer_token(&self) -> Result<String, ApiError> {
        if let Some(ref t) = self.bearer_token {
            return Ok(t.clone());
        }
        Vault::get_token().map_err(|_| ApiError::NotPaired)
    }

    /// POST /api/v1/agent/pair — no auth required.
    pub async fn pair(&self, code: &str, tally_company: &str) -> Result<PairResponse, ApiError> {
        let body = PairRequest {
            pairing_code: code.to_uppercase(),
            tally_company_name: tally_company.to_string(),
        };

        log::info!("Pairing attempt initiated");

        let resp = self
            .http
            .post(self.url("/api/v1/agent/pair"))
            .json(&body)
            .send()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;

        if !resp.status().is_success() {
            log::warn!("Pairing failed with status {}", resp.status());
            return Err(ApiError::PairingFailed);
        }

        resp.json::<PairResponse>()
            .await
            .map_err(|e| ApiError::InvalidResponse(redact(&e.to_string())))
    }

    /// POST /api/v1/sync/initial — bearer auth, one-way push.
    pub async fn push_initial_batch(&self, batch: &SyncBatchPayload) -> Result<(), ApiError> {
        let token = self.bearer_token().await?;
        let resp = self
            .http
            .post(self.url("/api/v1/sync/initial"))
            .bearer_auth(&token)
            .json(batch)
            .send()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;

        if resp.status().as_u16() != 200 {
            return Err(ApiError::SyncFailed(resp.status().as_u16()));
        }
        Ok(())
    }

    /// POST /api/v1/sync/delta — bearer auth, one-way push.
    pub async fn push_delta(&self, payload: &DeltaSyncPayload) -> Result<(), ApiError> {
        let token = self.bearer_token().await?;
        let resp = self
            .http
            .post(self.url("/api/v1/sync/delta"))
            .bearer_auth(&token)
            .json(payload)
            .send()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;

        if resp.status().as_u16() != 200 {
            return Err(ApiError::SyncFailed(resp.status().as_u16()));
        }
        Ok(())
    }

    /// POST /api/v1/agent/heartbeat — reports liveness only, may return sync_requested flag.
    pub async fn heartbeat(&self, payload: &HeartbeatPayload) -> Result<HeartbeatResponse, ApiError> {
        let token = self.bearer_token().await?;
        let resp = self
            .http
            .post(self.url("/api/v1/agent/heartbeat"))
            .bearer_auth(&token)
            .json(payload)
            .send()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ApiError::HeartbeatFailed(resp.status().as_u16()));
        }

        resp.json::<HeartbeatResponse>()
            .await
            .map_err(|e| ApiError::InvalidResponse(redact(&e.to_string())))
    }
}

impl Default for CloudClient {
    fn default() -> Self {
        Self::new().expect("cloud client")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_base_not_hardcoded_localhost_in_release() {
        let base = api_base_url();
        #[cfg(not(debug_assertions))]
        assert!(
            !base.contains("localhost"),
            "Release builds must not use localhost"
        );
        #[cfg(debug_assertions)]
        assert!(!base.is_empty());
    }
}
