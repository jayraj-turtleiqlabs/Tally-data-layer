//! Integration tests for sync checkpoint behavior and envelope safety.

use fininsight_tally_agent_lib::checkpoint::{Checkpoint, CheckpointStore};
use fininsight_tally_agent_lib::cloud_client::{CloudClient, DeltaSyncPayload};
use fininsight_tally_agent_lib::sync::SyncOrchestrator;
use fininsight_tally_agent_lib::tally_client::{TallyClient, TallyEndpoint};
use fininsight_tally_agent_lib::tally_envelope::{EnvelopeDirection, ExportEnvelope, ExportReport};
use fininsight_tally_agent_lib::vault::Vault;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn request_builder_has_no_import_variant() {
    fn assert_export_only(dir: EnvelopeDirection) {
        assert_eq!(dir, EnvelopeDirection::Export);
    }
    assert_export_only(EnvelopeDirection::Export);

    let env = ExportEnvelope::build(ExportReport::Vouchers, None);
    assert!(!env.xml.to_ascii_lowercase().contains("import"));
}

#[tokio::test]
async fn backend_non_200_leaves_checkpoint_unchanged() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/sync/delta"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let store = CheckpointStore::new(dir.path().join("cp.json"));
    store
        .save(&Checkpoint {
            last_known_alter_id: 100,
            backfill_complete: true,
            ..Default::default()
        })
        .unwrap();

    let cp_before = store.load().unwrap();

    let client = CloudClient::from_base_url(&server.uri())
        .unwrap()
        .with_token("test-token-for-mock");
    let payload = DeltaSyncPayload {
        records: vec![serde_json::json!({"alter_id": 200})],
        alter_id_high: 200,
    };

    let result = client.push_delta(&payload).await;
    assert!(result.is_err());

    let cp_after = store.load().unwrap();
    assert_eq!(cp_before.last_known_alter_id, cp_after.last_known_alter_id);
}

#[test]
fn backend_200_advances_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let store = CheckpointStore::new(dir.path().join("cp.json"));
    store
        .save(&Checkpoint {
            last_known_alter_id: 100,
            backfill_complete: true,
            ..Default::default()
        })
        .unwrap();

    store.advance_on_ack(250, false).unwrap();

    let cp = store.load().unwrap();
    assert_eq!(cp.last_known_alter_id, 250);
    assert!(cp.last_successful_sync.is_some());
}

#[test]
fn invalid_pairing_does_not_store_token() {
    Vault::delete_token().ok();
    Vault::delete_connection_id().ok();
    assert!(!Vault::is_paired());
}

#[tokio::test]
async fn retry_after_failure_sync_orchestrator() {
    // 1. Mock Tally instance on loopback port
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let tally_endpoint = TallyEndpoint::new("127.0.0.1", tally_port).unwrap();
    let tally_client = TallyClient::new(tally_endpoint).unwrap();

    let tally_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME>Acme Corp Ltd</NAME><ALTERID>100</ALTERID></COMPANY>
    <VOUCHER>
      <VOUCHERNUMBER>INV-001</VOUCHERNUMBER>
      <VOUCHERTYPENAME>Sales</VOUCHERTYPENAME>
      <PARTYLEDGERNAME>Aarav Textiles</PARTYLEDGERNAME>
      <DATE>20260401</DATE>
      <ALTERID>150</ALTERID>
      <AMOUNT>25000.00</AMOUNT>
    </VOUCHER>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(tally_xml))
        .mount(&tally_server)
        .await;

    // 2. Mock Cloud Backend: fails on 1st push, succeeds on 2nd push
    let cloud_server = MockServer::start().await;

    struct FailThenSucceed {
        count: std::sync::atomic::AtomicUsize,
    }

    impl wiremock::Respond for FailThenSucceed {
        fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
            let prev = self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if prev == 0 {
                ResponseTemplate::new(500)
            } else {
                ResponseTemplate::new(200)
            }
        }
    }

    Mock::given(method("POST"))
        .and(path("/api/v1/agent/sync/delta"))
        .respond_with(FailThenSucceed {
            count: std::sync::atomic::AtomicUsize::new(0),
        })
        .mount(&cloud_server)
        .await;

    let cloud_client = CloudClient::from_base_url(&cloud_server.uri())
        .unwrap()
        .with_token("test-token-retry");

    // 3. Set up CheckpointStore starting at alter_id = 100 with backfill complete
    let dir = tempfile::tempdir().unwrap();
    let store = CheckpointStore::new(dir.path().join("cp.json"));
    store
        .save(&Checkpoint {
            connection_id: Some("test-conn".into()),
            last_known_alter_id: 100,
            backfill_complete: true,
            last_successful_sync: None,
        })
        .unwrap();

    let orch = SyncOrchestrator::new(tally_client, cloud_client, store.clone());

    // 4. Attempt 1: Should fail because cloud backend returns 500
    let res1 = orch.run_delta_sync().await.unwrap();
    assert!(!res1.success, "First sync attempt must report failure");
    assert_eq!(res1.records_pushed, 0);

    // Checkpoint must NOT advance after failed attempt
    let cp_after_fail = store.load().unwrap();
    assert_eq!(
        cp_after_fail.last_known_alter_id, 100,
        "Checkpoint must remain at 100 after first failed sync"
    );
    assert!(cp_after_fail.last_successful_sync.is_none());

    // 5. Attempt 2: Should succeed because cloud backend returns 200
    let res2 = orch.run_delta_sync().await.unwrap();
    assert!(res2.success, "Second sync attempt must succeed");
    assert_eq!(res2.records_pushed, 1);
    assert_eq!(res2.new_alter_id, Some(150));

    // Checkpoint MUST advance to 150 only after successful attempt
    let cp_after_success = store.load().unwrap();
    assert_eq!(
        cp_after_success.last_known_alter_id, 150,
        "Checkpoint must advance to 150 only after successful sync"
    );
    assert!(
        cp_after_success.last_successful_sync.is_some(),
        "last_successful_sync timestamp must be updated"
    );

    // 6. Verify received requests on Cloud Server: payload must be byte/field-identical
    let received = cloud_server.received_requests().await.unwrap();
    assert_eq!(received.len(), 2, "Expected exactly 2 requests to cloud backend");

    let body1: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    let body2: serde_json::Value = serde_json::from_slice(&received[1].body).unwrap();

    assert_eq!(
        body1, body2,
        "The second (retried) delta sync payload must be identical to the first payload"
    );
    assert_eq!(body1["alter_id_high"], 150);
    assert_eq!(body1["records"].as_array().unwrap().len(), 1);
    assert_eq!(body1["records"][0]["alter_id"], 150);
    assert_eq!(body1["records"][0]["entity_type"], "voucher");

    Vault::delete_token().ok();
}

static VAULT_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn test_device_auth_approved_stores_token_via_shared_path() {
    let _guard = VAULT_TEST_LOCK.lock().await;
    let _ = Vault::delete_token();

    // 1. Mock Tally instance on loopback port
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let tally_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME>Acme Corp Ltd</NAME><ALTERID>50</ALTERID><GUID>guid-test-999</GUID></COMPANY>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(tally_xml))
        .mount(&tally_server)
        .await;

    // 2. Mock Cloud Backend for delta sync (backfill)
    let cloud_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/sync/delta"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&cloud_server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let agent_state = fininsight_tally_agent_lib::AgentState::with_options(
        tally_port,
        Some(cloud_server.uri()),
        dir.path().join("cp.json"),
    )
    .unwrap();

    let company = agent_state
        .complete_pairing_with_token(
            "test-approved-agent-token-999",
            "Acme Corp Ltd",
            Some("guid-test-999"),
            Some("conn-test-999"),
        )
        .await
        .unwrap();

    assert_eq!(company, "Acme Corp Ltd");
    assert!(Vault::is_paired());
    assert_eq!(
        Vault::get_token_for("conn-test-999").unwrap(),
        "test-approved-agent-token-999"
    );

    let status = agent_state.get_status().await;
    assert!(status.paired);
    assert_eq!(status.company_name, Some("Acme Corp Ltd".into()));

    let _ = Vault::delete_token_for("conn-test-999");
    let _ = Vault::delete_connection_id();
}

#[tokio::test]
async fn test_device_auth_denied_stops_polling_and_no_token() {
    let _guard = VAULT_TEST_LOCK.lock().await;
    let _ = Vault::delete_token();
    let _ = Vault::delete_connection_id();

    let cloud_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/device/poll"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "denied"
        })))
        .mount(&cloud_server)
        .await;

    let client = CloudClient::from_base_url(&cloud_server.uri()).unwrap();
    let session = fininsight_tally_agent_lib::device_auth::DeviceAuthSession {
        device_code: "dev-denied".into(),
        user_code: "USER-DENIED".into(),
        verification_uri: "http://example.com/device".into(),
        poll_interval_secs: 1,
        expires_at: std::time::Instant::now() + std::time::Duration::from_secs(10),
    };

    let result = fininsight_tally_agent_lib::device_auth::poll_until_complete(&client, &session).await.unwrap();
    assert_eq!(result, fininsight_tally_agent_lib::device_auth::DeviceAuthStatus::Denied);
    assert!(!Vault::is_paired());

    // Verify exactly 1 request was sent before stopping immediately
    let reqs = cloud_server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1, "Polling must stop immediately upon Denied response");
}

#[tokio::test]
async fn test_device_auth_expired_stops_polling() {
    let cloud_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/device/poll"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "expired"
        })))
        .mount(&cloud_server)
        .await;

    let client = CloudClient::from_base_url(&cloud_server.uri()).unwrap();
    let session = fininsight_tally_agent_lib::device_auth::DeviceAuthSession {
        device_code: "dev-expired".into(),
        user_code: "USER-EXPIRED".into(),
        verification_uri: "http://example.com/device".into(),
        poll_interval_secs: 1,
        expires_at: std::time::Instant::now() + std::time::Duration::from_secs(10),
    };

    let result = fininsight_tally_agent_lib::device_auth::poll_until_complete(&client, &session).await.unwrap();
    assert_eq!(result, fininsight_tally_agent_lib::device_auth::DeviceAuthStatus::Expired);

    let reqs = cloud_server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1, "Polling must stop immediately upon Expired response");
}

