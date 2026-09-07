//! Sync orchestration — user-triggered only, no background sync timer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::json;

use crate::checkpoint::{Checkpoint, CheckpointStore};
use crate::cloud_client::{CloudClient, DeltaSyncPayload, SyncBatchPayload};
use crate::errors::AgentError;
use crate::redact::redact;
use crate::tally_client::TallyClient;

pub const DEFAULT_BATCH_SIZE: usize = 500;

/// Data quality issue detected during pre-push inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityIssue {
    pub entity_type: &'static str,
    pub alter_id: u64,
    pub issue: String,
}

pub fn check_voucher_quality(v: &crate::tally_schema::VoucherRecord) -> Option<QualityIssue> {
    if v.voucher_number.trim().is_empty() {
        return Some(QualityIssue {
            entity_type: "voucher",
            alter_id: v.alter_id,
            issue: "Missing or empty voucher number".to_string(),
        });
    }
    let date_str = v.date.trim();
    if date_str.is_empty() || date_str == "00000000" || date_str.len() < 8 {
        return Some(QualityIssue {
            entity_type: "voucher",
            alter_id: v.alter_id,
            issue: format!("Invalid voucher date: '{}'", v.date),
        });
    }
    if let Some(amt) = v.amount {
        if amt < 0.0 && (v.voucher_type.eq_ignore_ascii_case("Sales") || v.voucher_type.eq_ignore_ascii_case("Receipt")) {
            return Some(QualityIssue {
                entity_type: "voucher",
                alter_id: v.alter_id,
                issue: format!("Negative amount {} for positive voucher type '{}'", amt, v.voucher_type),
            });
        }
    }
    if v.voucher_type.eq_ignore_ascii_case("Sales") {
        if let Some(ref party) = v.party_name {
            if party.trim().is_empty() {
                return Some(QualityIssue {
                    entity_type: "voucher",
                    alter_id: v.alter_id,
                    issue: "Empty party name for Sales voucher".to_string(),
                });
            }
        }
    }
    None
}

pub fn check_ledger_quality(l: &crate::tally_schema::LedgerRecord) -> Option<QualityIssue> {
    if l.name.trim().is_empty() {
        return Some(QualityIssue {
            entity_type: "ledger",
            alter_id: l.alter_id,
            issue: "Missing or empty ledger name".to_string(),
        });
    }
    None
}

