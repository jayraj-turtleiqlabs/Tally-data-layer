//! One-way HTTPS client — agent only pushes data outward, never reads backend data.

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::device_auth::{
    DeviceInitiateRequest, DeviceInitiateResponse, DevicePollRequest, DevicePollResponse,
};
use crate::errors::ApiError;
use crate::redact::redact;
use crate::vault::Vault;

/// Resolved at runtime via FININSIGHT_API_BASE env var, falling back to compile-time env or production Render base.
pub fn api_base_url() -> String {
    std::env::var("FININSIGHT_API_BASE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            option_env!("FININSIGHT_API_BASE")
                .unwrap_or("https://fininsight-api-vzv5.onrender.com")
                .to_string()
        })
}

#[derive(Debug, Clone, Serialize)]
pub struct PairRequest {
    pub code: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(non_snake_case)]
pub struct PairResponse {
    pub token: Option<String>,
    pub agent_token: Option<String>,
    pub agentToken: Option<String>,
    pub connection_id: Option<String>,
    pub connectionId: Option<String>,
    pub organization_id: Option<String>,
    pub organizationId: Option<String>,
    pub company_name: Option<String>,
    pub companyName: Option<String>,
    pub data: Option<serde_json::Value>,
}

impl PairResponse {
    pub fn token(&self) -> Result<String, ApiError> {
        if let Some(ref t) = self.token {
            if !t.is_empty() {
                return Ok(t.clone());
            }
        }
        if let Some(ref t) = self.agent_token {
            if !t.is_empty() {
                return Ok(t.clone());
            }
        }
        if let Some(ref t) = self.agentToken {
            if !t.is_empty() {
                return Ok(t.clone());
            }
        }
        if let Some(ref d) = self.data {
            if let Some(t) = d.get("token").or_else(|| d.get("agent_token")).or_else(|| d.get("agentToken")) {
                if let Some(s) = t.as_str() {
                    if !s.is_empty() {
                        return Ok(s.to_string());
                    }
                }
            }
        }
        Err(ApiError::InvalidResponse("No token field in pairing response".to_string()))
    }

    pub fn connection_id(&self) -> Option<String> {
        self.connection_id
            .clone()
            .or_else(|| self.connectionId.clone())
            .or_else(|| {
                self.data.as_ref().and_then(|d| {
                    d.get("connection_id")
                        .or_else(|| d.get("connectionId"))
                        .and_then(|v| v.as_str().map(String::from))
                })
            })
    }

    pub fn company_name(&self) -> Option<String> {
        self.company_name
            .clone()
            .or_else(|| self.companyName.clone())
            .or_else(|| {
                self.data.as_ref().and_then(|d| {
                    d.get("company_name")
                        .or_else(|| d.get("companyName"))
                        .and_then(|v| v.as_str().map(String::from))
                })
            })
    }
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

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(non_snake_case)]
pub struct HeartbeatResponse {
    pub sync_requested: Option<bool>,
    pub syncRequested: Option<bool>,
}

impl HeartbeatResponse {
    pub fn is_sync_requested(&self) -> bool {
        self.sync_requested.unwrap_or(false) || self.syncRequested.unwrap_or(false)
    }
}

pub struct CloudClient {
    http: Client,
    base_url: String,
    bearer_token: Option<String>,
}