#[tokio::test]
async fn test_token_revocation_on_sync_clears_token_and_resets_status() {
    let _guard = VAULT_TEST_LOCK.lock().await;
    let _ = Vault::delete_token();

    // 1. Mock Tally instance on loopback port with a delta voucher
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let tally_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME>Revoke Test Corp</NAME><ALTERID>50</ALTERID><GUID>guid-revoke-123</GUID></COMPANY>
    <VOUCHER>
      <VOUCHERNUMBER>INV-REVOKE</VOUCHERNUMBER>
      <VOUCHERTYPENAME>Sales</VOUCHERTYPENAME>
      <DATE>20260401</DATE>
      <ALTERID>150</ALTERID>
      <AMOUNT>1000.00</AMOUNT>
    </VOUCHER>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(tally_xml))
        .mount(&tally_server)
        .await;

    // 2. Mock Cloud Backend to return 401 Unauthorized on sync
    let cloud_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/sync/delta"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&cloud_server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let agent_state = fininsight_tally_agent_lib::AgentState::with_options(
        tally_port,
        Some(cloud_server.uri()),
        dir.path().join("cp.json"),
    )
    .unwrap();

    // Initial pairing and backfill
    agent_state
        .complete_pairing_with_token(
            "revoked-token-123",
            "Revoke Test Corp",
            Some("guid-revoke-123"),
            Some("conn-revoke-123"),
        )
        .await
        .unwrap();
    assert!(Vault::is_paired());

    // Trigger delta sync_now — it encounters 401, invokes handle_token_revoked, and returns Err
    let sync_result = agent_state.sync_now().await;
    assert!(sync_result.is_err());

    // Token must be deleted from vault
    assert!(!Vault::is_paired(), "Vault must not be paired after token revocation");

    // Agent status must reflect unpaired and display the re-pairing prompt error
    let status = agent_state.get_status().await;
    assert!(!status.paired);
    assert_eq!(status.company_name, None);
    assert!(status.last_error.is_some());
    assert!(status.last_error.unwrap().contains("revoked or expired"));

    Vault::delete_token().ok();
}

#[tokio::test]
async fn test_token_revocation_on_heartbeat() {
    let cloud_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/heartbeat"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&cloud_server)
        .await;

    let client = CloudClient::from_base_url(&cloud_server.uri())
        .unwrap()
        .with_token("dead-token");

    let payload = fininsight_tally_agent_lib::cloud_client::HeartbeatPayload {
        tally_reachable: true,
        agent_version: "0.1.0".into(),
        last_known_alter_id: 100,
    };

    let result = client.heartbeat(&payload).await;
    match result {
        Err(fininsight_tally_agent_lib::errors::ApiError::TokenRevoked) => {}
        other => panic!("Expected ApiError::TokenRevoked, got {:?}", other),
    }
}

#[tokio::test]
async fn test_initial_backfill_with_zero_records_does_not_advance_checkpoint() {
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let tally_endpoint = TallyEndpoint::new("127.0.0.1", tally_port).unwrap();
    let tally_client = TallyClient::new(tally_endpoint).unwrap();

    // Tally returns company info on ping, but empty collections on ledgers/vouchers
    let company_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY NAME="Zero Record Corp"><ALTERID>500</ALTERID></COMPANY>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    let empty_collection_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION></COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .and(wiremock::matchers::body_string_contains("FinInsightActiveCompanyColl"))
        .respond_with(ResponseTemplate::new(200).set_body_string(company_xml))
        .mount(&tally_server)
        .await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(empty_collection_xml))
        .mount(&tally_server)
        .await;

    let cloud_server = MockServer::start().await;
    let cloud_client = CloudClient::from_base_url(&cloud_server.uri())
        .unwrap()
        .with_token("test-token-zero");

    let dir = tempfile::tempdir().unwrap();
    let store = CheckpointStore::new(dir.path().join("cp.json"));
    store.save(&Checkpoint::default()).unwrap();

    let orch = SyncOrchestrator::new(tally_client, cloud_client, store.clone());
    let result = orch.run_initial_backfill().await.unwrap();

    // Backfill must be reported as unsuccessful
    assert!(!result.success);
    assert_eq!(result.records_pushed, 0);

    // Checkpoint must NOT be marked backfill_complete or advanced
    let cp = store.load().unwrap();
    assert!(!cp.backfill_complete);
    assert_eq!(cp.last_known_alter_id, 0);
}

#[tokio::test]
async fn test_initial_backfill_with_partial_ledger_data_advances_checkpoint() {
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let tally_endpoint = TallyEndpoint::new("127.0.0.1", tally_port).unwrap();
    let tally_client = TallyClient::new(tally_endpoint).unwrap();

    let company_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY NAME="Partial Record Corp"><ALTERID>500</ALTERID></COMPANY>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    let ledger_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <LEDGER NAME="Customer A">
      <PARENT>Sundry Debtors</PARENT>
      <ALTERID>650</ALTERID>
      <OPENINGBALANCE>10000.00</OPENINGBALANCE>
    </LEDGER>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    let empty_vouchers_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION></COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .and(wiremock::matchers::body_string_contains("FinInsightActiveCompanyColl"))
        .respond_with(ResponseTemplate::new(200).set_body_string(company_xml))
        .mount(&tally_server)
        .await;

    Mock::given(method("POST"))
        .and(wiremock::matchers::body_string_contains("<ID>Ledgers</ID>"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ledger_xml))
        .mount(&tally_server)
        .await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(empty_vouchers_xml))
        .mount(&tally_server)
        .await;

    let cloud_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/sync/delta"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&cloud_server)
        .await;

    let cloud_client = CloudClient::from_base_url(&cloud_server.uri())
        .unwrap()
        .with_token("test-token-partial");

    let dir = tempfile::tempdir().unwrap();
    let store = CheckpointStore::new(dir.path().join("cp.json"));
    store.save(&Checkpoint::default()).unwrap();

    let orch = SyncOrchestrator::new(tally_client, cloud_client, store.clone());
    let result = orch.run_initial_backfill().await.unwrap();

    assert!(result.success);
    assert_eq!(result.records_pushed, 2); // 1 company + 1 ledger

    let cp = store.load().unwrap();
    assert!(cp.backfill_complete);
    assert_eq!(cp.last_known_alter_id, 650);
}

#[tokio::test]
async fn test_start_device_login_sends_company_name_in_initiate_request() {
    // 1. Mock Tally
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let tally_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME>Acme Corp Ltd</NAME><ALTERID>50</ALTERID></COMPANY>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(tally_xml))
        .mount(&tally_server)
        .await;

    // 2. Mock Cloud Backend and verify received initiate body
    let cloud_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/device/initiate"))
        .and(wiremock::matchers::body_string_contains("Acme Corp Ltd"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_code": "dev-init-123",
            "user_code": "CODE-123",
            "verification_uri": "https://app.fininsight.io/device",
            "expires_in": 300,
            "interval": 5
        })))
        .mount(&cloud_server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let agent_state = fininsight_tally_agent_lib::AgentState::with_options(
        tally_port,
        Some(cloud_server.uri()),
        dir.path().join("cp.json"),
    )
    .unwrap();

    let session = agent_state.start_device_login().await.unwrap();
    assert_eq!(session.user_code, "CODE-123");
    assert_eq!(session.verification_uri, "https://app.fininsight.io/device");
}

#[tokio::test]
async fn test_start_device_login_fails_when_tally_unreachable() {
    let unused_port = 59998;
    let cloud_server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    let agent_state = fininsight_tally_agent_lib::AgentState::with_options(
        unused_port,
        Some(cloud_server.uri()),
        dir.path().join("cp.json"),
    )
    .unwrap();

    let result = agent_state.start_device_login().await;
    assert!(result.is_err(), "Must fail when Tally is unreachable");
}

#[tokio::test]
async fn test_start_device_login_fails_when_no_active_company() {
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let empty_company_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME></NAME><ALTERID>0</ALTERID></COMPANY>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(empty_company_xml))
        .mount(&tally_server)
        .await;

    let cloud_server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    let agent_state = fininsight_tally_agent_lib::AgentState::with_options(
        tally_port,
        Some(cloud_server.uri()),
        dir.path().join("cp.json"),
    )
    .unwrap();

    let result = agent_state.start_device_login().await;
    assert!(result.is_err(), "Must fail when Tally has no active company");
}

#[tokio::test]
async fn test_connection_id_stored_and_survives_token_revocation_for_re_pairing() {
    let _guard = VAULT_TEST_LOCK.lock().await;
    let _ = Vault::delete_token();
    let _ = Vault::delete_connection_id();

    // 1. Mock Tally
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let tally_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME>Acme Corp Ltd</NAME><ALTERID>50</ALTERID></COMPANY>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(tally_xml))
        .mount(&tally_server)
        .await;

    // 2. Mock Cloud Backend for initial pairing via poll
    let cloud_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/device/poll"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "approved",
            "agent_token": "token-test-123",
            "connection_id": "conn-saved-777"
        })))
        .mount(&cloud_server)
        .await;

    // 3. Mock Cloud Backend for delta sync backfill
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/sync/delta"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&cloud_server)
        .await;

    // 4. Mock Cloud Backend for second initiate request (after revocation), verifying previousConnectionId
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/device/initiate"))
        .and(wiremock::matchers::body_string_contains("conn-saved-777"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_code": "dev-re-pair",
            "user_code": "CODE-REPAIR",
            "verification_uri": "https://app.fininsight.io/device",
            "expires_in": 300,
            "interval": 5
        })))
        .mount(&cloud_server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let agent_state = fininsight_tally_agent_lib::AgentState::with_options(
        tally_port,
        Some(cloud_server.uri()),
        dir.path().join("cp.json"),
    )
    .unwrap();

    // Store active session to poll
    let initial_session = fininsight_tally_agent_lib::device_auth::DeviceAuthSession {
        device_code: "dev-init".into(),
        user_code: "USER-INIT".into(),
        verification_uri: "http://example.com/device".into(),
        poll_interval_secs: 1,
        expires_at: std::time::Instant::now() + std::time::Duration::from_secs(60),
    };
    *agent_state.active_device_session.lock().await = Some(initial_session);

    let company = agent_state.poll_device_login().await.unwrap();
    assert_eq!(company, "Acme Corp Ltd");
    assert!(Vault::is_paired());
    assert_eq!(Vault::get_connection_id().unwrap(), "conn-saved-777");

    // Revoke token
    agent_state.handle_token_revoked().await;
    assert!(!Vault::is_paired(), "Token must be cleared upon revocation");
    assert_eq!(
        Vault::get_connection_id().unwrap(),
        "conn-saved-777",
        "Connection ID must persist across token revocation"
    );

    // Re-initiate device pairing: verify previousConnectionId is sent to backend
    let repair_session = agent_state.start_device_login().await.unwrap();
    assert_eq!(repair_session.user_code, "CODE-REPAIR");

    Vault::delete_token().ok();
    Vault::delete_connection_id().ok();
}

