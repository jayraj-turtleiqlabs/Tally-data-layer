//! Device Authorization Flow (RFC 8628-style) for browser-based login.

use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};

use crate::cloud_client::CloudClient;
use crate::errors::ApiError;
use crate::redact::redact;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceAuthStatus {
    Pending,
    SlowDown,
    Denied,
    Expired,
    Approved { agent_token: String, connection_id: String },
}

#[derive(Debug, Clone)]
pub struct DeviceAuthSession {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub poll_interval_secs: u64,
    pub expires_at: Instant,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct DeviceInitiateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceInitiateResponse {
    #[serde(alias = "deviceCode")]
    pub device_code: String,
    #[serde(alias = "userCode")]
    pub user_code: String,
    #[serde(alias = "verificationUri", alias = "verification_url", alias = "verificationUrl")]
    pub verification_uri: String,
    #[serde(default = "default_expires_in", alias = "expiresIn")]
    pub expires_in: u64,
    #[serde(alias = "poll_interval", alias = "poll_interval_secs", alias = "pollInterval")]
    pub interval: Option<u64>,
}

fn default_expires_in() -> u64 {
    900 // 15 minutes default
}

#[derive(Debug, Clone, Serialize)]
pub struct DevicePollRequest<'a> {
    pub device_code: &'a str,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DevicePollResponse {
    pub status: Option<String>,
    pub error: Option<String>,
    #[serde(alias = "agent_token", alias = "agentToken")]
    pub token: Option<String>,
    #[serde(alias = "connection_id", alias = "connectionId")]
    pub connection_id: Option<String>,
    pub data: Option<serde_json::Value>,
}

impl DevicePollResponse {
    pub fn extract_token(&self) -> Option<String> {
        if let Some(ref t) = self.token {
            if !t.is_empty() {
                return Some(t.clone());
            }
        }
        if let Some(ref d) = self.data {
            if let Some(t) = d.get("agent_token").or_else(|| d.get("token")).or_else(|| d.get("agentToken")) {
                if let Some(s) = t.as_str() {
                    if !s.is_empty() {
                        return Some(s.to_string());
                    }
                }
            }
        }
        None
    }

    pub fn extract_connection_id(&self) -> String {
        if let Some(ref cid) = self.connection_id {
            if !cid.is_empty() {
                return cid.clone();
            }
        }
        if let Some(ref d) = self.data {
            if let Some(cid) = d.get("connection_id").or_else(|| d.get("connectionId")) {
                if let Some(s) = cid.as_str() {
                    return s.to_string();
                }
            }
        }
        String::new()
    }
}

/// Request a new device authorization session from the cloud backend.
pub async fn initiate_device_auth(cloud_client: &CloudClient) -> Result<DeviceAuthSession, ApiError> {
    let hostname = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok();

    let request = DeviceInitiateRequest {
        label: hostname.clone(),
        hostname,
    };

    let resp = cloud_client.initiate_device(&request).await?;

    let poll_interval_secs = resp.interval.unwrap_or(5).max(1);
    let expires_in_secs = if resp.expires_in == 0 { 900 } else { resp.expires_in };
    let expires_at = Instant::now() + Duration::from_secs(expires_in_secs);

    Ok(DeviceAuthSession {
        device_code: resp.device_code,
        user_code: resp.user_code,
        verification_uri: resp.verification_uri,
        poll_interval_secs,
        expires_at,
    })
}

/// Poll the device authorization status once.
pub async fn poll_device_auth(
    cloud_client: &CloudClient,
    device_code: &str,
) -> Result<DeviceAuthStatus, ApiError> {
    let resp = cloud_client.poll_device(device_code).await?;

    // Check status string or error string
    let status_str = resp
        .status
        .as_deref()
        .or(resp.error.as_deref())
        .unwrap_or("")
        .to_ascii_lowercase();

    match status_str.as_str() {
        "pending" | "authorization_pending" => Ok(DeviceAuthStatus::Pending),
        "slow_down" | "slowdown" => Ok(DeviceAuthStatus::SlowDown),
        "denied" | "access_denied" => Ok(DeviceAuthStatus::Denied),
        "expired" | "expired_token" => Ok(DeviceAuthStatus::Expired),
        "approved" => {
            if let Some(token) = resp.extract_token() {
                Ok(DeviceAuthStatus::Approved {
                    agent_token: token,
                    connection_id: resp.extract_connection_id(),
                })
            } else {
                Err(ApiError::InvalidResponse(
                    "Approved response missing agent_token".into(),
                ))
            }
        }
        _ => {
            // If token is present, consider approved
            if let Some(token) = resp.extract_token() {
                Ok(DeviceAuthStatus::Approved {
                    agent_token: token,
                    connection_id: resp.extract_connection_id(),
                })
            } else if status_str.is_empty() {
                Ok(DeviceAuthStatus::Pending)
            } else {
                Err(ApiError::InvalidResponse(format!(
                    "Unknown device auth status: {}",
                    redact(&status_str)
                )))
            }
        }
    }
}