pub struct SyncOrchestrator {
    tally: TallyClient,
    cloud: CloudClient,
    checkpoint_store: CheckpointStore,
    batch_size: usize,
    expected_company: Option<String>,
    sync_in_progress: Arc<AtomicBool>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SyncResult {
    pub success: bool,
    pub records_pushed: usize,
    pub new_alter_id: Option<u64>,
    pub error_message: Option<String>,
}

impl SyncOrchestrator {
    pub fn new(
        tally: TallyClient,
        cloud: CloudClient,
        checkpoint_store: CheckpointStore,
    ) -> Self {
        Self {
            tally,
            cloud,
            checkpoint_store,
            batch_size: DEFAULT_BATCH_SIZE,
            expected_company: None,
            sync_in_progress: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn with_batch_size(mut self, size: usize) -> Self {
        self.batch_size = size.max(1);
        self
    }

    pub fn with_expected_company(mut self, company: Option<String>) -> Self {
        self.expected_company = company;
        self
    }

    pub fn is_sync_in_progress(&self) -> bool {
        self.sync_in_progress.load(Ordering::SeqCst)
    }

    pub fn load_checkpoint(&self) -> Result<Checkpoint, AgentError> {
        Ok(self.checkpoint_store.load()?)
    }

    /// Initial backfill — runs once automatically after pairing.
    pub async fn run_initial_backfill(&self) -> Result<SyncResult, AgentError> {
        if self
            .sync_in_progress
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(AgentError::SyncInProgress);
        }

        let result = self.do_initial_backfill().await;

        self.sync_in_progress.store(false, Ordering::SeqCst);
        result
    }

    async fn do_initial_backfill(&self) -> Result<SyncResult, AgentError> {
        log::info!("Starting initial backfill");

        let company = self.tally.ping().await?;
        if let Some(ref expected) = self.expected_company {
            if !expected.is_empty() && !company.company_name.eq_ignore_ascii_case(expected) {
                log::warn!(
                    "Tally active company '{}' does not match paired company '{}'",
                    company.company_name,
                    expected
                );
                return Err(AgentError::Tally(crate::errors::TallyError::CompanyMismatch {
                    current: company.company_name,
                    expected: expected.clone(),
                }));
            }
        }

        let ledgers = self.tally.export_ledgers().await?;
        let vouchers = self.tally.export_vouchers().await?;

        log::info!(
            "[backfill] Extracted {} ledgers and {} vouchers from Tally",
            ledgers.len(),
            vouchers.len()
        );

        // Guard against false completion: do not advance checkpoint if no accounting data was extracted
        if ledgers.is_empty() && vouchers.is_empty() {
            log::warn!("[backfill] Tally returned 0 ledgers and 0 vouchers. Backfill incomplete.");
            return Ok(SyncResult {
                success: false,
                records_pushed: 0,
                new_alter_id: None,
                error_message: Some(
                    "Tally returned 0 ledgers and 0 vouchers. Please ensure your active company in Tally contains accounting data.".into(),
                ),
            });
        }

        // Data quality pre-checks
        for l in &ledgers {
            if let Some(issue) = check_ledger_quality(l) {
                log::warn!("[Data Quality] Ledger warning: {}", issue.issue);
            }
        }
        for v in &vouchers {
            if let Some(issue) = check_voucher_quality(v) {
                log::warn!("[Data Quality] Voucher warning: {}", issue.issue);
            }
        }

        let mut all_records: Vec<(String, serde_json::Value, u64)> = Vec::new();

        all_records.push((
            "company".into(),
            json!({ "name": company.company_name, "alter_id": company.alter_id }),
            company.alter_id,
        ));

        for l in &ledgers {
            all_records.push((
                "ledger".into(),
                serde_json::to_value(l).map_err(|e| AgentError::Other(e.to_string()))?,
                l.alter_id,
            ));
        }

        for v in &vouchers {
            all_records.push((
                "voucher".into(),
                serde_json::to_value(v).map_err(|e| AgentError::Other(e.to_string()))?,
                v.alter_id,
            ));
        }

        let max_alter_id = all_records.iter().map(|(_, _, id)| *id).max().unwrap_or(0);
        let total = all_records.len();
        let total_batches = total.div_ceil(self.batch_size) as u32;
        let mut pushed = 0usize;

        log::debug!(
            "[backfill] Pushing {} total records (1 company, {} ledgers, {} vouchers, max_alter_id={}) in {} batch(es)",
            total,
            ledgers.len(),
            vouchers.len(),
            max_alter_id,
            total_batches
        );

        for (batch_index, chunk) in all_records.chunks(self.batch_size).enumerate() {
            let entity_type = chunk
                .first()
                .map(|(t, _, _)| t.clone())
                .unwrap_or_else(|| "mixed".into());

            let records: Vec<serde_json::Value> = chunk.iter().map(|(_, v, _)| v.clone()).collect();
            let batch_alter_high = chunk.iter().map(|(_, _, id)| *id).max().unwrap_or(0);

            let batch = SyncBatchPayload {
                batch_index: batch_index as u32,
                total_batches,
                entity_type,
                records,
                alter_id_high: batch_alter_high,
            };

            self.cloud.push_initial_batch(&batch).await?;
            pushed += chunk.len();
        }

        // Checkpoint advances only after final batch acknowledged (all batches returned 200)
        self.checkpoint_store
            .advance_on_ack(max_alter_id, true)?;

        log::info!(
            "[backfill] Initial backfill complete: {} records pushed ({} ledgers, {} vouchers). Checkpoint advanced to alter_id={}",
            pushed,
            ledgers.len(),
            vouchers.len(),
            max_alter_id
        );

        Ok(SyncResult {
            success: true,
            records_pushed: pushed,
            new_alter_id: Some(max_alter_id),
            error_message: None,
        })
    }

    /// On-demand delta sync — triggered explicitly by user (or dashboard poll flag).
    pub async fn run_delta_sync(&self) -> Result<SyncResult, AgentError> {
        if self
            .sync_in_progress
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(AgentError::SyncInProgress);
        }

        let result = self.do_delta_sync().await;
        self.sync_in_progress.store(false, Ordering::SeqCst);
        result
    }