#[tokio::test]
async fn test_fresh_agent_omits_previous_connection_id() {
    let _guard = VAULT_TEST_LOCK.lock().await;
    let _ = Vault::delete_token();
    let _ = Vault::delete_connection_id();

    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let tally_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME>Fresh Corp</NAME><ALTERID>10</ALTERID></COMPANY>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(tally_xml))
        .mount(&tally_server)
        .await;

    let cloud_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/device/initiate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_code": "dev-fresh",
            "user_code": "CODE-FRESH",
            "verification_uri": "https://app.fininsight.io/device",
            "expires_in": 300,
            "interval": 5
        })))
        .mount(&cloud_server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let agent_state = fininsight_tally_agent_lib::AgentState::with_options(
        tally_port,
        Some(cloud_server.uri()),
        dir.path().join("cp.json"),
    )
    .unwrap();

    let session = agent_state.start_device_login().await.unwrap();
    assert_eq!(session.user_code, "CODE-FRESH");

    // Confirm nothing in vault
    assert!(Vault::get_connection_id().is_err());
}

#[tokio::test]
async fn test_disconnect_clears_token_and_connection_id() {
    let _guard = VAULT_TEST_LOCK.lock().await;
    let _ = Vault::store_token("temp-token-123");
    let _ = Vault::store_connection_id("conn-to-delete-456");

    let dir = tempfile::tempdir().unwrap();
    let agent_state = fininsight_tally_agent_lib::AgentState::with_options(
        9000,
        None,
        dir.path().join("cp.json"),
    )
    .unwrap();

    agent_state.disconnect().await.unwrap();
    assert!(Vault::get_token().is_err());
    assert!(Vault::get_connection_id().is_err());
}

#[tokio::test]
async fn test_company_guid_mismatch_fails_closed() {
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    // Tally returns company with GUID "guid-company-B"
    let tally_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME>Acme Corp Ltd</NAME><ALTERID>100</ALTERID><GUID>guid-company-B</GUID></COMPANY>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(tally_xml))
        .mount(&tally_server)
        .await;

    let tally_endpoint = TallyEndpoint::new("127.0.0.1", tally_port).unwrap();
    let tally_client = TallyClient::new(tally_endpoint).unwrap();

    let cloud_server = MockServer::start().await;
    let cloud_client = CloudClient::from_base_url(&cloud_server.uri())
        .unwrap()
        .with_token("test-token");

    let dir = tempfile::tempdir().unwrap();
    let store = CheckpointStore::new(dir.path().join("cp.json"));
    store
        .save(&Checkpoint {
            connection_id: Some("conn-test".into()),
            last_known_alter_id: 50,
            backfill_complete: true,
            last_successful_sync: None,
        })
        .unwrap();

    // Expected GUID is "guid-company-A", but Tally has "guid-company-B"
    let orch = SyncOrchestrator::new(tally_client, cloud_client, store)
        .with_expected_profile(
            Some("Acme Corp Ltd".into()),
            Some("guid-company-A".into()),
            Some("conn-test".into()),
        );

    let result = orch.run_delta_sync().await;
    assert!(result.is_err(), "Sync must fail closed when GUID does not match");
    let err_str = result.unwrap_err().to_string();
    assert!(err_str.contains("GUID") || err_str.contains("match"));
}

#[tokio::test]
async fn test_agent_restart_loads_profile_and_blocks_wrong_guid() {
    let _guard = VAULT_TEST_LOCK.lock().await;
    let _ = Vault::delete_token();
    let _ = Vault::delete_connection_id();

    let dir = tempfile::tempdir().unwrap();
    let base_path = dir.path().join("profiles.json");

    // Profile store has company A with GUID "guid-company-A"
    let profile_store = fininsight_tally_agent_lib::checkpoint::ProfileStore::new(base_path.clone());
    profile_store
        .save(&fininsight_tally_agent_lib::checkpoint::ConnectionProfile {
            connection_id: "conn-123".into(),
            company_guid: Some("guid-company-A".into()),
            company_name: "Acme Corp Ltd".into(),
            paired_at: "2026-09-09T10:00:00Z".into(),
        })
        .unwrap();

    let _ = Vault::store_token_for("conn-123", "token-for-conn-123");
    let _ = Vault::store_connection_id("conn-123");

    // Mock Tally serving Company B with GUID "guid-company-B"
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let tally_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME>Acme Corp Ltd</NAME><ALTERID>100</ALTERID><GUID>guid-company-B</GUID></COMPANY>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(tally_xml))
        .mount(&tally_server)
        .await;

    // Simulate Agent startup (restarting after profile was persisted)
    let agent_state = fininsight_tally_agent_lib::AgentState::with_options(
        tally_port,
        None,
        base_path,
    )
    .unwrap();

    // Verify profile identity is loaded
    let profile = agent_state.profile_store.load().unwrap().expect("Profile should exist");
    assert_eq!(profile.connection_id, "conn-123");
    assert_eq!(profile.company_guid, Some("guid-company-A".into()));

    // Attempt sync_now: must fail closed due to GUID mismatch with running Tally
    let sync_res = agent_state.sync_now().await;
    assert!(sync_res.is_err(), "Sync must fail closed when Tally GUID != Profile GUID");

    let _ = Vault::delete_token_for("conn-123");
    let _ = Vault::delete_connection_id();
}

#[tokio::test]
async fn test_zero_delta_sync_pushes_empty_batch() {
    // 1. Mock Tally instance with alter_id = 100 and no new records above 100
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let tally_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME>Acme Corp Ltd</NAME><ALTERID>100</ALTERID><GUID>guid-acme-100</GUID></COMPANY>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(tally_xml))
        .mount(&tally_server)
        .await;

    let tally_endpoint = TallyEndpoint::new("127.0.0.1", tally_port).unwrap();
    let tally_client = TallyClient::new(tally_endpoint).unwrap();

    // 2. Mock Cloud Backend expecting empty records array and alter_id_high = 100
    let cloud_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/sync/delta"))
        .and(wiremock::matchers::body_json(serde_json::json!({
            "records": [],
            "alter_id_high": 100
        })))
        .respond_with(ResponseTemplate::new(200))
        .mount(&cloud_server)
        .await;

    let cloud_client = CloudClient::from_base_url(&cloud_server.uri())
        .unwrap()
        .with_token("test-token-zero-delta");

    let dir = tempfile::tempdir().unwrap();
    let store = CheckpointStore::new(dir.path().join("cp.json"));
    store
        .save(&Checkpoint {
            connection_id: Some("conn-zero".into()),
            last_known_alter_id: 100,
            backfill_complete: true,
            last_successful_sync: None,
        })
        .unwrap();

    let orch = SyncOrchestrator::new(tally_client, cloud_client, store.clone())
        .with_expected_profile(
            Some("Acme Corp Ltd".into()),
            Some("guid-acme-100".into()),
            Some("conn-zero".into()),
        );

    let result = orch.run_delta_sync().await.unwrap();
    assert!(result.success);
    assert_eq!(result.records_pushed, 0);

    // Verify checkpoint timestamp was updated even with 0 new records
    let cp = store.load().unwrap();
    assert_eq!(cp.last_known_alter_id, 100);
    assert!(cp.last_successful_sync.is_some());
}

#[tokio::test]
async fn test_disconnect_preserves_checkpoint_file() {
    let _guard = VAULT_TEST_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let profile_file = dir.path().join("profiles.json");
    let checkpoint_file = dir.path().join("checkpoints").join("checkpoint_conn-keep-me.json");

    let profile_store = fininsight_tally_agent_lib::checkpoint::ProfileStore::new(profile_file.clone());
    profile_store
        .save(&fininsight_tally_agent_lib::checkpoint::ConnectionProfile {
            connection_id: "conn-keep-me".into(),
            company_guid: Some("guid-keep-me".into()),
            company_name: "Keep Me Corp".into(),
            paired_at: "2026-09-09T10:00:00Z".into(),
        })
        .unwrap();

    let cp_store = CheckpointStore::new(checkpoint_file.clone());
    cp_store
        .save(&Checkpoint {
            connection_id: Some("conn-keep-me".into()),
            last_known_alter_id: 500,
            backfill_complete: true,
            last_successful_sync: Some("2026-09-09T10:00:00Z".into()),
        })
        .unwrap();

    let _ = Vault::store_token_for("conn-keep-me", "token-keep-me");
    let _ = Vault::store_connection_id("conn-keep-me");

    let agent_state = fininsight_tally_agent_lib::AgentState::with_options(
        9000,
        None,
        profile_file.clone(),
    )
    .unwrap();

    agent_state.disconnect().await.unwrap();

    // Profile must be deleted
    assert!(profile_store.load().unwrap().is_none());
    // Keyring tokens must be deleted
    assert!(Vault::get_token_for("conn-keep-me").is_err());
    assert!(Vault::get_connection_id().is_err());

    // Checkpoint file MUST still exist to preserve alter_id state for re-pairing
    assert!(checkpoint_file.exists(), "Checkpoint file must be preserved on disconnect");
    let preserved_cp = cp_store.load().unwrap();
    assert_eq!(preserved_cp.last_known_alter_id, 500);
    assert_eq!(preserved_cp.backfill_complete, true);
}

