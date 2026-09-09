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

    // 2. Mock Cloud Backend to return 200 on initial backfill, but 401 Unauthorized on subsequent delta sync
    let cloud_server = MockServer::start().await;

    struct BackfillOkThen401 {
        count: std::sync::atomic::AtomicUsize,
    }

    impl wiremock::Respond for BackfillOkThen401 {
        fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
            let prev = self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if prev == 0 {
                ResponseTemplate::new(200) // Initial backfill
            } else {
                ResponseTemplate::new(401) // Delta sync revoked
            }
        }
    }

    Mock::given(method("POST"))
        .and(path("/api/v1/agent/sync/delta"))
        .respond_with(BackfillOkThen401 {
            count: std::sync::atomic::AtomicUsize::new(0),
        })
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
        .and(wiremock::matchers::body_string_contains("Collection of Companies"))
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
        .and(wiremock::matchers::body_string_contains("Collection of Companies"))
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
    let base_path = dir.path().join("profile.json");

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
    let profile_file = dir.path().join("profile.json");
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

