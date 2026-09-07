//! Performance benchmark and fallback tests for TDL-based custom reports.

use std::time::Instant;
use fininsight_tally_agent_lib::tally_client::{TallyClient, TallyEndpoint};
use fininsight_tally_agent_lib::tally_schema::{adapter_for_xml, VoucherRecord};
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Generates a verbose default Tally voucher collection XML (simulating default Tally export).
fn generate_default_verbose_vouchers_xml(count: usize) -> String {
    let mut xml = String::from("<ENVELOPE><BODY><DATA><COLLECTION>\n");
    for i in 1..=count {
        xml.push_str(&format!(
            r#"  <VOUCHER VCHTYPE="Sales" ACTION="Create" OBJVIEW="Invoice Voucher View">
    <DATE>20260401</DATE>
    <VOUCHERTYPENAME>Sales</VOUCHERTYPENAME>
    <VOUCHERNUMBER>INV-{i:05}</VOUCHERNUMBER>
    <ALTERID>{i}</ALTERID>
    <AMOUNT>15000.00</AMOUNT>
    <GUID>00000000-0000-0000-0000-{i:012}</GUID>
    <STATENAME>Maharashtra</STATENAME>
    <COUNTRYOFRESIDENCE>India</COUNTRYOFRESIDENCE>
    <PARTYNAME>Acme Customer {i}</PARTYNAME>
    <PARTYLEDGERNAME>Acme Customer {i}</PARTYLEDGERNAME>
    <BASECURRENCYNAME>INR</BASECURRENCYNAME>
    <NARRATION>Payment for invoice {i}</NARRATION>
    <ALLLEDGERENTRIES.LIST>
      <LEDGERNAME>Acme Customer {i}</LEDGERNAME>
      <ISDEEMEDPOSITIVE>No</ISDEEMEDPOSITIVE>
      <ISPARTYLEDGER>Yes</ISPARTYLEDGER>
      <AMOUNT>-15000.00</AMOUNT>
      <BILLALLOCATIONS.LIST>
        <NAME>INV-{i:05}</NAME>
        <BILLTYPE>New Ref</BILLTYPE>
        <AMOUNT>-15000.00</AMOUNT>
      </BILLALLOCATIONS.LIST>
    </ALLLEDGERENTRIES.LIST>
    <ALLLEDGERENTRIES.LIST>
      <LEDGERNAME>Sales Account</LEDGERNAME>
      <ISDEEMEDPOSITIVE>Yes</ISDEEMEDPOSITIVE>
      <ISPARTYLEDGER>No</ISPARTYLEDGER>
      <AMOUNT>15000.00</AMOUNT>
    </ALLLEDGERENTRIES.LIST>
    <UDF:LOCALNAME.LIST>
      <UDF:LOCALNAME>Default Local Name</UDF:LOCALNAME>
    </UDF:LOCALNAME.LIST>
  </VOUCHER>
"#
        ));
    }
    xml.push_str("</COLLECTION></DATA></BODY></ENVELOPE>");
    xml
}

/// Generates a lean custom TDL report XML (simulating the dynamic TDL report output).
fn generate_custom_tdl_vouchers_xml(count: usize) -> String {
    let mut xml = String::from("<ENVELOPE><BODY><DATA><FININSIGHTVOUCHERREPORT>\n");
    for i in 1..=count {
        xml.push_str(&format!(
            r#"  <FININSIGHTVOUCHERLINE>
    <VOUCHERNUMBER>INV-{i:05}</VOUCHERNUMBER>
    <VOUCHERTYPENAME>Sales</VOUCHERTYPENAME>
    <PARTYLEDGERNAME>Acme Customer {i}</PARTYLEDGERNAME>
    <DATE>20260401</DATE>
    <ALTERID>{i}</ALTERID>
    <AMOUNT>15000.00</AMOUNT>
  </FININSIGHTVOUCHERLINE>
"#
        ));
    }
    xml.push_str("</FININSIGHTVOUCHERREPORT></DATA></BODY></ENVELOPE>");
    xml
}