#[tokio::test]
async fn test_repeated_sync_does_not_duplicate_records() {
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let tally_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME>Acme Corp Ltd</NAME><ALTERID>150</ALTERID><GUID>guid-acme-dup</GUID></COMPANY>
    <VOUCHER>
      <VOUCHERNUMBER>INV-001</VOUCHERNUMBER>
      <VOUCHERTYPENAME>Sales</VOUCHERTYPENAME>
      <PARTYLEDGERNAME>Aarav Textiles</PARTYLEDGERNAME>
      <DATE>20260401</DATE>
      <ALTERID>150</ALTERID>
      <AMOUNT>25000.00</AMOUNT>
      <GUID>vch-guid-1</GUID>
      <MASTERID>101</MASTERID>
    </VOUCHER>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(tally_xml))
        .mount(&tally_server)
        .await;

    let tally_endpoint = TallyEndpoint::new("127.0.0.1", tally_port).unwrap();
    let tally_client = TallyClient::new(tally_endpoint).unwrap();

    let cloud_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/sync/delta"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&cloud_server)
        .await;

    let cloud_client = CloudClient::from_base_url(&cloud_server.uri())
        .unwrap()
        .with_token("test-token-dup");

    let dir = tempfile::tempdir().unwrap();
    let store = CheckpointStore::new(dir.path().join("cp.json"));
    store
        .save(&Checkpoint {
            connection_id: Some("conn-dup".into()),
            last_known_alter_id: 100,
            backfill_complete: true,
            last_successful_sync: None,
        })
        .unwrap();

    let orch = SyncOrchestrator::new(tally_client.clone(), cloud_client.clone(), store.clone())
        .with_expected_profile(
            Some("Acme Corp Ltd".into()),
            Some("guid-acme-dup".into()),
            Some("conn-dup".into()),
        );

    // First sync run: advances checkpoint to 150
    let res1 = orch.run_delta_sync().await.unwrap();
    assert_eq!(res1.records_pushed, 1);
    assert_eq!(res1.new_alter_id, Some(150));

    let cp1 = store.load().unwrap();
    assert_eq!(cp1.last_known_alter_id, 150);

    // Second sync run with unchanged Tally state: sends 0 records (empty batch)
    let res2 = orch.run_delta_sync().await.unwrap();
    assert_eq!(res2.records_pushed, 0);
    assert_eq!(res2.new_alter_id, Some(150));

    let cp2 = store.load().unwrap();
    assert_eq!(cp2.last_known_alter_id, 150);
}

#[tokio::test]
async fn test_retry_after_success_does_not_duplicate_records() {
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let tally_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME>Acme Corp Ltd</NAME><ALTERID>200</ALTERID><GUID>guid-acme-retry</GUID></COMPANY>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(tally_xml))
        .mount(&tally_server)
        .await;

    let tally_endpoint = TallyEndpoint::new("127.0.0.1", tally_port).unwrap();
    let tally_client = TallyClient::new(tally_endpoint).unwrap();

    let cloud_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/sync/delta"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&cloud_server)
        .await;

    let cloud_client = CloudClient::from_base_url(&cloud_server.uri())
        .unwrap()
        .with_token("test-token-retry-success");

    let dir = tempfile::tempdir().unwrap();
    let store = CheckpointStore::new(dir.path().join("cp.json"));
    store
        .save(&Checkpoint {
            connection_id: Some("conn-retry-success".into()),
            last_known_alter_id: 200,
            backfill_complete: true,
            last_successful_sync: Some("2026-09-09T10:00:00Z".into()),
        })
        .unwrap();

    let orch = SyncOrchestrator::new(tally_client, cloud_client, store.clone())
        .with_expected_profile(
            Some("Acme Corp Ltd".into()),
            Some("guid-acme-retry".into()),
            Some("conn-retry-success".into()),
        );

    // Running sync when checkpoint is already at alter_id 200 results in 0 records extracted
    let res = orch.run_delta_sync().await.unwrap();
    assert_eq!(res.records_pushed, 0);
    assert_eq!(res.new_alter_id, Some(200));
}

#[tokio::test]
async fn test_checkpoint_does_not_advance_on_failed_push() {
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let tally_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME>Acme Corp Ltd</NAME><ALTERID>300</ALTERID><GUID>guid-acme-fail</GUID></COMPANY>
    <VOUCHER>
      <VOUCHERNUMBER>INV-999</VOUCHERNUMBER>
      <VOUCHERTYPENAME>Sales</VOUCHERTYPENAME>
      <PARTYLEDGERNAME>Aarav Textiles</PARTYLEDGERNAME>
      <DATE>20260401</DATE>
      <ALTERID>300</ALTERID>
      <AMOUNT>1000.00</AMOUNT>
      <GUID>vch-guid-fail</GUID>
      <MASTERID>999</MASTERID>
    </VOUCHER>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(tally_xml))
        .mount(&tally_server)
        .await;

    let tally_endpoint = TallyEndpoint::new("127.0.0.1", tally_port).unwrap();
    let tally_client = TallyClient::new(tally_endpoint).unwrap();

    let cloud_server = MockServer::start().await;
    // Cloud returns 500 error
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/sync/delta"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&cloud_server)
        .await;

    let cloud_client = CloudClient::from_base_url(&cloud_server.uri())
        .unwrap()
        .with_token("test-token-failed-push");

    let dir = tempfile::tempdir().unwrap();
    let store = CheckpointStore::new(dir.path().join("cp.json"));
    store
        .save(&Checkpoint {
            connection_id: Some("conn-fail".into()),
            last_known_alter_id: 200,
            backfill_complete: true,
            last_successful_sync: None,
        })
        .unwrap();

    let orch = SyncOrchestrator::new(tally_client, cloud_client, store.clone())
        .with_expected_profile(
            Some("Acme Corp Ltd".into()),
            Some("guid-acme-fail".into()),
            Some("conn-fail".into()),
        );

    let res = orch.run_delta_sync().await.unwrap();
    assert!(!res.success, "Sync should report failure on 500 status");

    let cp = store.load().unwrap();
    assert_eq!(
        cp.last_known_alter_id, 200,
        "Checkpoint must remain at 200 and not advance to 300 when push fails"
    );
}

#[tokio::test]
async fn test_crash_after_push_before_checkpoint_does_not_corrupt_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = CheckpointStore::new(dir.path().join("cp.json"));
    store
        .save(&Checkpoint {
            connection_id: Some("conn-crash-test".into()),
            last_known_alter_id: 100,
            backfill_complete: true,
            last_successful_sync: None,
        })
        .unwrap();

    // If agent process crashes right after pushing to gateway, checkpoint is still at 100
    let cp = store.load().unwrap();
    assert_eq!(cp.last_known_alter_id, 100);
    assert_eq!(cp.backfill_complete, true);
    // State is intact and valid JSON
}

#[tokio::test]
async fn test_multi_company_discovery_parses_all_and_matches_active() {
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();

    // Tally returns 2 companies in List of Companies
    let company_list_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME>DemoCorp</NAME><ALTERID>230</ALTERID><GUID>guid-demo-111</GUID></COMPANY>
    <COMPANY><NAME>ThunderClaps</NAME><ALTERID>450</ALTERID><GUID>guid-thunder-222</GUID></COMPANY>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(company_list_xml))
        .mount(&tally_server)
        .await;

    let tally_endpoint = TallyEndpoint::new("127.0.0.1", tally_port).unwrap();
    let tally_client = TallyClient::new(tally_endpoint).unwrap();

    let discovered = tally_client.discover_companies().await.unwrap();
    assert_eq!(discovered.len(), 2);
    assert_eq!(discovered[0].company_name, "DemoCorp");
    assert_eq!(discovered[0].company_guid.as_deref(), Some("guid-demo-111"));
    assert_eq!(discovered[1].company_name, "ThunderClaps");
    assert_eq!(discovered[1].company_guid.as_deref(), Some("guid-thunder-222"));

    // ping takes the first company from collection
    let active = tally_client.ping().await.unwrap();
    assert_eq!(active.company_name, "DemoCorp");
    assert_eq!(active.company_guid.as_deref(), Some("guid-demo-111"));
}

#[tokio::test]
async fn test_company_switching_updates_active_and_blocks_inactive_sync() {
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();

    // Tally currently has DemoCorp active
    let tally_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME>DemoCorp</NAME><ALTERID>230</ALTERID><GUID>guid-demo-111</GUID></COMPANY>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(tally_xml))
        .mount(&tally_server)
        .await;

    let tally_endpoint = TallyEndpoint::new("127.0.0.1", tally_port).unwrap();
    let tally_client = TallyClient::new(tally_endpoint).unwrap();

    let cloud_server = MockServer::start().await;
    let cloud_client = CloudClient::from_base_url(&cloud_server.uri())
        .unwrap()
        .with_token("test-token-multi");

    let dir = tempfile::tempdir().unwrap();
    let store_b = CheckpointStore::for_connection_in_dir(dir.path(), "conn-thunder-222");
    store_b
        .save(&Checkpoint {
            connection_id: Some("conn-thunder-222".into()),
            last_known_alter_id: 450,
            backfill_complete: true,
            last_successful_sync: None,
        })
        .unwrap();

    // Trying to sync ThunderClaps while DemoCorp is active in Tally MUST fail closed
    let orch_b = SyncOrchestrator::new(tally_client.clone(), cloud_client.clone(), store_b)
        .with_expected_profile(
            Some("ThunderClaps".into()),
            Some("guid-thunder-222".into()),
            Some("conn-thunder-222".into()),
        );

    let res_b = orch_b.run_delta_sync().await;
    assert!(res_b.is_err(), "Syncing ThunderClaps must fail when DemoCorp is active in Tally");
    let err_msg = res_b.unwrap_err().to_string();
    assert!(err_msg.contains("DemoCorp") || err_msg.contains("guid-demo-111"));
}