    async fn do_delta_sync(&self) -> Result<SyncResult, AgentError> {
        let checkpoint = self.checkpoint_store.load()?;
        if !checkpoint.backfill_complete {
            return Err(AgentError::BackfillRequired);
        }

        if let Some(ref expected) = self.expected_company {
            let company = self.tally.ping().await?;
            if !expected.is_empty() && !company.company_name.eq_ignore_ascii_case(expected) {
                log::warn!(
                    "Tally active company '{}' does not match paired company '{}'",
                    company.company_name,
                    expected
                );
                return Err(AgentError::Tally(crate::errors::TallyError::CompanyMismatch {
                    current: company.company_name,
                    expected: expected.clone(),
                }));
            }
        }

        let last_known = checkpoint.last_known_alter_id;
        log::info!("Delta sync from ALTERID > {}", last_known);

        let deltas = self.tally.export_delta(last_known).await?;

        if deltas.is_empty() {
            return Ok(SyncResult {
                success: true,
                records_pushed: 0,
                new_alter_id: Some(last_known),
                error_message: None,
            });
        }

        let records: Vec<serde_json::Value> = deltas
            .iter()
            .map(|d| {
                json!({
                    "entity_type": d.entity_type,
                    "alter_id": d.alter_id,
                    "payload": d.payload,
                })
            })
            .collect();

        let alter_id_high = deltas.iter().map(|d| d.alter_id).max().unwrap_or(last_known);

        let payload = DeltaSyncPayload {
            records,
            alter_id_high,
        };

        // Push to backend — checkpoint NOT advanced on failure
        match self.cloud.push_delta(&payload).await {
            Ok(()) => {
                self.checkpoint_store.advance_on_ack(alter_id_high, false)?;
                Ok(SyncResult {
                    success: true,
                    records_pushed: deltas.len(),
                    new_alter_id: Some(alter_id_high),
                    error_message: None,
                })
            }
            Err(crate::errors::ApiError::TokenRevoked) => {
                log::warn!("Delta sync detected revoked token");
                Err(AgentError::Api(crate::errors::ApiError::TokenRevoked))
            }
            Err(e) => {
                log::error!("Delta sync push failed: {}", redact(&e.to_string()));
                Ok(SyncResult {
                    success: false,
                    records_pushed: 0,
                    new_alter_id: None,
                    error_message: Some("Sync failed. Please try again.".into()),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tally_client::{TallyClient, TallyEndpoint};

    #[test]
    fn default_batch_size_is_reasonable() {
        assert_eq!(DEFAULT_BATCH_SIZE, 500);
    }

    #[tokio::test]
    async fn tally_unreachable_produces_error_no_checkpoint_change() {
        let endpoint = TallyEndpoint::new("127.0.0.1", 59999).unwrap();
        let tally = TallyClient::new(endpoint).unwrap();
        let cloud = CloudClient::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(dir.path().join("cp.json"));
        store.save(&Checkpoint::default()).unwrap();

        let orch = SyncOrchestrator::new(tally, cloud, store.clone());
        let cp_before = store.load().unwrap();

        let result = orch.run_delta_sync().await;
        assert!(result.is_err() || result.as_ref().map(|r| !r.success).unwrap_or(true));

        let cp_after = store.load().unwrap();
        assert_eq!(cp_before.last_known_alter_id, cp_after.last_known_alter_id);
    }

    #[test]
    fn test_data_quality_voucher_checks() {
        let valid_vch = crate::tally_schema::VoucherRecord {
            voucher_number: "INV-101".into(),
            voucher_type: "Sales".into(),
            date: "20260401".into(),
            alter_id: 100,
            amount: Some(5000.0),
            party_name: Some("Aarav Textiles".into()),
            party_ledger_name: Some("Aarav Textiles".into()),
        };
        assert_eq!(check_voucher_quality(&valid_vch), None);

        let empty_num_vch = crate::tally_schema::VoucherRecord {
            voucher_number: "   ".into(),
            voucher_type: "Sales".into(),
            date: "20260401".into(),
            alter_id: 101,
            amount: Some(5000.0),
            party_name: Some("Aarav Textiles".into()),
            party_ledger_name: Some("Aarav Textiles".into()),
        };
        assert!(check_voucher_quality(&empty_num_vch).is_some());

        let invalid_date_vch = crate::tally_schema::VoucherRecord {
            voucher_number: "INV-102".into(),
            voucher_type: "Sales".into(),
            date: "00000000".into(),
            alter_id: 102,
            amount: Some(5000.0),
            party_name: Some("Aarav Textiles".into()),
            party_ledger_name: Some("Aarav Textiles".into()),
        };
        assert!(check_voucher_quality(&invalid_date_vch).is_some());

        let negative_sales_vch = crate::tally_schema::VoucherRecord {
            voucher_number: "INV-103".into(),
            voucher_type: "Sales".into(),
            date: "20260401".into(),
            alter_id: 103,
            amount: Some(-5000.0),
            party_name: Some("Aarav Textiles".into()),
            party_ledger_name: Some("Aarav Textiles".into()),
        };
        assert!(check_voucher_quality(&negative_sales_vch).is_some());

        let empty_party_sales_vch = crate::tally_schema::VoucherRecord {
            voucher_number: "INV-104".into(),
            voucher_type: "Sales".into(),
            date: "20260401".into(),
            alter_id: 104,
            amount: Some(5000.0),
            party_name: Some("   ".into()),
            party_ledger_name: Some("   ".into()),
        };
        assert!(check_voucher_quality(&empty_party_sales_vch).is_some());
    }

    #[test]
    fn test_data_quality_ledger_checks() {
        let valid_l = crate::tally_schema::LedgerRecord {
            name: "HDFC Bank".into(),
            parent: "Bank Accounts".into(),
            alter_id: 50,
            opening_balance: Some(10000.0),
        };
        assert_eq!(check_ledger_quality(&valid_l), None);

        let empty_name_l = crate::tally_schema::LedgerRecord {
            name: "  ".into(),
            parent: "Bank Accounts".into(),
            alter_id: 51,
            opening_balance: Some(10000.0),
        };
        assert!(check_ledger_quality(&empty_name_l).is_some());
    }

    #[tokio::test]
    async fn test_company_mismatch_prevents_sync() {
        let server = wiremock::MockServer::start().await;
        let port = server.address().port();
        let tally_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
        <COMPANY><NAME>Different Company Ltd</NAME><ALTERID>50</ALTERID></COMPANY>
        </COLLECTION></DATA></BODY></ENVELOPE>"#;

        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(tally_xml))
            .mount(&server)
            .await;

        let endpoint = TallyEndpoint::new("127.0.0.1", port).unwrap();
        let tally = TallyClient::new(endpoint).unwrap();
        let cloud = CloudClient::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(dir.path().join("cp.json"));
        store
            .save(&Checkpoint {
                backfill_complete: true,
                last_known_alter_id: 10,
                ..Default::default()
            })
            .unwrap();

        let orch = SyncOrchestrator::new(tally, cloud, store)
            .with_expected_company(Some("Acme Corp".into()));

        let result = orch.run_delta_sync().await;
        match result {
            Err(AgentError::Tally(crate::errors::TallyError::CompanyMismatch { current, expected })) => {
                assert_eq!(current, "Different Company Ltd");
                assert_eq!(expected, "Acme Corp");
            }
            other => panic!("Expected CompanyMismatch error, got {:?}", other),
        }
    }
}
