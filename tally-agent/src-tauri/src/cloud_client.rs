//! One-way HTTPS client — agent only pushes data outward, never reads backend data.

use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::device_auth::{
    DeviceInitiateRequest, DeviceInitiateResponse, DevicePollRequest, DevicePollResponse,
};
use crate::errors::ApiError;
use crate::redact::redact;

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

#[derive(Debug, Clone)]
pub struct CloudClient {
    http: Client,
    base_url: String,
    bearer_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(non_snake_case)]
struct BackendErrorPayload {
    code: Option<String>,
    error: Option<serde_json::Value>,
    message: Option<String>,
}

fn is_explicit_token_revocation(status: reqwest::StatusCode, body: &str) -> bool {
    if status.as_u16() != 401 {
        return false;
    }
    let body_trimmed = body.trim();
    if body_trimmed.is_empty() {
        return true;
    }

    // HTML / XML responses indicate an upstream proxy, CDN, or gateway error rather than an API token rejection
    if body_trimmed.starts_with("<!DOCTYPE")
        || body_trimmed.starts_with("<html")
        || body_trimmed.starts_with("<?xml")
    {
        return false;
    }

    if let Ok(parsed) = serde_json::from_str::<BackendErrorPayload>(body_trimmed) {
        let code = parsed.code.as_deref().unwrap_or("").to_ascii_uppercase();
        let msg = parsed.message.as_deref().unwrap_or("").to_ascii_lowercase();

        let error_str = match &parsed.error {
            Some(serde_json::Value::String(s)) => s.to_ascii_lowercase(),
            Some(serde_json::Value::Object(map)) => {
                map.get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_ascii_lowercase()
            }
            _ => String::new(),
        };

        if matches!(
            code.as_str(),
            "UNAUTHORIZED" | "TOKEN_REVOKED" | "TOKEN_EXPIRED" | "INVALID_TOKEN" | "CONNECTION_REVOKED"
        ) {
            return true;
        }

        if msg.contains("invalid")
            || msg.contains("revoked")
            || msg.contains("expired")
            || msg.contains("unauthorized")
            || error_str.contains("invalid")
            || error_str.contains("revoked")
            || error_str.contains("expired")
            || error_str.contains("unauthorized")
        {
            return true;
        }
    }

    let lower = body_trimmed.to_ascii_lowercase();
    lower.contains("invalid")
        || lower.contains("revoked")
        || lower.contains("expired")
        || lower.contains("unauthorized")
        || !body_trimmed.starts_with('{')
}

fn reqwest_error(e: reqwest::Error) -> ApiError {
    if e.is_timeout() {
        ApiError::Network(format!("Request timed out: {}", e))
    } else {
        ApiError::Network(e.to_string())
    }
}

impl CloudClient {
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
    pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

    pub fn new() -> Result<Self, ApiError> {
        Self::with_timeouts(Self::DEFAULT_TIMEOUT, Self::DEFAULT_CONNECT_TIMEOUT)
    }

    pub fn with_timeouts(timeout: Duration, connect_timeout: Duration) -> Result<Self, ApiError> {
        let base = api_base_url();
        log::info!("FinInsight Agent using API base: {}", base);
        let http = Client::builder()
            .timeout(timeout)
            .connect_timeout(connect_timeout)
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
        Self::from_base_url_with_timeouts(url, Self::DEFAULT_TIMEOUT, Self::DEFAULT_CONNECT_TIMEOUT)
    }