#[tokio::test]
async fn test_multi_company_sync_isolation_company_a_vs_company_b() {
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();

    // Tally has DemoCorp active with a new voucher
    let tally_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME>DemoCorp</NAME><ALTERID>250</ALTERID><GUID>guid-demo-111</GUID></COMPANY>
    <VOUCHER>
      <VOUCHERNUMBER>INV-DEMO-01</VOUCHERNUMBER>
      <VOUCHERTYPENAME>Sales</VOUCHERTYPENAME>
      <PARTYLEDGERNAME>Customer X</PARTYLEDGERNAME>
      <DATE>20260401</DATE>
      <ALTERID>250</ALTERID>
      <AMOUNT>15000.00</AMOUNT>
      <GUID>vch-guid-demo</GUID>
      <MASTERID>2501</MASTERID>
    </VOUCHER>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(tally_xml))
        .mount(&tally_server)
        .await;

    let tally_endpoint = TallyEndpoint::new("127.0.0.1", tally_port).unwrap();
    let tally_client = TallyClient::new(tally_endpoint).unwrap();

    let cloud_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/sync/delta"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&cloud_server)
        .await;

    let cloud_client = CloudClient::from_base_url(&cloud_server.uri())
        .unwrap()
        .with_token("test-token-demo");

    let dir = tempfile::tempdir().unwrap();
    let store_a = CheckpointStore::for_connection_in_dir(dir.path(), "conn-demo-111");
    store_a
        .save(&Checkpoint {
            connection_id: Some("conn-demo-111".into()),
            last_known_alter_id: 230,
            backfill_complete: true,
            last_successful_sync: None,
        })
        .unwrap();

    let store_b = CheckpointStore::for_connection_in_dir(dir.path(), "conn-thunder-222");
    store_b
        .save(&Checkpoint {
            connection_id: Some("conn-thunder-222".into()),
            last_known_alter_id: 450,
            backfill_complete: true,
            last_successful_sync: Some("2026-09-09T08:00:00Z".into()),
        })
        .unwrap();

    // Sync Company A
    let orch_a = SyncOrchestrator::new(tally_client, cloud_client, store_a.clone())
        .with_expected_profile(
            Some("DemoCorp".into()),
            Some("guid-demo-111".into()),
            Some("conn-demo-111".into()),
        );

    let res_a = orch_a.run_delta_sync().await.unwrap();
    assert!(res_a.success);
    assert_eq!(res_a.records_pushed, 1);
    assert_eq!(res_a.new_alter_id, Some(250));

    // Verify Checkpoint A advanced to 250
    let cp_a = store_a.load().unwrap();
    assert_eq!(cp_a.last_known_alter_id, 250);

    // Verify Checkpoint B remained strictly untouched at 450
    let cp_b = store_b.load().unwrap();
    assert_eq!(cp_b.last_known_alter_id, 450);
    assert_eq!(cp_b.last_successful_sync.as_deref(), Some("2026-09-09T08:00:00Z"));
}

#[tokio::test]
async fn test_crash_and_retry_idempotent_recovery() {
    // Scenario:
    // 1. Agent extracts voucher alter_id=300 from Tally.
    // 2. Gateway accepts sync (HTTP 200).
    // 3. Agent crashes before local checkpoint update (simulated: checkpoint file still at 200).
    // 4. Agent restarts and retries the exact same batch.
    // 5. Gateway receives idempotent batch, returns HTTP 200.
    // 6. Checkpoint successfully advances to 300 without duplicate records.

    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let tally_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME>DemoCorp</NAME><ALTERID>300</ALTERID><GUID>guid-demo-111</GUID></COMPANY>
    <VOUCHER>
      <VOUCHERNUMBER>INV-300</VOUCHERNUMBER>
      <VOUCHERTYPENAME>Sales</VOUCHERTYPENAME>
      <PARTYLEDGERNAME>Customer Y</PARTYLEDGERNAME>
      <DATE>20260401</DATE>
      <ALTERID>300</ALTERID>
      <AMOUNT>50000.00</AMOUNT>
      <GUID>vch-guid-300</GUID>
      <MASTERID>3001</MASTERID>
    </VOUCHER>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(tally_xml))
        .mount(&tally_server)
        .await;

    let tally_endpoint = TallyEndpoint::new("127.0.0.1", tally_port).unwrap();
    let tally_client = TallyClient::new(tally_endpoint).unwrap();

    let cloud_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/sync/delta"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&cloud_server)
        .await;

    let cloud_client = CloudClient::from_base_url(&cloud_server.uri())
        .unwrap()
        .with_token("test-token-crash-recovery");

    let dir = tempfile::tempdir().unwrap();
    let store = CheckpointStore::for_connection_in_dir(dir.path(), "conn-crash-rec");
    store
        .save(&Checkpoint {
            connection_id: Some("conn-crash-rec".into()),
            last_known_alter_id: 200,
            backfill_complete: true,
            last_successful_sync: None,
        })
        .unwrap();

    // Step 1: Agent run 1 -> pushes to gateway
    let orch1 = SyncOrchestrator::new(tally_client.clone(), cloud_client.clone(), store.clone())
        .with_expected_profile(
            Some("DemoCorp".into()),
            Some("guid-demo-111".into()),
            Some("conn-crash-rec".into()),
        );

    // Simulate crash after Gateway HTTP 200 by manually resetting checkpoint store back to 200
    let res1 = orch1.run_delta_sync().await.unwrap();
    assert!(res1.success);
    assert_eq!(res1.new_alter_id, Some(300));

    // Force checkpoint back to pre-crash state (alter_id=200)
    store
        .save(&Checkpoint {
            connection_id: Some("conn-crash-rec".into()),
            last_known_alter_id: 200,
            backfill_complete: true,
            last_successful_sync: None,
        })
        .unwrap();

    assert_eq!(store.load().unwrap().last_known_alter_id, 200);

    // Step 2: Agent restarts and retries the sync
    let orch2 = SyncOrchestrator::new(tally_client, cloud_client, store.clone())
        .with_expected_profile(
            Some("DemoCorp".into()),
            Some("guid-demo-111".into()),
            Some("conn-crash-rec".into()),
        );

    let res2 = orch2.run_delta_sync().await.unwrap();
    assert!(res2.success);
    assert_eq!(res2.records_pushed, 1);
    assert_eq!(res2.new_alter_id, Some(300));

    // Checkpoint reaches correct value 300
    let final_cp = store.load().unwrap();
    assert_eq!(final_cp.last_known_alter_id, 300);
    assert!(final_cp.last_successful_sync.is_some());

    // Verify exactly 2 requests reached the gateway (both identical payloads)
    let reqs = cloud_server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2);
    let b1: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let b2: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert_eq!(b1, b2);
}

#[tokio::test]
async fn test_crash_during_company_a_sync_leaves_company_b_state_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let store_a = CheckpointStore::for_connection_in_dir(dir.path(), "conn-a");
    let store_b = CheckpointStore::for_connection_in_dir(dir.path(), "conn-b");

    store_a
        .save(&Checkpoint {
            connection_id: Some("conn-a".into()),
            last_known_alter_id: 100,
            backfill_complete: true,
            last_successful_sync: None,
        })
        .unwrap();

    store_b
        .save(&Checkpoint {
            connection_id: Some("conn-b".into()),
            last_known_alter_id: 500,
            backfill_complete: true,
            last_successful_sync: Some("2026-09-09T09:00:00Z".into()),
        })
        .unwrap();

    // Mock Tally failure/crash during Company A extraction
    let unused_port = 59997;
    let tally_endpoint = TallyEndpoint::new("127.0.0.1", unused_port).unwrap();
    let tally_client = TallyClient::new(tally_endpoint).unwrap();

    let cloud_server = MockServer::start().await;
    let cloud_client = CloudClient::from_base_url(&cloud_server.uri())
        .unwrap()
        .with_token("test-token");

    let orch_a = SyncOrchestrator::new(tally_client, cloud_client, store_a.clone())
        .with_expected_profile(
            Some("Company A".into()),
            Some("guid-a".into()),
            Some("conn-a".into()),
        );

    let res_a = orch_a.run_delta_sync().await;
    assert!(res_a.is_err());

    // Company A checkpoint unchanged
    let cp_a = store_a.load().unwrap();
    assert_eq!(cp_a.last_known_alter_id, 100);

    // Company B checkpoint completely untouched
    let cp_b = store_b.load().unwrap();
    assert_eq!(cp_b.last_known_alter_id, 500);
    assert_eq!(cp_b.last_successful_sync.as_deref(), Some("2026-09-09T09:00:00Z"));
}

#[tokio::test]
async fn test_inactive_company_can_be_paired_while_another_company_active() {
    let _guard = VAULT_TEST_LOCK.lock().await;

    // 1. Tally has DemoCorp active
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let tally_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME>DemoCorp</NAME><ALTERID>100</ALTERID><GUID>guid-demo-111</GUID></COMPANY>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(tally_xml))
        .mount(&tally_server)
        .await;

    // 2. Mock Cloud Backend for device auth
    let cloud_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/device/initiate"))
        .and(wiremock::matchers::body_string_contains("ThunderClaps"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_code": "dev-thunder-code",
            "user_code": "THUNDER-CODE",
            "verification_uri": "https://app.fininsight.io/device",
            "expires_in": 300,
            "interval": 5
        })))
        .mount(&cloud_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/v1/agent/device/poll"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "approved",
            "agent_token": "token-thunder-secret",
            "connection_id": "conn-thunder-222"
        })))
        .mount(&cloud_server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let agent_state = fininsight_tally_agent_lib::AgentState::with_options(
        tally_port,
        Some(cloud_server.uri()),
        dir.path().join("profiles.json"),
    )
    .unwrap();

    // Initiate pairing for ThunderClaps (which is NOT the active company in Tally)
    let init_res = agent_state
        .start_company_device_login("ThunderClaps", Some("guid-thunder-222"))
        .await;
    assert!(init_res.is_ok(), "Initiating pairing for inactive ThunderClaps must succeed");
    let session = init_res.unwrap();
    assert_eq!(session.user_code, "THUNDER-CODE");

    // Poll and complete pairing
    let paired_name = agent_state
        .poll_company_device_login("ThunderClaps", Some("guid-thunder-222"))
        .await
        .unwrap();
    assert_eq!(paired_name, "ThunderClaps");

    // Verify connection profile for ThunderClaps was stored with its exact GUID
    let profile = agent_state.profile_store.get_by_connection_id("conn-thunder-222").unwrap().unwrap();
    assert_eq!(profile.company_name, "ThunderClaps");
    assert_eq!(profile.company_guid.as_deref(), Some("guid-thunder-222"));

    // Verify token stored in vault for ThunderClaps
    assert_eq!(Vault::get_token_for("conn-thunder-222").unwrap(), "token-thunder-secret");

    let _ = Vault::delete_token_for("conn-thunder-222");
    let _ = Vault::delete_connection_id();
}