#[test]
fn benchmark_payload_size_and_parse_time() {
    let record_count = 200;
    let verbose_xml = generate_default_verbose_vouchers_xml(record_count);
    let lean_xml = generate_custom_tdl_vouchers_xml(record_count);

    let verbose_bytes = verbose_xml.len();
    let lean_bytes = lean_xml.len();

    println!("Verbose Payload Size: {} bytes ({} KB)", verbose_bytes, verbose_bytes / 1024);
    println!("Lean TDL Payload Size: {} bytes ({} KB)", lean_bytes, lean_bytes / 1024);

    // Verify payload size reduction is at least 60%
    assert!(
        lean_bytes < verbose_bytes / 2,
        "Lean payload ({} bytes) should be less than half the verbose payload ({} bytes)",
        lean_bytes,
        verbose_bytes
    );

    let size_reduction_pct = ((verbose_bytes - lean_bytes) as f64 / verbose_bytes as f64) * 100.0;
    println!("Payload Size Reduction: {:.2}%", size_reduction_pct);

    // Warm-up
    let adapter_v = adapter_for_xml(&verbose_xml);
    let adapter_l = adapter_for_xml(&lean_xml);
    let _ = adapter_v.parse_vouchers(&verbose_xml).unwrap();
    let _ = adapter_l.parse_vouchers(&lean_xml).unwrap();

    // Benchmark parse times over 50 iterations
    let iterations = 50;

    let start_verbose = Instant::now();
    for _ in 0..iterations {
        let records = adapter_v.parse_vouchers(&verbose_xml).unwrap();
        assert_eq!(records.len(), record_count);
    }
    let duration_verbose = start_verbose.elapsed();

    let start_lean = Instant::now();
    for _ in 0..iterations {
        let records = adapter_l.parse_vouchers(&lean_xml).unwrap();
        assert_eq!(records.len(), record_count);
    }
    let duration_lean = start_lean.elapsed();

    println!(
        "Verbose Parse Time ({} iterations): {:?}",
        iterations, duration_verbose
    );
    println!(
        "Lean TDL Parse Time ({} iterations): {:?}",
        iterations, duration_lean
    );

    // Verify record contents match between both approaches
    let verbose_records: Vec<VoucherRecord> = adapter_v.parse_vouchers(&verbose_xml).unwrap();
    let lean_records: Vec<VoucherRecord> = adapter_l.parse_vouchers(&lean_xml).unwrap();
    assert_eq!(verbose_records, lean_records, "Parsed records must be 100% identical");

    // Lean parsing should be faster due to significantly fewer elements/tokens to scan
    assert!(
        duration_lean <= duration_verbose,
        "Lean TDL parsing ({:?}) should be faster than verbose parsing ({:?})",
        duration_lean,
        duration_verbose
    );
}

#[tokio::test]
async fn fallback_to_default_export_when_custom_tdl_fails() {
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let tally_endpoint = TallyEndpoint::new("127.0.0.1", tally_port).unwrap();
    let tally_client = TallyClient::new(tally_endpoint).unwrap();

    // 1. Custom TDL request returns a TDL syntax error
    Mock::given(method("POST"))
        .and(body_string_contains("FinInsightVoucherReport"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<RESPONSE><LINEERROR>Error in TDL: Unknown Report FinInsightVoucherReport</LINEERROR></RESPONSE>"#,
        ))
        .expect(1)
        .mount(&tally_server)
        .await;

    // 2. Default export fallback succeeds
    let default_vouchers_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <VOUCHER>
      <VOUCHERNUMBER>INV-FALLBACK-01</VOUCHERNUMBER>
      <VOUCHERTYPENAME>Sales</VOUCHERTYPENAME>
      <PARTYLEDGERNAME>Fallback Customer</PARTYLEDGERNAME>
      <DATE>20260401</DATE>
      <ALTERID>999</ALTERID>
      <AMOUNT>42000.00</AMOUNT>
    </VOUCHER>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .and(body_string_contains("<ID>Vouchers</ID>"))
        .respond_with(ResponseTemplate::new(200).set_body_string(default_vouchers_xml))
        .expect(1)
        .mount(&tally_server)
        .await;

    // Execute voucher export
    let vouchers = tally_client.export_vouchers().await.expect("export vouchers via fallback");
    assert_eq!(vouchers.len(), 1);
    assert_eq!(vouchers[0].voucher_number, "INV-FALLBACK-01");
    assert_eq!(vouchers[0].party_name, Some("Fallback Customer".to_string()));
    assert_eq!(vouchers[0].alter_id, 999);
    assert_eq!(vouchers[0].amount, Some(42000.00));
}