impl CloudClient {
    pub fn new() -> Result<Self, ApiError> {
        let base = api_base_url();
        println!("=== FinInsight Agent using API base: {} ===", base);
        log::info!("FinInsight Agent using API base: {}", base);
        let http = Client::builder()
            .build()
            .map_err(|e| ApiError::Network(e.to_string()))?;
        Ok(Self {
            http,
            base_url: base.trim_end_matches('/').to_string(),
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
    pub async fn pair(&self, code: &str, _tally_company: &str) -> Result<PairResponse, ApiError> {
        let body = PairRequest {
            code: code.to_uppercase(),
        };

        log::info!("Pairing attempt initiated");

        let resp = self
            .http
            .post(self.url("/api/v1/agent/pair"))
            .json(&body)
            .send()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;

        println!("[cloud_client] Pair response HTTP {}, body: {}", status, text);

        if !status.is_success() {
            log::warn!("Pairing failed with status {}: {}", status, text);
            return Err(ApiError::PairingFailed);
        }

        serde_json::from_str::<PairResponse>(&text).map_err(|e| {
            println!("[cloud_client] Deserialization error: {} for text: {}", e, text);
            ApiError::InvalidResponse(redact(&e.to_string()))
        })
    }

    /// POST /api/v1/agent/device/initiate — requests device authorization codes.
    pub async fn initiate_device(
        &self,
        request: &DeviceInitiateRequest,
    ) -> Result<DeviceInitiateResponse, ApiError> {
        log::info!("Device authorization initiation attempt");

        let resp = self
            .http
            .post(self.url("/api/v1/agent/device/initiate"))
            .json(request)
            .send()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;

        if !status.is_success() {
            log::warn!("Device initiate failed with status {}: {}", status, text);
            return Err(ApiError::InvalidResponse(format!(
                "Device initiate failed with HTTP {}: {}",
                status,
                redact(&text)
            )));
        }

        serde_json::from_str::<DeviceInitiateResponse>(&text).map_err(|e| {
            ApiError::InvalidResponse(format!(
                "Invalid device initiate response: {}",
                redact(&e.to_string())
            ))
        })
    }

    /// POST /api/v1/agent/device/poll — polls device authorization status.
    pub async fn poll_device(&self, device_code: &str) -> Result<DevicePollResponse, ApiError> {
        let body = DevicePollRequest { device_code };
        let resp = self
            .http
            .post(self.url("/api/v1/agent/device/poll"))
            .json(&body)
            .send()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;

        if !status.is_success() && status.as_u16() != 400 {
            log::warn!("Device poll failed with status {}: {}", status, text);
            return Err(ApiError::InvalidResponse(format!(
                "Device poll HTTP {}: {}",
                status,
                redact(&text)
            )));
        }

        serde_json::from_str::<DevicePollResponse>(&text).map_err(|e| {
            ApiError::InvalidResponse(format!(
                "Invalid device poll response: {}",
                redact(&e.to_string())
            ))
        })
    }

    /// POST /api/v1/agent/sync/delta — bearer auth, one-way push for initial batches.
    pub async fn push_initial_batch(&self, batch: &SyncBatchPayload) -> Result<(), ApiError> {
        let token = self.bearer_token().await?;
        let resp = self
            .http
            .post(self.url("/api/v1/agent/sync/delta"))
            .bearer_auth(&token)
            .json(batch)
            .send()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;

        let status = resp.status();
        println!(
            "[cloud_client] Initial batch {}/{} response HTTP {}",
            batch.batch_index + 1,
            batch.total_batches,
            status
        );

        if status.as_u16() == 401 || status.as_u16() == 403 {
            log::warn!(
                "Initial batch push returned HTTP {} - token revoked or unauthorized",
                status
            );
            return Err(ApiError::TokenRevoked);
        }

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            println!("[cloud_client] Initial batch failed body: {}", body);
            return Err(ApiError::SyncFailed(status.as_u16()));
        }
        Ok(())
    }

    /// POST /api/v1/agent/sync/delta — bearer auth, one-way push.
    pub async fn push_delta(&self, payload: &DeltaSyncPayload) -> Result<(), ApiError> {
        let token = self.bearer_token().await?;
        let resp = self
            .http
            .post(self.url("/api/v1/agent/sync/delta"))
            .bearer_auth(&token)
            .json(payload)
            .send()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;

        let status = resp.status();
        println!("[cloud_client] Delta sync response HTTP {}", status);

        if status.as_u16() == 401 || status.as_u16() == 403 {
            log::warn!(
                "Delta sync push returned HTTP {} - token revoked or unauthorized",
                status
            );
            return Err(ApiError::TokenRevoked);
        }

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            println!("[cloud_client] Delta sync failed body: {}", body);
            return Err(ApiError::SyncFailed(status.as_u16()));
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

        if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 {
            log::warn!("Heartbeat returned HTTP {} - token revoked or unauthorized", resp.status());
            return Err(ApiError::TokenRevoked);
        }

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
    fn api_base_is_non_empty() {
        let base = api_base_url();
        assert!(!base.is_empty());
    }
}