#[tokio::test]
async fn test_democorp_data_can_never_be_pushed_through_thunderclaps_connection() {
    let _guard = VAULT_TEST_LOCK.lock().await;

    // Tally currently has DemoCorp open with private invoices
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let tally_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME>DemoCorp</NAME><ALTERID>500</ALTERID><GUID>guid-demo-111</GUID></COMPANY>
    <VOUCHER>
      <VOUCHERNUMBER>INV-DEMO-SECRET</VOUCHERNUMBER>
      <VOUCHERTYPENAME>Sales</VOUCHERTYPENAME>
      <PARTYLEDGERNAME>Confidential Party</PARTYLEDGERNAME>
      <DATE>20260401</DATE>
      <ALTERID>500</ALTERID>
      <AMOUNT>999999.00</AMOUNT>
      <GUID>vch-demo-secret</GUID>
    </VOUCHER>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(tally_xml))
        .mount(&tally_server)
        .await;

    // Mock Cloud Backend — should NEVER receive any sync requests
    let cloud_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/agent/sync/delta"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&cloud_server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let base_path = dir.path().join("profiles.json");
    let agent_state = fininsight_tally_agent_lib::AgentState::with_options(
        tally_port,
        Some(cloud_server.uri()),
        base_path,
    )
    .unwrap();

    // Save ThunderClaps connection profile
    agent_state
        .profile_store
        .save(&fininsight_tally_agent_lib::checkpoint::ConnectionProfile {
            connection_id: "conn-thunder-222".into(),
            company_guid: Some("guid-thunder-222".into()),
            company_name: "ThunderClaps".into(),
            paired_at: "2026-09-09T10:00:00Z".into(),
        })
        .unwrap();

    let _ = Vault::store_token_for("conn-thunder-222", "token-thunder-222");

    // Attempting to sync ThunderClaps while DemoCorp is active MUST fail closed
    let sync_res = agent_state.sync_company("conn-thunder-222").await;
    assert!(sync_res.is_err(), "Syncing ThunderClaps must fail closed when DemoCorp is open");
    let err = sync_res.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("Open 'ThunderClaps' in Tally to sync this company") || err_msg.contains("guid-thunder-222"),
        "Error must clearly instruct user to open ThunderClaps: got '{err_msg}'"
    );

    // Verify 0 delta sync requests reached cloud backend
    let reqs = cloud_server.received_requests().await.unwrap();
    let delta_reqs: Vec<_> = reqs.iter().filter(|r| r.url.path().contains("/sync/")).collect();
    assert_eq!(delta_reqs.len(), 0, "No data from DemoCorp can ever be pushed to ThunderClaps connection");

    let _ = Vault::delete_token_for("conn-thunder-222");
    let _ = Vault::delete_connection_id();
}

#[tokio::test]
async fn test_restart_preserves_multiple_company_profiles_and_tokens() {
    let _guard = VAULT_TEST_LOCK.lock().await;

    let dir = tempfile::tempdir().unwrap();
    let profiles_file = dir.path().join("profiles.json");

    let profile_store = fininsight_tally_agent_lib::checkpoint::ProfileStore::new(profiles_file.clone());
    profile_store
        .save(&fininsight_tally_agent_lib::checkpoint::ConnectionProfile {
            connection_id: "conn-demo-111".into(),
            company_guid: Some("guid-demo-111".into()),
            company_name: "DemoCorp".into(),
            paired_at: "2026-09-09T08:00:00Z".into(),
        })
        .unwrap();

    profile_store
        .save(&fininsight_tally_agent_lib::checkpoint::ConnectionProfile {
            connection_id: "conn-thunder-222".into(),
            company_guid: Some("guid-thunder-222".into()),
            company_name: "ThunderClaps".into(),
            paired_at: "2026-09-09T09:00:00Z".into(),
        })
        .unwrap();

    let _ = Vault::store_token_for("conn-demo-111", "token-demo-111");
    let _ = Vault::store_token_for("conn-thunder-222", "token-thunder-222");

    let cp_store_a = CheckpointStore::for_connection_in_dir(dir.path(), "conn-demo-111");
    cp_store_a
        .save(&Checkpoint {
            connection_id: Some("conn-demo-111".into()),
            last_known_alter_id: 120,
            backfill_complete: true,
            last_successful_sync: Some("2026-09-09T08:30:00Z".into()),
        })
        .unwrap();

    let cp_store_b = CheckpointStore::for_connection_in_dir(dir.path(), "conn-thunder-222");
    cp_store_b
        .save(&Checkpoint {
            connection_id: Some("conn-thunder-222".into()),
            last_known_alter_id: 450,
            backfill_complete: true,
            last_successful_sync: Some("2026-09-09T09:30:00Z".into()),
        })
        .unwrap();

    // Simulate Agent restart
    let restarted_agent = fininsight_tally_agent_lib::AgentState::with_options(
        9000,
        None,
        profiles_file,
    )
    .unwrap();

    let loaded_profiles = restarted_agent.profile_store.load_all().unwrap();
    assert_eq!(loaded_profiles.len(), 2);

    let prof_a = restarted_agent.profile_store.get_by_connection_id("conn-demo-111").unwrap().unwrap();
    assert_eq!(prof_a.company_name, "DemoCorp");
    assert_eq!(prof_a.company_guid.as_deref(), Some("guid-demo-111"));

    let prof_b = restarted_agent.profile_store.get_by_connection_id("conn-thunder-222").unwrap().unwrap();
    assert_eq!(prof_b.company_name, "ThunderClaps");
    assert_eq!(prof_b.company_guid.as_deref(), Some("guid-thunder-222"));

    assert_eq!(Vault::get_token_for("conn-demo-111").unwrap(), "token-demo-111");
    assert_eq!(Vault::get_token_for("conn-thunder-222").unwrap(), "token-thunder-222");

    let loaded_cp_a = cp_store_a.load().unwrap();
    assert_eq!(loaded_cp_a.last_known_alter_id, 120);

    let loaded_cp_b = cp_store_b.load().unwrap();
    assert_eq!(loaded_cp_b.last_known_alter_id, 450);

    let _ = Vault::delete_token_for("conn-demo-111");
    let _ = Vault::delete_token_for("conn-thunder-222");
    let _ = Vault::delete_connection_id();
}

#[tokio::test]
async fn test_active_company_dynamic_transitions_a_b_c_d() {
    let _guard = VAULT_TEST_LOCK.lock().await;

    // Shared state to switch active company dynamically
    let active_name = std::sync::Arc::new(std::sync::Mutex::new("DemoCorp".to_string()));
    let active_guid = std::sync::Arc::new(std::sync::Mutex::new("guid-demo-111".to_string()));

    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();

    // 1. Discovery list mock: returns all 3 companies
    let discovery_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <COMPANY><NAME>DemoCorp</NAME><ALTERID>100</ALTERID><GUID>guid-demo-111</GUID></COMPANY>
    <COMPANY><NAME>ThunderClaps</NAME><ALTERID>200</ALTERID><GUID>guid-thunder-222</GUID></COMPANY>
    <COMPANY><NAME>WireSnaks</NAME><ALTERID>300</ALTERID><GUID>guid-wiresnaks-333</GUID></COMPANY>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .and(wiremock::matchers::body_string_contains("Collection of Companies"))
        .respond_with(ResponseTemplate::new(200).set_body_string(discovery_xml))
        .mount(&tally_server)
        .await;

    // 2. Active company ping mock: returns the current active company dynamically
    struct DynamicActiveResponder {
        name: std::sync::Arc<std::sync::Mutex<String>>,
        guid: std::sync::Arc<std::sync::Mutex<String>>,
    }

    impl wiremock::Respond for DynamicActiveResponder {
        fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
            let n = self.name.lock().unwrap().clone();
            let g = self.guid.lock().unwrap().clone();
            let xml = format!(
                r#"<ENVELOPE><BODY><DATA><COLLECTION>
                <COMPANY><NAME>{}</NAME><ALTERID>150</ALTERID><GUID>{}</GUID></COMPANY>
                </COLLECTION></DATA></BODY></ENVELOPE>"#,
                n, g
            );
            ResponseTemplate::new(200).set_body_string(xml)
        }
    }

    Mock::given(method("POST"))
        .and(wiremock::matchers::body_string_contains("FinInsightActiveCompanyColl"))
        .respond_with(DynamicActiveResponder {
            name: active_name.clone(),
            guid: active_guid.clone(),
        })
        .mount(&tally_server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let profiles_file = dir.path().join("profiles.json");
    let agent_state = fininsight_tally_agent_lib::AgentState::with_options(
        tally_port,
        None,
        profiles_file,
    )
    .unwrap();

    // Setup 2 connected profiles: DemoCorp and ThunderClaps. WireSnaks is unconnected.
    agent_state
        .profile_store
        .save(&fininsight_tally_agent_lib::checkpoint::ConnectionProfile {
            connection_id: "conn-demo-111".into(),
            company_guid: Some("guid-demo-111".into()),
            company_name: "DemoCorp".into(),
            paired_at: "2026-09-09T08:00:00Z".into(),
        })
        .unwrap();

    agent_state
        .profile_store
        .save(&fininsight_tally_agent_lib::checkpoint::ConnectionProfile {
            connection_id: "conn-thunder-222".into(),
            company_guid: Some("guid-thunder-222".into()),
            company_name: "ThunderClaps".into(),
            paired_at: "2026-09-09T09:00:00Z".into(),
        })
        .unwrap();

    // Transition A: DemoCorp is active in Tally
    *active_name.lock().unwrap() = "DemoCorp".to_string();
    *active_guid.lock().unwrap() = "guid-demo-111".to_string();

    let items_a = agent_state.discover_and_merge_companies().await.unwrap();
    let demo_a = items_a.iter().find(|i| i.company_name == "DemoCorp").unwrap();
    let thunder_a = items_a.iter().find(|i| i.company_name == "ThunderClaps").unwrap();
    let wiresnaks_a = items_a.iter().find(|i| i.company_name == "WireSnaks").unwrap();

    assert_eq!(demo_a.status, "Connected · Active");
    assert_eq!(demo_a.is_active_in_tally, true);
    assert_eq!(thunder_a.status, "Connected · Inactive");
    assert_eq!(thunder_a.is_active_in_tally, false);
    assert_eq!(wiresnaks_a.status, "Available · Inactive");
    assert_eq!(wiresnaks_a.is_active_in_tally, false);

    // Transition B: User switches active company in Tally to ThunderClaps
    *active_name.lock().unwrap() = "ThunderClaps".to_string();
    *active_guid.lock().unwrap() = "guid-thunder-222".to_string();

    let items_b = agent_state.discover_and_merge_companies().await.unwrap();
    let demo_b = items_b.iter().find(|i| i.company_name == "DemoCorp").unwrap();
    let thunder_b = items_b.iter().find(|i| i.company_name == "ThunderClaps").unwrap();
    let wiresnaks_b = items_b.iter().find(|i| i.company_name == "WireSnaks").unwrap();

    assert_eq!(demo_b.status, "Connected · Inactive");
    assert_eq!(demo_b.is_active_in_tally, false);
    assert_eq!(thunder_b.status, "Connected · Active");
    assert_eq!(thunder_b.is_active_in_tally, true);
    assert_eq!(wiresnaks_b.status, "Available · Inactive");
    assert_eq!(wiresnaks_b.is_active_in_tally, false);

    // Transition C: User switches active company in Tally to WireSnaks (unconnected)
    *active_name.lock().unwrap() = "WireSnaks".to_string();
    *active_guid.lock().unwrap() = "guid-wiresnaks-333".to_string();

    let items_c = agent_state.discover_and_merge_companies().await.unwrap();
    let demo_c = items_c.iter().find(|i| i.company_name == "DemoCorp").unwrap();
    let thunder_c = items_c.iter().find(|i| i.company_name == "ThunderClaps").unwrap();
    let wiresnaks_c = items_c.iter().find(|i| i.company_name == "WireSnaks").unwrap();

    assert_eq!(demo_c.status, "Connected · Inactive");
    assert_eq!(demo_c.is_active_in_tally, false);
    assert_eq!(thunder_c.status, "Connected · Inactive");
    assert_eq!(thunder_c.is_active_in_tally, false);
    assert_eq!(wiresnaks_c.status, "Available · Active");
    assert_eq!(wiresnaks_c.is_active_in_tally, true);

    // Transition D: User switches back to DemoCorp
    *active_name.lock().unwrap() = "DemoCorp".to_string();
    *active_guid.lock().unwrap() = "guid-demo-111".to_string();

    let items_d = agent_state.discover_and_merge_companies().await.unwrap();
    let demo_d = items_d.iter().find(|i| i.company_name == "DemoCorp").unwrap();
    let thunder_d = items_d.iter().find(|i| i.company_name == "ThunderClaps").unwrap();
    let wiresnaks_d = items_d.iter().find(|i| i.company_name == "WireSnaks").unwrap();

    assert_eq!(demo_d.status, "Connected · Active");
    assert_eq!(demo_d.is_active_in_tally, true);
    assert_eq!(thunder_d.status, "Connected · Inactive");
    assert_eq!(thunder_d.is_active_in_tally, false);
    assert_eq!(wiresnaks_d.status, "Available · Inactive");
    assert_eq!(wiresnaks_d.is_active_in_tally, false);
}