#[tokio::test]
async fn fallback_to_default_ledger_export_when_custom_tdl_fails() {
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let tally_endpoint = TallyEndpoint::new("127.0.0.1", tally_port).unwrap();
    let tally_client = TallyClient::new(tally_endpoint).unwrap();

    // 1. Custom TDL request fails with 500 error
    Mock::given(method("POST"))
        .and(body_string_contains("FinInsightLedgerReport"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&tally_server)
        .await;

    // 2. Default export fallback succeeds
    let default_ledgers_xml = r#"<ENVELOPE><BODY><DATA><COLLECTION>
    <LEDGER NAME="Bank Account">
      <PARENT>Bank Accounts</PARENT>
      <ALTERID>888</ALTERID>
      <OPENINGBALANCE>100000.00</OPENINGBALANCE>
    </LEDGER>
    </COLLECTION></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .and(body_string_contains("<ID>Ledgers</ID>"))
        .respond_with(ResponseTemplate::new(200).set_body_string(default_ledgers_xml))
        .expect(1)
        .mount(&tally_server)
        .await;

    let ledgers = tally_client.export_ledgers().await.expect("export ledgers via fallback");
    assert_eq!(ledgers.len(), 1);
    assert_eq!(ledgers[0].name, "Bank Account");
    assert_eq!(ledgers[0].alter_id, 888);
}

#[tokio::test]
async fn custom_tdl_fast_path_when_supported() {
    let tally_server = MockServer::start().await;
    let tally_port = tally_server.address().port();
    let tally_endpoint = TallyEndpoint::new("127.0.0.1", tally_port).unwrap();
    let tally_client = TallyClient::new(tally_endpoint).unwrap();

    let custom_xml = r#"<ENVELOPE><BODY><DATA><FININSIGHTVOUCHERREPORT>
    <FININSIGHTVOUCHERLINE>
      <VOUCHERNUMBER>INV-FAST-01</VOUCHERNUMBER>
      <VOUCHERTYPENAME>Sales</VOUCHERTYPENAME>
      <PARTYLEDGERNAME>Fast Customer</PARTYLEDGERNAME>
      <DATE>20260401</DATE>
      <ALTERID>111</ALTERID>
      <AMOUNT>5000.00</AMOUNT>
    </FININSIGHTVOUCHERLINE>
    </FININSIGHTVOUCHERREPORT></DATA></BODY></ENVELOPE>"#;

    Mock::given(method("POST"))
        .and(body_string_contains("FinInsightVoucherReport"))
        .respond_with(ResponseTemplate::new(200).set_body_string(custom_xml))
        .expect(1)
        .mount(&tally_server)
        .await;

    let vouchers = tally_client.export_vouchers().await.expect("export vouchers via custom TDL");
    assert_eq!(vouchers.len(), 1);
    assert_eq!(vouchers[0].voucher_number, "INV-FAST-01");
    assert_eq!(vouchers[0].party_name, Some("Fast Customer".to_string()));
    assert_eq!(vouchers[0].alter_id, 111);
}
