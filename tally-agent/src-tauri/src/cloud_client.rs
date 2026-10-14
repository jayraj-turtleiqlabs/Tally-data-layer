//! One-way HTTPS client — agent only pushes data outward, never reads backend data.

use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::device_auth::{
    DeviceInitiateRequest, DeviceInitiateResponse, DevicePollRequest, DevicePollResponse,
};
use crate::errors::ApiError;
use crate::redact::redact;

/// Resolved at runtime via FININSIGHT_API_BASE env var, falling back to production base.
pub fn api_base_url() -> String {
    let base_url = std::env::var("FININSIGHT_API_BASE")
        .unwrap_or_else(|_| "https://fin.turtleiqlabs.com".to_string());
    base_url
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncBatchPayload {
    #[serde(rename = "companyName", skip_serializing_if = "Option::is_none")]
    pub company_name: Option<String>,
    pub batch_index: u32,
    pub total_batches: u32,
    pub entity_type: String,
    pub records: Vec<serde_json::Value>,
    pub alter_id_high: u64,
    #[serde(rename = "newAlterId")]
    pub new_alter_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct InitialSyncPayload {
    pub batches: Vec<SyncBatchPayload>,
    pub final_alter_id: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeltaSyncPayload {
    #[serde(rename = "companyName", skip_serializing_if = "Option::is_none")]
    pub company_name: Option<String>,
    pub records: Vec<serde_json::Value>,
    pub alter_id_high: u64,
    #[serde(rename = "newAlterId")]
    pub new_alter_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HeartbeatPayload {
    pub tally_reachable: bool,
    pub agent_version: String,
    pub last_known_alter_id: u64,
    pub timestamp: String,
    #[serde(rename = "lastSuccessfulSyncAt", skip_serializing_if = "Option::is_none")]
    pub last_successful_sync_at: Option<String>,
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
    // DESIGN NOTE: 90-Day Sliding Agent Token Model
    // ==============================================================================================
    // Current Architecture:
    //   Agent tokens are long-lived with a sliding 90-day expiry extended automatically on each heartbeat.
    //   Tokens are stored in the OS credential vault (Vault::store_token_for) and sent via
    //   `Authorization: Bearer <token>` on all outbound agent requests (delta sync, heartbeat, revoke).
    //
    //   There is no separate refresh token endpoint (/agent/token/refresh) and no TOKEN_EXPIRED code.
    //   If an agent remains offline for more than 90 days, or if the connection is revoked,
    //   the gateway returns HTTP 401, forcing the agent to re-pair via the RFC 8628 device authorization flow.
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
    /// The gateway returns HTTP 200 (idempotent), HTTP 403 if connection_id mismatches,
    /// or HTTP 429 when rate limited.
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

    /// Helper to validate 2xx response body from sync push.
    /// If `success == false`, returns `Err(ApiError::SyncFailed(status))`.
    /// If body is empty or unparseable JSON on 2xx, treats as success and logs at WARN.
    fn check_sync_response_body(status: reqwest::StatusCode, body: &str) -> Result<(), ApiError> {
        let trimmed = body.trim();
        if trimmed.is_empty() {
            log::warn!(
                "[cloud_client] Empty response body on HTTP {} sync push; treating as success",
                status
            );
            return Ok(());
        }

        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(json_val) => {
                if let Some(false) = json_val.get("success").and_then(|v| v.as_bool()) {
                    log::warn!(
                        "[cloud_client] Sync push response indicated failure (success=false): {}",
                        redact(trimmed)
                    );
                    return Err(ApiError::SyncFailed(status.as_u16()));
                }
                Ok(())
            }
            Err(e) => {
                log::warn!(
                    "[cloud_client] Unparseable response body on HTTP {} sync push ({}); treating as success: {}",
                    status,
                    e,
                    redact(trimmed)
                );
                Ok(())
            }
        }
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
        let body = resp.text().await.unwrap_or_default();
        log::debug!(
            "[cloud_client] POST /api/v1/agent/sync/delta (initial batch {}/{}) -> HTTP {} (token_present: {})",
            batch.batch_index + 1,
            batch.total_batches,
            status,
            has_token
        );

        if !status.is_success() {
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

        Self::check_sync_response_body(status, &body)
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
        let body = resp.text().await.unwrap_or_default();
        log::debug!(
            "[cloud_client] POST /api/v1/agent/sync/delta (records: {}) -> HTTP {} (token_present: {})",
            payload.records.len(),
            status,
            has_token
        );

        if !status.is_success() {
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

        Self::check_sync_response_body(status, &body)
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
            .and(path("/api/v1/agent/device/poll"))
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
        let result = client.poll_device("dev-code").await;
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

        // 1. Initiate device
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

        // 2. Poll device
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

    #[tokio::test]
    async fn sync_push_200_success_true_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/agent/sync/delta"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "alterId": 1234,
                "recordsProcessed": 5
            })))
            .mount(&server)
            .await;

        let client = CloudClient::from_base_url(&server.uri())
            .unwrap()
            .with_token("test-agent-token");

        let payload = DeltaSyncPayload {
            company_name: Some("Acme Corp".into()),
            records: vec![],
            alter_id_high: 1234,
            new_alter_id: "1234".into(),
        };

        let res = client.push_delta(&payload).await;
        assert!(res.is_ok(), "push_delta must succeed on 200 {{\"success\":true}}");
    }

    #[tokio::test]
    async fn sync_push_200_success_false_returns_sync_failed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/agent/sync/delta"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": false,
                "error": "Batch validation failed"
            })))
            .mount(&server)
            .await;

        let client = CloudClient::from_base_url(&server.uri())
            .unwrap()
            .with_token("test-agent-token");

        let payload = DeltaSyncPayload {
            company_name: Some("Acme Corp".into()),
            records: vec![],
            alter_id_high: 1234,
            new_alter_id: "1234".into(),
        };

        let err = client.push_delta(&payload).await.unwrap_err();
        match err {
            ApiError::SyncFailed(code) => assert_eq!(code, 200),
            other => panic!("Expected ApiError::SyncFailed(200), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn sync_push_200_empty_body_treated_as_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/agent/sync/delta"))
            .respond_with(ResponseTemplate::new(200).set_body_string(""))
            .mount(&server)
            .await;

        let client = CloudClient::from_base_url(&server.uri())
            .unwrap()
            .with_token("test-agent-token");

        let payload = DeltaSyncPayload {
            company_name: Some("Acme Corp".into()),
            records: vec![],
            alter_id_high: 1234,
            new_alter_id: "1234".into(),
        };

        let res = client.push_delta(&payload).await;
        assert!(res.is_ok(), "push_delta must treat 200 empty body as success");
    }

    #[tokio::test]
    async fn sync_push_422_failure_returns_sync_failed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/agent/sync/delta"))
            .respond_with(ResponseTemplate::new(422).set_body_string(r#"{"error":"Unprocessable Entity"}"#))
            .mount(&server)
            .await;

        let client = CloudClient::from_base_url(&server.uri())
            .unwrap()
            .with_token("test-agent-token");

        let payload = DeltaSyncPayload {
            company_name: Some("Acme Corp".into()),
            records: vec![],
            alter_id_high: 1234,
            new_alter_id: "1234".into(),
        };

        let err = client.push_delta(&payload).await.unwrap_err();
        match err {
            ApiError::SyncFailed(code) => assert_eq!(code, 422),
            other => panic!("Expected ApiError::SyncFailed(422), got {:?}", other),
        }
    }

    #[test]
    fn sync_payloads_serialize_company_name_and_new_alter_id() {
        let delta_payload = DeltaSyncPayload {
            company_name: Some("Acme Corp".into()),
            records: vec![],
            alter_id_high: 42,
            new_alter_id: "42".into(),
        };
        let delta_val = serde_json::to_value(&delta_payload).unwrap();
        assert_eq!(delta_val["companyName"], "Acme Corp");
        assert_eq!(delta_val["newAlterId"], "42");
        assert_eq!(delta_val["alter_id_high"], 42);

        // Omit companyName if None
        let delta_payload_none = DeltaSyncPayload {
            company_name: None,
            records: vec![],
            alter_id_high: 42,
            new_alter_id: "42".into(),
        };
        let delta_val_none = serde_json::to_value(&delta_payload_none).unwrap();
        assert!(delta_val_none.get("companyName").is_none());
        assert_eq!(delta_val_none["newAlterId"], "42");

        let batch_payload = SyncBatchPayload {
            company_name: Some("Acme Corp".into()),
            batch_index: 0,
            total_batches: 1,
            entity_type: "ledger".into(),
            records: vec![],
            alter_id_high: 100,
            new_alter_id: "100".into(),
        };
        let batch_val = serde_json::to_value(&batch_payload).unwrap();
        assert_eq!(batch_val["companyName"], "Acme Corp");
        assert_eq!(batch_val["newAlterId"], "100");
        assert_eq!(batch_val["alter_id_high"], 100);

        let hb = HeartbeatPayload {
            tally_reachable: true,
            agent_version: "1.0.0".into(),
            last_known_alter_id: 100,
            timestamp: "2026-10-13T10:00:00Z".into(),
            last_successful_sync_at: Some("2026-10-13T09:00:00Z".into()),
        };
        let hb_val = serde_json::to_value(&hb).unwrap();
        assert_eq!(hb_val["timestamp"], "2026-10-13T10:00:00Z");
        assert_eq!(hb_val["lastSuccessfulSyncAt"], "2026-10-13T09:00:00Z");
    }
}