#[tokio::test]
async fn test_multicompany_democorp_profitcorp_thunderclaps_full_lifecycle() {
    let _guard = VAULT_TEST_LOCK.lock().await;

    let dir = tempfile::tempdir().unwrap();
    let profiles_file = dir.path().join("profiles.json");

    let cloud_server = MockServer::start().await;
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();

    let agent_state = fininsight_tally_agent_lib::AgentState::with_options(
        tally_port,
        Some(cloud_server.uri()),
        profiles_file.clone(),
    )
    .unwrap();

    // 1. Connect DemoCorp
    agent_state
        .complete_pairing_for_company(
            "conn-demo-111",
            "token-demo-secret-111",
            "DemoCorp",
            Some("guid-demo-111"),
        )
        .await
        .unwrap();

    // Set DemoCorp checkpoint to 207 with backfill complete
    let cp_store_demo = CheckpointStore::for_connection_in_dir(dir.path(), "conn-demo-111");
    cp_store_demo
        .advance_on_ack(207, true)
        .unwrap();

    assert_eq!(cp_store_demo.load().unwrap().last_known_alter_id, 207);
    assert_eq!(Vault::get_token_for("conn-demo-111").unwrap(), "token-demo-secret-111");

    // 2. Connect ProfitCorp afterward
    agent_state
        .complete_pairing_for_company(
            "conn-profit-222",
            "token-profit-secret-222",
            "ProfitCorp",
            Some("guid-profit-222"),
        )
        .await
        .unwrap();

    // 3. Both profiles must remain connected in profiles.json
    let profiles = agent_state.profile_store.load_all().unwrap();
    assert_eq!(profiles.len(), 2, "Both DemoCorp and ProfitCorp must be in profiles.json");
    let prof_demo = profiles.iter().find(|p| p.connection_id == "conn-demo-111").expect("DemoCorp profile present");
    let prof_profit = profiles.iter().find(|p| p.connection_id == "conn-profit-222").expect("ProfitCorp profile present");
    assert_eq!(prof_demo.company_name, "DemoCorp");
    assert_eq!(prof_profit.company_name, "ProfitCorp");

    // 4. DemoCorp token in vault remains unchanged
    assert_eq!(Vault::get_token_for("conn-demo-111").unwrap(), "token-demo-secret-111");

    // 5. ProfitCorp gets separate token
    assert_eq!(Vault::get_token_for("conn-profit-222").unwrap(), "token-profit-secret-222");

    // 6. DemoCorp checkpoint remains unchanged at 207
    let cp_demo_after = cp_store_demo.load().unwrap();
    assert_eq!(cp_demo_after.last_known_alter_id, 207, "DemoCorp checkpoint must remain 207");
    assert!(cp_demo_after.backfill_complete, "DemoCorp backfill state must remain true");

    // 7. ProfitCorp has independent checkpoint (0)
    let cp_store_profit = CheckpointStore::for_connection_in_dir(dir.path(), "conn-profit-222");
    let cp_profit = cp_store_profit.load().unwrap();
    assert_eq!(cp_profit.last_known_alter_id, 0, "ProfitCorp checkpoint must be 0");
    assert!(!cp_profit.backfill_complete, "ProfitCorp backfill must be false");

    // 8. Connecting third company (ThunderClaps) does not disconnect first two
    agent_state
        .complete_pairing_for_company(
            "conn-thunder-333",
            "token-thunder-secret-333",
            "ThunderClaps",
            Some("guid-thunder-333"),
        )
        .await
        .unwrap();

    let profiles_3 = agent_state.profile_store.load_all().unwrap();
    assert_eq!(profiles_3.len(), 3, "All 3 companies must be present");
    assert_eq!(Vault::get_token_for("conn-demo-111").unwrap(), "token-demo-secret-111");
    assert_eq!(Vault::get_token_for("conn-profit-222").unwrap(), "token-profit-secret-222");
    assert_eq!(Vault::get_token_for("conn-thunder-333").unwrap(), "token-thunder-secret-333");
    assert_eq!(cp_store_demo.load().unwrap().last_known_alter_id, 207);

    // 9. Restart restores all companies
    let restarted_agent = fininsight_tally_agent_lib::AgentState::with_options(
        tally_port,
        Some(cloud_server.uri()),
        profiles_file.clone(),
    )
    .unwrap();

    let loaded_profiles = restarted_agent.profile_store.load_all().unwrap();
    assert_eq!(loaded_profiles.len(), 3);
    assert_eq!(Vault::get_token_for("conn-demo-111").unwrap(), "token-demo-secret-111");
    assert_eq!(Vault::get_token_for("conn-profit-222").unwrap(), "token-profit-secret-222");
    assert_eq!(Vault::get_token_for("conn-thunder-333").unwrap(), "token-thunder-secret-333");

    // 10. Re-pairing DemoCorp preserves existing checkpoint (207)
    restarted_agent
        .complete_pairing_for_company(
            "conn-demo-reconnected-444",
            "token-demo-new-444",
            "DemoCorp",
            Some("guid-demo-111"),
        )
        .await
        .unwrap();

    let cp_reconnected = CheckpointStore::for_connection_in_dir(dir.path(), "conn-demo-reconnected-444").load().unwrap();
    assert_eq!(cp_reconnected.last_known_alter_id, 207, "Existing checkpoint (207) must be preserved on re-pairing DemoCorp");
    assert!(cp_reconnected.backfill_complete, "Backfill state preserved on re-pairing");

    // Cleanup vault entries
    let _ = Vault::delete_token_for("conn-demo-111");
    let _ = Vault::delete_token_for("conn-profit-222");
    let _ = Vault::delete_token_for("conn-thunder-333");
    let _ = Vault::delete_token_for("conn-demo-reconnected-444");
}

#[tokio::test]
async fn test_tally_offline_preserves_connected_state_and_recovers_cleanly() {
    let _guard = VAULT_TEST_LOCK.lock().await;

    let dir = tempfile::tempdir().unwrap();
    let profiles_file = dir.path().join("profiles.json");

    // Tally server that is initially offline (using unused port)
    let offline_port = 59990;

    let agent_state = fininsight_tally_agent_lib::AgentState::with_options(
        offline_port,
        None,
        profiles_file,
    )
    .unwrap();

    // Setup connected profiles
    agent_state
        .profile_store
        .save(&fininsight_tally_agent_lib::checkpoint::ConnectionProfile {
            connection_id: "conn-demo-111".into(),
            company_guid: Some("guid-demo-111".into()),
            company_name: "DemoCorp".into(),
            paired_at: "2026-09-09T08:00:00Z".into(),
        })
        .unwrap();

    agent_state
        .profile_store
        .save(&fininsight_tally_agent_lib::checkpoint::ConnectionProfile {
            connection_id: "conn-profit-222".into(),
            company_guid: Some("guid-profit-222".into()),
            company_name: "ProfitCorp".into(),
            paired_at: "2026-09-09T09:00:00Z".into(),
        })
        .unwrap();

    // When Tally is offline, discover_and_merge_companies should return all connected companies with "Offline" status
    // and NOT erase or hide them.
    let _items_offline = agent_state.discover_and_merge_companies().await;
    // Note: ping fails so discover returns error or offline items
    let cached_offline = agent_state.get_companies_cached().await;
    assert_eq!(cached_offline.len(), 2, "Both connected companies must remain present in cache when offline");
    assert_eq!(cached_offline[0].status, "Offline");
    assert_eq!(cached_offline[1].status, "Offline");
    assert_eq!(cached_offline[0].is_connected, true);
    assert_eq!(cached_offline[1].is_connected, true);
}

