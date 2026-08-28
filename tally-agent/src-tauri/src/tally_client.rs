//! Local Tally XML client — read-only Export envelopes only.

use std::time::Duration;

use reqwest::Client;

use crate::errors::TallyError;
use crate::tally_envelope::{ExportEnvelope, ExportReport};
use crate::tally_schema::{
    adapter_for_xml, CompanyInfo, DeltaRecord, LedgerRecord, VoucherRecord,
};

const DEFAULT_TALLY_PORT: u16 = 9000;
const PING_TIMEOUT_SECS: u64 = 5;
const DEFAULT_EXPORT_TIMEOUT_SECS: u64 = 120;

/// Validated loopback-only Tally endpoint. Cannot be constructed with a remote host.
#[derive(Debug, Clone)]
pub struct TallyEndpoint {
    port: u16,
    url: String,
}

impl TallyEndpoint {
    /// Creates a loopback endpoint or returns an error if host is not 127.0.0.1.
    pub fn new(host: &str, port: u16) -> Result<Self, TallyError> {
        validate_loopback(host)?;
        Ok(Self {
            port,
            url: format!("http://127.0.0.1:{port}"),
        })
    }

    pub fn default_local() -> Result<Self, TallyError> {
        Self::new("127.0.0.1", DEFAULT_TALLY_PORT)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

fn validate_loopback(host: &str) -> Result<(), TallyError> {
    match host {
        "127.0.0.1" | "localhost" | "::1" => Ok(()),
        other => Err(TallyError::NonLoopbackHost(other.to_string())),
    }
}

pub struct TallyClient {
    endpoint: TallyEndpoint,
    http: Client,
    export_timeout: Duration,
}

impl TallyClient {
    pub fn new(endpoint: TallyEndpoint) -> Result<Self, TallyError> {
        let http = Client::builder()
            .timeout(Duration::from_secs(PING_TIMEOUT_SECS))
            .build()
            .map_err(|e| TallyError::Transport(e.to_string()))?;

        Ok(Self {
            endpoint,
            http,
            export_timeout: Duration::from_secs(DEFAULT_EXPORT_TIMEOUT_SECS),
        })
    }

    pub fn with_export_timeout(mut self, secs: u64) -> Self {
        self.export_timeout = Duration::from_secs(secs);
        self
    }

    pub fn endpoint(&self) -> &TallyEndpoint {
        &self.endpoint
    }

    /// Ping Tally for active company name and current ALTERID.
    pub async fn ping(&self) -> Result<CompanyInfo, TallyError> {
        let envelope = ExportEnvelope::ping();
        let xml = self.send_export(&envelope, PING_TIMEOUT_SECS).await?;
        let adapter = adapter_for_xml(&xml);
        adapter.parse_company_info(&xml)
    }

    pub async fn export_ledgers(&self) -> Result<Vec<LedgerRecord>, TallyError> {
        let envelope = ExportEnvelope::build(ExportReport::Ledgers, None);
        let xml = self
            .send_export(&envelope, self.export_timeout.as_secs())
            .await?;
        let adapter = adapter_for_xml(&xml);
        adapter.parse_ledgers(&xml)
    }

    pub async fn export_vouchers(&self) -> Result<Vec<VoucherRecord>, TallyError> {
        let envelope = ExportEnvelope::build(ExportReport::Vouchers, None);
        let xml = self
            .send_export(&envelope, self.export_timeout.as_secs())
            .await?;
        let adapter = adapter_for_xml(&xml);
        adapter.parse_vouchers(&xml)
    }

    pub async fn export_delta(&self, after_alter_id: u64) -> Result<Vec<DeltaRecord>, TallyError> {
        let envelope = ExportEnvelope::build(ExportReport::DeltaCollection, Some(after_alter_id));
        let xml = self
            .send_export(&envelope, self.export_timeout.as_secs())
            .await?;
        let adapter = adapter_for_xml(&xml);
        adapter.parse_delta_records(&xml)
    }

    async fn send_export(
        &self,
        envelope: &ExportEnvelope,
        timeout_secs: u64,
    ) -> Result<String, TallyError> {
        debug_assert_eq!(envelope.direction, crate::tally_envelope::EnvelopeDirection::Export);

        let response = self
            .http
            .post(self.endpoint.url())
            .header("Content-Type", "application/xml")
            .body(envelope.xml.clone())
            .timeout(Duration::from_secs(timeout_secs))
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() {
                    TallyError::ConnectionRefused(self.endpoint.url().to_string())
                } else if e.is_timeout() {
                    TallyError::Timeout(timeout_secs)
                } else {
                    TallyError::Transport(e.to_string())
                }
            })?;

        if !response.status().is_success() {
            return Err(TallyError::HttpError(response.status().as_u16()));
        }

        response
            .text()
            .await
            .map_err(|e| TallyError::Transport(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_loopback_host() {
        let result = TallyEndpoint::new("192.168.1.1", 9000);
        assert!(matches!(result, Err(TallyError::NonLoopbackHost(_))));
    }

    #[test]
    fn accepts_loopback_hosts() {
        assert!(TallyEndpoint::new("127.0.0.1", 9000).is_ok());
        assert!(TallyEndpoint::new("localhost", 9000).is_ok());
    }

    #[test]
    fn client_only_builds_export_envelopes() {
        let env = ExportEnvelope::ping();
        assert!(env.xml.contains("Export"));
        assert!(!env.xml.to_ascii_lowercase().contains("import"));
    }
}