    pub fn from_base_url_with_timeouts(
        url: &str,
        timeout: Duration,
        connect_timeout: Duration,
    ) -> Result<Self, ApiError> {
        let http = Client::builder()
            .timeout(timeout)
            .connect_timeout(connect_timeout)
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

    // ==============================================================================================
    // DESIGN NOTE: Short-Lived Access Token & Refresh Token Mechanism (Follow-Up Architecture)
    // ==============================================================================================
    // Current State:
    //   The agent currently uses long-lived agent tokens obtained during device authorization/pairing.
    //   Tokens are stored in the OS credential vault (Vault::store_token_for) and sent via
    //   `Authorization: Bearer <token>` on all requests (delta sync, heartbeat, revoke).
    //   If revoked or expired, the backend returns HTTP 401 (ApiError::TokenRevoked), forcing re-pairing.
    //
    // Target Architecture:
    //   Transition to OAuth 2.0 / RFC 6749 short-lived access tokens (e.g. 15-60 min TTL) paired with
    //   long-lived refresh tokens (with single-use rotation) or mTLS/device binding.
    //
    // Requirements for Backend (Unified-Accounting-Gateway):
    //   1. Endpoint: POST /api/v1/agent/token/refresh
    //      - Accepts `{ refresh_token: String, connection_id: String }`
    //      - Returns `{ access_token: String, expires_in: u64, refresh_token: Option<String> }`
    //      - Enforces single-use refresh token rotation (RTR) to detect token theft.
    //   2. Response headers / error payloads:
    //      - Return HTTP 401 with standard `WWW-Authenticate: Bearer error="invalid_token", error_description="token expired"`
    //        or JSON code `"TOKEN_EXPIRED"` vs `"TOKEN_REVOKED"` so client knows when to refresh vs re-pair.
    //
    // Requirements for Agent (tally-agent):
    //   1. Vault Storage:
    //      - Store both `access_token` and `refresh_token` per connection (e.g. `Vault::store_tokens_for(...)`).
    //      - Track access token expiration time locally in memory to proactively refresh before expiry.
    //   2. CloudClient Token Manager / Middleware:
    //      - Proactive refresh: if access token has expired (or within a 60s buffer), automatically
    //        execute refresh call before dispatching the outbound request.
    //      - Reactive refresh: on HTTP 401 TOKEN_EXPIRED, execute single refresh attempt and retry in-flight request.
    //        If refresh fails with 401/revoked, fail closed with ApiError::TokenRevoked.
    //      - Concurrency safety: synchronize refresh calls per connection via tokio::sync::Mutex to prevent
    //        race conditions between concurrent heartbeat and delta sync calls attempting parallel rotation.
    // ==============================================================================================
    async fn bearer_token(&self) -> Result<String, ApiError> {
        if let Some(ref t) = self.bearer_token {
            let clean = t.trim();
            if !clean.is_empty() {
                return Ok(clean.to_string());
            }
        }
        Err(ApiError::NotPaired)
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
            .map_err(reqwest_error)?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(reqwest_error)?;

        log::debug!("[cloud_client] Pair response HTTP {}, body: {}", status, redact(&text));

        if !status.is_success() {
            log::warn!("Pairing failed with status {}: {}", status, redact(&text));
            return Err(ApiError::PairingFailed);
        }

        serde_json::from_str::<PairResponse>(&text).map_err(|e| {
            log::error!("[cloud_client] Deserialization error: {} for text: {}", e, redact(&text));
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
            .map_err(reqwest_error)?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(reqwest_error)?;

        if !status.is_success() {
            log::warn!("Device initiate failed with status {}: {}", status, redact(&text));
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
            .map_err(reqwest_error)?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(reqwest_error)?;

        if !status.is_success() && status.as_u16() != 400 {
            log::warn!("Device poll failed with status {}: {}", status, redact(&text));
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

    /// POST /api/v1/agent/revoke — best-effort server-side token revocation on disconnect.
    ///
    /// NOTE: Endpoint is pending implementation in Unified-Accounting-Gateway. Until available,
    /// calls will return HTTP 404 which the agent catches and logs at WARN level without blocking local disconnect.
    pub async fn revoke_token(&self, connection_id: Option<&str>) -> Result<(), ApiError> {
        let token = self.bearer_token().await?;
        let mut req = self
            .http
            .post(self.url("/api/v1/agent/revoke"))
            .bearer_auth(&token);

        if let Some(cid) = connection_id {
            req = req.json(&serde_json::json!({ "connection_id": cid }));
        }

        let resp = req
            .send()
            .await
            .map_err(reqwest_error)?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            log::warn!(
                "[cloud_client] Revocation request returned status {}: {}",
                status,
                redact(&body)
            );
            return Err(ApiError::Network(format!("Revocation failed with status {}", status)));
        }
        Ok(())
    }

    /// POST /api/v1/agent/sync/delta — bearer auth, one-way push for initial batches.
    pub async fn push_initial_batch(&self, batch: &SyncBatchPayload) -> Result<(), ApiError> {
        let token = self.bearer_token().await?;
        let has_token = !token.is_empty();
        let resp = self
            .http
            .post(self.url("/api/v1/agent/sync/delta"))
            .bearer_auth(&token)
            .json(batch)
            .send()
            .await
            .map_err(reqwest_error)?;

        let status = resp.status();
        log::debug!(
            "[cloud_client] POST /api/v1/agent/sync/delta (initial batch {}/{}) -> HTTP {} (token_present: {})",
            batch.batch_index + 1,
            batch.total_batches,
            status,
            has_token
        );

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            if is_explicit_token_revocation(status, &body) {
                log::warn!(
                    "[cloud_client] Initial batch push returned HTTP {} with confirmed token revocation (body: {})",
                    status,
                    redact(&body)
                );
                return Err(ApiError::TokenRevoked);
            }
            log::warn!(
                "[cloud_client] Initial batch failed with HTTP {} (body: {})",
                status,
                redact(&body)
            );
            return Err(ApiError::SyncFailed(status.as_u16()));
        }
        Ok(())
    }

    /// POST /api/v1/agent/sync/delta — bearer auth, one-way push.
    pub async fn push_delta(&self, payload: &DeltaSyncPayload) -> Result<(), ApiError> {
        let token = self.bearer_token().await?;
        let has_token = !token.is_empty();
        let resp = self
            .http
            .post(self.url("/api/v1/agent/sync/delta"))
            .bearer_auth(&token)
            .json(payload)
            .send()
            .await
            .map_err(reqwest_error)?;

        let status = resp.status();
        log::debug!(
            "[cloud_client] POST /api/v1/agent/sync/delta (records: {}) -> HTTP {} (token_present: {})",
            payload.records.len(),
            status,
            has_token
        );

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            if is_explicit_token_revocation(status, &body) {
                log::warn!(
                    "[cloud_client] Delta sync push returned HTTP {} with confirmed token revocation (body: {})",
                    status,
                    redact(&body)
                );
                return Err(ApiError::TokenRevoked);
            }
            log::warn!(
                "[cloud_client] Delta sync failed with HTTP {} (body: {})",
                status,
                redact(&body)
            );
            return Err(ApiError::SyncFailed(status.as_u16()));
        }
        Ok(())
    }

    /// POST /api/v1/agent/heartbeat — reports liveness only, may return sync_requested flag.
    pub async fn heartbeat(&self, payload: &HeartbeatPayload) -> Result<HeartbeatResponse, ApiError> {
        let token = self.bearer_token().await?;
        let has_token = !token.is_empty();
        let resp = self
            .http
            .post(self.url("/api/v1/agent/heartbeat"))
            .bearer_auth(&token)
            .json(payload)
            .send()
            .await
            .map_err(reqwest_error)?;

        let status = resp.status();
        log::debug!(
            "[cloud_client] POST /api/v1/agent/heartbeat -> HTTP {} (token_present: {})",
            status,
            has_token
        );

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            if is_explicit_token_revocation(status, &body) {
                log::warn!(
                    "[cloud_client] Heartbeat returned HTTP {} with confirmed token revocation (body: {})",
                    status,
                    redact(&body)
                );
                return Err(ApiError::TokenRevoked);
            }
            log::warn!(
                "[cloud_client] Heartbeat non-auth failure HTTP {} (body: {})",
                status,
                redact(&body)
            );
            return Err(ApiError::HeartbeatFailed(status.as_u16()));
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
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn api_base_is_non_empty() {
        let base = api_base_url();
        assert!(!base.is_empty());
    }

    #[tokio::test]
    async fn bearer_token_returns_not_paired_when_unset_even_if_legacy_token_exists() {
        crate::vault::Vault::store_token("legacy-test-token-never-fallback").ok();

        let client = CloudClient::from_base_url("http://127.0.0.1:18888").unwrap();
        let res = client.bearer_token().await;
        assert!(
            matches!(res, Err(ApiError::NotPaired)),
            "Must return NotPaired when bearer_token is not set on client, ignoring legacy Vault token"
        );

        let _ = crate::vault::Vault::delete_token();
    }

    #[tokio::test]
    async fn default_timeouts_are_configured() {
        assert_eq!(CloudClient::DEFAULT_TIMEOUT, Duration::from_secs(30));
        assert_eq!(CloudClient::DEFAULT_CONNECT_TIMEOUT, Duration::from_secs(10));
    }

    #[tokio::test]
    async fn request_times_out_within_bounded_time() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/agent/pair"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(800)))
            .mount(&server)
            .await;

        let client = CloudClient::from_base_url_with_timeouts(
            &server.uri(),
            Duration::from_millis(150),
            Duration::from_millis(100),
        )
        .unwrap();

        let start = std::time::Instant::now();
        let result = client.pair("ABCDEF", "Company").await;
        let elapsed = start.elapsed();

        assert!(
            result.is_err(),
            "Expected timeout error on delayed response, got {:?}",
            result
        );
        assert!(
            elapsed < Duration::from_millis(600),
            "Request must time out within bounded time (took {:?})",
            elapsed
        );
        match result.unwrap_err() {
            ApiError::Network(msg) => {
                assert!(
                    msg.to_ascii_lowercase().contains("timeout")
                        || msg.to_ascii_lowercase().contains("timed out"),
                    "Network error should indicate timeout: {}",
                    msg
                );
            }
            other => panic!("Expected ApiError::Network, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn response_body_in_error_logs_is_redacted() {
        let server = MockServer::start().await;
        let sensitive_token = "agent-tok-supersecret1234567890abcdef";
        let sensitive_gstin = "27AABCU9603R1ZM";

        Mock::given(method("POST"))
            .and(path("/api/v1/agent/pair"))
            .respond_with(ResponseTemplate::new(500).set_body_string(format!(
                r#"{{"error": "failure with token {} and gstin {}"}}"#,
                sensitive_token, sensitive_gstin
            )))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/v1/agent/device/initiate"))
            .respond_with(ResponseTemplate::new(500).set_body_string(format!(
                r#"{{"error": "initiate failed with token {}"}}"#,
                sensitive_token
            )))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/v1/agent/device/poll"))
            .respond_with(ResponseTemplate::new(500).set_body_string(format!(
                r#"{{"error": "poll failed with token {}"}}"#,
                sensitive_token
            )))
            .mount(&server)
            .await;

        let client = CloudClient::from_base_url(&server.uri()).unwrap();

        // 1. Pair
        let pair_err = client.pair("ABCDEF", "Company").await.unwrap_err();
        assert!(matches!(pair_err, ApiError::PairingFailed));

        // 2. Initiate device
        let init_req = DeviceInitiateRequest {
            company_name: Some("Test Company".into()),
            ..Default::default()
        };
        let init_err = client.initiate_device(&init_req).await.unwrap_err();
        if let ApiError::InvalidResponse(msg) = init_err {
            assert!(
                !msg.contains(sensitive_token),
                "Error message must redact token: {}",
                msg
            );
            assert!(
                msg.contains("[REDACTED"),
                "Error message must contain redacted indicator: {}",
                msg
            );
        } else {
            panic!("Expected InvalidResponse, got {:?}", init_err);
        }

        // 3. Poll device
        let poll_err = client.poll_device("dev-code-123").await.unwrap_err();
        if let ApiError::InvalidResponse(msg) = poll_err {
            assert!(
                !msg.contains(sensitive_token),
                "Error message must redact token: {}",
                msg
            );
            assert!(
                msg.contains("[REDACTED"),
                "Error message must contain redacted indicator: {}",
                msg
            );
        } else {
            panic!("Expected InvalidResponse, got {:?}", poll_err);
        }
    }

    #[tokio::test]
    async fn revoke_token_sends_bearer_and_connection_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/agent/revoke"))
            .and(header("authorization", "Bearer test-revoke-bearer-token"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = CloudClient::from_base_url(&server.uri())
            .unwrap()
            .with_token("test-revoke-bearer-token");

        let res = client.revoke_token(Some("conn-test-revoke-1")).await;
        assert!(res.is_ok(), "revoke_token must succeed on 200 OK");

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(body["connection_id"], "conn-test-revoke-1");
    }
}