/// Polls repeatedly until the session is approved, denied, or expired.
/// Respects `session.expires_at` as a hard ceiling.
/// On `SlowDown`, increases the polling interval by 5 seconds.
pub async fn poll_until_complete(
    cloud_client: &CloudClient,
    session: &DeviceAuthSession,
) -> Result<DeviceAuthStatus, ApiError> {
    let mut current_interval = session.poll_interval_secs;

    loop {
        if Instant::now() >= session.expires_at {
            log::warn!("Device authorization session reached local expiration ceiling");
            return Ok(DeviceAuthStatus::Expired);
        }

        tokio::time::sleep(Duration::from_secs(current_interval)).await;

        if Instant::now() >= session.expires_at {
            log::warn!("Device authorization session expired while waiting to poll");
            return Ok(DeviceAuthStatus::Expired);
        }

        match poll_device_auth(cloud_client, &session.device_code).await {
            Ok(DeviceAuthStatus::Pending) => {
                log::debug!("Device auth status: Pending");
                continue;
            }
            Ok(DeviceAuthStatus::SlowDown) => {
                current_interval = current_interval.saturating_add(5);
                log::info!(
                    "Device auth SlowDown received: increased poll interval to {}s",
                    current_interval
                );
                continue;
            }
            Ok(DeviceAuthStatus::Approved { agent_token, connection_id }) => {
                log::info!("Device auth Approved");
                return Ok(DeviceAuthStatus::Approved {
                    agent_token,
                    connection_id,
                });
            }
            Ok(DeviceAuthStatus::Denied) => {
                log::warn!("Device auth Denied by user");
                return Ok(DeviceAuthStatus::Denied);
            }
            Ok(DeviceAuthStatus::Expired) => {
                log::warn!("Device auth Expired on backend");
                return Ok(DeviceAuthStatus::Expired);
            }
            Err(e) => {
                log::error!("Device auth poll network/API error: {}", redact(&e.to_string()));
                return Err(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_initiate_device_auth_success() {
        let server = MockServer::start().await;
        let response_body = serde_json::json!({
            "device_code": "dev-12345",
            "user_code": "WDJB-4K9P",
            "verification_uri": "https://app.fininsight.io/device",
            "expires_in": 300,
            "interval": 5
        });

        Mock::given(method("POST"))
            .and(path("/api/v1/agent/device/initiate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
            .mount(&server)
            .await;

        let client = CloudClient::from_base_url(&server.uri()).unwrap();
        let session = initiate_device_auth(&client).await.unwrap();

        assert_eq!(session.device_code, "dev-12345");
        assert_eq!(session.user_code, "WDJB-4K9P");
        assert_eq!(session.verification_uri, "https://app.fininsight.io/device");
        assert_eq!(session.poll_interval_secs, 5);
        assert!(session.expires_at > Instant::now());
    }

    #[tokio::test]
    async fn test_poll_pending_to_approved() {
        let server = MockServer::start().await;

        struct PendingThenApprove {
            count: std::sync::atomic::AtomicUsize,
        }

        impl wiremock::Respond for PendingThenApprove {
            fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
                let prev = self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if prev == 0 {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "status": "pending"
                    }))
                } else {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "status": "approved",
                        "agent_token": "secret-agent-token-xyz",
                        "connection_id": "conn-999"
                    }))
                }
            }
        }

        Mock::given(method("POST"))
            .and(path("/api/v1/agent/device/poll"))
            .respond_with(PendingThenApprove {
                count: std::sync::atomic::AtomicUsize::new(0),
            })
            .mount(&server)
            .await;

        let client = CloudClient::from_base_url(&server.uri()).unwrap();
        let session = DeviceAuthSession {
            device_code: "dev-code-1".into(),
            user_code: "USER-1".into(),
            verification_uri: "http://example.com/device".into(),
            poll_interval_secs: 1,
            expires_at: Instant::now() + Duration::from_secs(10),
        };

        let result = poll_until_complete(&client, &session).await.unwrap();
        match result {
            DeviceAuthStatus::Approved { agent_token, connection_id } => {
                assert_eq!(agent_token, "secret-agent-token-xyz");
                assert_eq!(connection_id, "conn-999");
            }
            other => panic!("Expected Approved, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_poll_pending_to_denied() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/v1/agent/device/poll"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "denied"
            })))
            .mount(&server)
            .await;

        let client = CloudClient::from_base_url(&server.uri()).unwrap();
        let session = DeviceAuthSession {
            device_code: "dev-code-2".into(),
            user_code: "USER-2".into(),
            verification_uri: "http://example.com/device".into(),
            poll_interval_secs: 1,
            expires_at: Instant::now() + Duration::from_secs(10),
        };

        let result = poll_until_complete(&client, &session).await.unwrap();
        assert_eq!(result, DeviceAuthStatus::Denied);
    }

    #[tokio::test]
    async fn test_poll_pending_to_expired() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/v1/agent/device/poll"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "expired"
            })))
            .mount(&server)
            .await;

        let client = CloudClient::from_base_url(&server.uri()).unwrap();
        let session = DeviceAuthSession {
            device_code: "dev-code-3".into(),
            user_code: "USER-3".into(),
            verification_uri: "http://example.com/device".into(),
            poll_interval_secs: 1,
            expires_at: Instant::now() + Duration::from_secs(10),
        };

        let result = poll_until_complete(&client, &session).await.unwrap();
        assert_eq!(result, DeviceAuthStatus::Expired);
    }

    #[tokio::test]
    async fn test_poll_local_session_expiry_ceiling() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/v1/agent/device/poll"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "pending"
            })))
            .mount(&server)
            .await;

        let client = CloudClient::from_base_url(&server.uri()).unwrap();
        // Session that expires in 0 seconds (already expired)
        let session = DeviceAuthSession {
            device_code: "dev-code-4".into(),
            user_code: "USER-4".into(),
            verification_uri: "http://example.com/device".into(),
            poll_interval_secs: 1,
            expires_at: Instant::now(),
        };

        let result = poll_until_complete(&client, &session).await.unwrap();
        assert_eq!(result, DeviceAuthStatus::Expired);
    }

    #[tokio::test]
    async fn test_poll_slowdown_backs_off_interval() {
        let server = MockServer::start().await;

        struct SlowDownThenApprove {
            count: std::sync::atomic::AtomicUsize,
        }

        impl wiremock::Respond for SlowDownThenApprove {
            fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
                let prev = self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if prev == 0 {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "status": "slow_down"
                    }))
                } else {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "status": "approved",
                        "agent_token": "token-after-slowdown",
                        "connection_id": "conn-1"
                    }))
                }
            }
        }

        Mock::given(method("POST"))
            .and(path("/api/v1/agent/device/poll"))
            .respond_with(SlowDownThenApprove {
                count: std::sync::atomic::AtomicUsize::new(0),
            })
            .mount(&server)
            .await;

        let client = CloudClient::from_base_url(&server.uri()).unwrap();
        let session = DeviceAuthSession {
            device_code: "dev-code-5".into(),
            user_code: "USER-5".into(),
            verification_uri: "http://example.com/device".into(),
            poll_interval_secs: 1,
            expires_at: Instant::now() + Duration::from_secs(15),
        };

        let start = Instant::now();
        let result = poll_until_complete(&client, &session).await.unwrap();
        let elapsed = start.elapsed();

        // 1st sleep = 1s (got SlowDown -> interval becomes 1 + 5 = 6s)
        // 2nd sleep = 6s (got Approved)
        // Total elapsed should be >= 7s
        assert!(
            elapsed >= Duration::from_millis(6500),
            "Expected backoff delay of at least 6.5s, got {:?}",
            elapsed
        );

        match result {
            DeviceAuthStatus::Approved { agent_token, .. } => {
                assert_eq!(agent_token, "token-after-slowdown");
            }
            other => panic!("Expected Approved, got {:?}", other),
        }
    }
}