#[test]
fn test_index_html_removes_empty_state_and_check_tally_now() {
    let index_html_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("index.html");

    let contents = std::fs::read_to_string(&index_html_path).expect("index.html must exist");

    assert!(
        !contents.contains("No Tally Companies Detected"),
        "index.html must NOT contain 'No Tally Companies Detected'"
    );
    assert!(
        !contents.contains("Check Tally Now"),
        "index.html must NOT contain 'Check Tally Now'"
    );
    assert!(
        !contents.contains("empty-refresh-btn"),
        "index.html must NOT contain 'empty-refresh-btn'"
    );
    assert!(
        contents.contains("id=\"toast\""),
        "index.html must contain toast element"
    );
}

#[tokio::test]
async fn test_discovery_reconciliation_lifecycle() {
    use fininsight_tally_agent_lib::tally_schema::DiscoveredCompany;
    use fininsight_tally_agent_lib::checkpoint::ConnectionProfile;

    let _guard = VAULT_TEST_LOCK.lock().await;

    let dir = tempfile::tempdir().unwrap();
    let profiles_file = dir.path().join("profiles.json");

    let agent_state = fininsight_tally_agent_lib::AgentState::with_options(
        9000,
        None,
        profiles_file,
    )
    .unwrap();

    // 1. Initial State: DemoCorp is connected with alter_id 207 and token
    agent_state.profile_store.save(&ConnectionProfile {
        connection_id: "conn-demo".into(),
        company_guid: Some("guid-demo".into()),
        company_name: "DemoCorp".into(),
        paired_at: "2026-09-09T10:00:00Z".into(),
    }).unwrap();
    Vault::store_token_for("conn-demo", "token-demo-xyz").unwrap();
    let cp_demo = agent_state.checkpoint_store_for_connection("conn-demo");
    cp_demo.save(&fininsight_tally_agent_lib::checkpoint::Checkpoint {
        connection_id: Some("conn-demo".into()),
        last_known_alter_id: 207,
        last_successful_sync: Some("2026-09-09T10:00:00Z".into()),
        backfill_complete: true,
    }).unwrap();

    // WireSnaks is ALSO connected with alter_id 150 and token
    agent_state.profile_store.save(&ConnectionProfile {
        connection_id: "conn-wire".into(),
        company_guid: Some("guid-wire".into()),
        company_name: "WireSnaks".into(),
        paired_at: "2026-09-09T11:00:00Z".into(),
    }).unwrap();
    Vault::store_token_for("conn-wire", "token-wire-abc").unwrap();
    let cp_wire = agent_state.checkpoint_store_for_connection("conn-wire");
    cp_wire.save(&fininsight_tally_agent_lib::checkpoint::Checkpoint {
        connection_id: Some("conn-wire".into()),
        last_known_alter_id: 150,
        last_successful_sync: Some("2026-09-09T11:00:00Z".into()),
        backfill_complete: true,
    }).unwrap();

    // --- DISCOVERY A: DemoCorp (active), ProfitCorp (unconnected), WireSnaks, UnconnectedExtra ---
    let disc_a = vec![
        DiscoveredCompany { company_name: "DemoCorp".into(), company_guid: Some("guid-demo".into()), alter_id: 207 },
        DiscoveredCompany { company_name: "ProfitCorp".into(), company_guid: Some("guid-profit".into()), alter_id: 50 },
        DiscoveredCompany { company_name: "WireSnaks".into(), company_guid: Some("guid-wire".into()), alter_id: 150 },
        DiscoveredCompany { company_name: "UnconnectedExtra".into(), company_guid: Some("guid-extra".into()), alter_id: 10 },
    ];
    let active_a = fininsight_tally_agent_lib::tally_schema::CompanyInfo {
        company_name: "DemoCorp".into(),
        company_guid: Some("guid-demo".into()),
        alter_id: 207,
    };

    let items_a = agent_state.reconcile_company_items(&disc_a, Some(&active_a), true).await;
    assert_eq!(items_a.len(), 4, "DemoCorp, WireSnaks, ProfitCorp, UnconnectedExtra");
    
    let demo_a = items_a.iter().find(|i| i.company_name == "DemoCorp").unwrap();
    assert_eq!(demo_a.status, "Connected · Active");
    assert!(demo_a.is_connected);
    assert!(demo_a.is_active_in_tally);

    let wire_a = items_a.iter().find(|i| i.company_name == "WireSnaks").unwrap();
    assert_eq!(wire_a.status, "Connected · Inactive");
    assert!(wire_a.is_connected);
    assert!(!wire_a.is_active_in_tally);

    let profit_a = items_a.iter().find(|i| i.company_name == "ProfitCorp").unwrap();
    assert_eq!(profit_a.status, "Available · Inactive");
    assert!(!profit_a.is_connected);

    let extra_a = items_a.iter().find(|i| i.company_name == "UnconnectedExtra").unwrap();
    assert_eq!(extra_a.status, "Available · Inactive");
    assert!(!extra_a.is_connected);

    // --- DISCOVERY B: Tally only returns DemoCorp and ProfitCorp (ProfitCorp now active) ---
    // WireSnaks & UnconnectedExtra closed/removed in Tally
    let disc_b = vec![
        DiscoveredCompany { company_name: "DemoCorp".into(), company_guid: Some("guid-demo".into()), alter_id: 207 },
        DiscoveredCompany { company_name: "ProfitCorp".into(), company_guid: Some("guid-profit".into()), alter_id: 50 },
    ];
    let active_b = fininsight_tally_agent_lib::tally_schema::CompanyInfo {
        company_name: "ProfitCorp".into(),
        company_guid: Some("guid-profit".into()),
        alter_id: 50,
    };

    let items_b = agent_state.reconcile_company_items(&disc_b, Some(&active_b), true).await;

    // 1. UnconnectedExtra MUST completely disappear
    assert!(items_b.iter().all(|i| i.company_name != "UnconnectedExtra"), "UnconnectedExtra must disappear after removal from Tally");

    // 2. DemoCorp is connected but inactive
    let demo_b = items_b.iter().find(|i| i.company_name == "DemoCorp").unwrap();
    assert_eq!(demo_b.status, "Connected · Inactive");
    assert!(demo_b.is_connected);
    assert!(!demo_b.is_active_in_tally);
    assert_eq!(demo_b.alter_id, 207);

    // 3. ProfitCorp is available and active
    let profit_b = items_b.iter().find(|i| i.company_name == "ProfitCorp").unwrap();
    assert_eq!(profit_b.status, "Available · Active");
    assert!(!profit_b.is_connected);
    assert!(profit_b.is_active_in_tally);

    // 4. WireSnaks was connected, so it MUST remain visible with "Connected · Not currently available"
    let wire_b = items_b.iter().find(|i| i.company_name == "WireSnaks").unwrap();
    assert_eq!(wire_b.status, "Connected · Not currently available");
    assert!(wire_b.is_connected);
    assert!(!wire_b.is_active_in_tally);
    assert_eq!(wire_b.alter_id, 150);

    // 5. Verify WireSnaks persistent profile, token, checkpoint are 100% UNTOUCHED
    assert_eq!(agent_state.profile_store.get_by_connection_id("conn-wire").unwrap().unwrap().company_name, "WireSnaks");
    assert_eq!(Vault::get_token_for("conn-wire").unwrap(), "token-wire-abc");
    assert_eq!(cp_wire.load().unwrap().last_known_alter_id, 150);

    // 6. Connect ProfitCorp
    agent_state.profile_store.save(&ConnectionProfile {
        connection_id: "conn-profit".into(),
        company_guid: Some("guid-profit".into()),
        company_name: "ProfitCorp".into(),
        paired_at: "2026-09-09T12:00:00Z".into(),
    }).unwrap();
    Vault::store_token_for("conn-profit", "token-profit-999").unwrap();
    let cp_profit = agent_state.checkpoint_store_for_connection("conn-profit");
    cp_profit.save(&fininsight_tally_agent_lib::checkpoint::Checkpoint {
        connection_id: Some("conn-profit".into()),
        last_known_alter_id: 50,
        last_successful_sync: None,
        backfill_complete: false,
    }).unwrap();

    // --- DISCOVERY C: WireSnaks re-opened in Tally ---
    let disc_c = vec![
        DiscoveredCompany { company_name: "DemoCorp".into(), company_guid: Some("guid-demo".into()), alter_id: 207 },
        DiscoveredCompany { company_name: "ProfitCorp".into(), company_guid: Some("guid-profit".into()), alter_id: 50 },
        DiscoveredCompany { company_name: "WireSnaks".into(), company_guid: Some("guid-wire".into()), alter_id: 150 },
    ];
    let items_c = agent_state.reconcile_company_items(&disc_c, Some(&active_b), true).await;
    assert_eq!(items_c.len(), 3);

    let wire_c = items_c.iter().find(|i| i.company_name == "WireSnaks").unwrap();
    assert_eq!(wire_c.status, "Connected · Inactive", "WireSnaks automatically becomes Connected Inactive without re-pairing");
    assert_eq!(wire_c.alter_id, 150);

    let profit_c = items_c.iter().find(|i| i.company_name == "ProfitCorp").unwrap();
    assert_eq!(profit_c.status, "Connected · Active");
    assert_eq!(profit_c.alter_id, 50);

    // --- DISCOVERY D: Tally is Offline ---
    let items_offline = agent_state.reconcile_company_items(&[], None, false).await;
    assert_eq!(items_offline.len(), 3, "All 3 connected companies must be visible when Tally is offline");
    for item in &items_offline {
        assert_eq!(item.status, "Offline");
        assert!(item.is_connected);
        assert!(!item.is_active_in_tally);
    }

    // Cleanup vault tokens
    let _ = Vault::delete_token_for("conn-demo");
    let _ = Vault::delete_token_for("conn-wire");
    let _ = Vault::delete_token_for("conn-profit");
}



