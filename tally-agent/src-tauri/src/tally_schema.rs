//! Adapter layer isolating Tally ERP 9 vs TallyPrime XML schema differences.

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::{Deserialize, Serialize};

use crate::errors::TallyError;

/// Detected Tally product variant based on response characteristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TallyVariant {
    Erp9,
    Prime,
    Unknown,
}

pub trait TallySchemaAdapter: Send + Sync {
    fn variant(&self) -> TallyVariant;
    fn parse_company_info(&self, xml: &str) -> Result<CompanyInfo, TallyError>;
    fn parse_company_list(&self, xml: &str) -> Result<Vec<DiscoveredCompany>, TallyError>;
    fn parse_ledgers(&self, xml: &str) -> Result<Vec<LedgerRecord>, TallyError>;
    fn parse_vouchers(&self, xml: &str) -> Result<Vec<VoucherRecord>, TallyError>;
    fn parse_delta_records(&self, xml: &str) -> Result<Vec<DeltaRecord>, TallyError>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiscoveredCompany {
    pub company_name: String,
    pub alter_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company_guid: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompanyInfo {
    pub company_name: String,
    pub alter_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company_guid: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LedgerRecord {
    pub name: String,
    pub parent: String,
    pub alter_id: u64,
    pub opening_balance: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub master_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoucherRecord {
    pub voucher_number: String,
    pub voucher_type: String,
    pub date: String,
    pub alter_id: u64,
    pub amount: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub party_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub party_ledger_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub master_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeltaRecord {
    pub entity_type: String,
    pub alter_id: u64,
    pub payload: serde_json::Value,
}

/// Auto-detect variant from XML root attributes / version markers.
pub fn detect_variant(xml: &str) -> TallyVariant {
    if xml.contains("TALLYPRIME") || xml.contains("TallyPrime") {
        TallyVariant::Prime
    } else if xml.contains("TALLY") || xml.contains("TallyERP") {
        TallyVariant::Erp9
    } else {
        TallyVariant::Unknown
    }
}

pub fn adapter_for_variant(variant: TallyVariant) -> Box<dyn TallySchemaAdapter> {
    match variant {
        TallyVariant::Prime => Box::new(PrimeAdapter),
        TallyVariant::Erp9 | TallyVariant::Unknown => Box::new(Erp9Adapter),
    }
}

pub fn adapter_for_xml(xml: &str) -> Box<dyn TallySchemaAdapter> {
    adapter_for_variant(detect_variant(xml))
}

struct Erp9Adapter;
struct PrimeAdapter;

impl TallySchemaAdapter for Erp9Adapter {
    fn variant(&self) -> TallyVariant {
        TallyVariant::Erp9
    }

    fn parse_company_info(&self, xml: &str) -> Result<CompanyInfo, TallyError> {
        parse_company_info_common(xml)
    }

    fn parse_company_list(&self, xml: &str) -> Result<Vec<DiscoveredCompany>, TallyError> {
        parse_company_list_common(xml)
    }

    fn parse_ledgers(&self, xml: &str) -> Result<Vec<LedgerRecord>, TallyError> {
        parse_ledgers_common(xml, "LEDGER")
    }

    fn parse_vouchers(&self, xml: &str) -> Result<Vec<VoucherRecord>, TallyError> {
        parse_vouchers_common(xml, "VOUCHER")
    }

    fn parse_delta_records(&self, xml: &str) -> Result<Vec<DeltaRecord>, TallyError> {
        parse_delta_common(xml)
    }
}

impl TallySchemaAdapter for PrimeAdapter {
    fn variant(&self) -> TallyVariant {
        TallyVariant::Prime
    }

    fn parse_company_info(&self, xml: &str) -> Result<CompanyInfo, TallyError> {
        parse_company_info_common(xml)
    }

    fn parse_company_list(&self, xml: &str) -> Result<Vec<DiscoveredCompany>, TallyError> {
        parse_company_list_common(xml)
    }

    fn parse_ledgers(&self, xml: &str) -> Result<Vec<LedgerRecord>, TallyError> {
        // TallyPrime may use LEDGER.LIST wrapper — handled in common parser
        parse_ledgers_common(xml, "LEDGER")
    }

    fn parse_vouchers(&self, xml: &str) -> Result<Vec<VoucherRecord>, TallyError> {
        parse_vouchers_common(xml, "VOUCHER")
    }

    fn parse_delta_records(&self, xml: &str) -> Result<Vec<DeltaRecord>, TallyError> {
        parse_delta_common(xml)
    }
}

fn parse_company_list_common(xml: &str) -> Result<Vec<DiscoveredCompany>, TallyError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut companies = Vec::new();
    let mut buf = Vec::new();
    let mut in_company = false;
    let mut current_tag = String::new();

    let mut current_name: Option<String> = None;
    let mut current_alter_id: Option<u64> = None;
    let mut current_guid: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag_name.eq_ignore_ascii_case("COMPANY") {
                    in_company = true;
                    current_name = attribute_value(&e, "NAME")?;
                    current_guid = attribute_value(&e, "GUID")?;
                    if let Some(alter_str) = attribute_value(&e, "ALTERID")? {
                        current_alter_id = alter_str.trim().parse().ok();
                    } else {
                        current_alter_id = None;
                    }
                }
                current_tag = tag_name;
            }
            Ok(Event::End(e)) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag_name.eq_ignore_ascii_case("COMPANY") && in_company {
                    if let Some(name) = current_name.take() {
                        if !name.trim().is_empty() {
                            let alter_id = current_alter_id.unwrap_or(0);
                            let guid = current_guid.take().filter(|g| !g.trim().is_empty());
                            if !companies.iter().any(|c: &DiscoveredCompany| c.company_name.eq_ignore_ascii_case(&name)) {
                                companies.push(DiscoveredCompany {
                                    company_name: name,
                                    alter_id,
                                    company_guid: guid,
                                });
                            }
                        }
                    }
                    in_company = false;
                    current_alter_id = None;
                    current_guid = None;
                }
            }
            Ok(Event::Text(e)) if in_company => {
                let text = e.unescape().map_err(|_| {
                    TallyError::MalformedXml("invalid text encoding".into())
                })?;
                let trimmed = text.trim();
                match current_tag.to_ascii_uppercase().as_str() {
                    "NAME" | "COMPANYNAME" => {
                        if !trimmed.is_empty() {
                            current_name = Some(trimmed.to_string());
                        }
                    }
                    "ALTERID" | "ALTERID.LIST" => {
                        if let Ok(id) = trimmed.parse::<u64>() {
                            current_alter_id = Some(id);
                        }
                    }
                    "GUID" | "COMPANYGUID" => {
                        if !trimmed.is_empty() {
                            current_guid = Some(trimmed.to_string());
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag_name.eq_ignore_ascii_case("COMPANY") {
                    let name = attribute_value(&e, "NAME")?;
                    let guid = attribute_value(&e, "GUID")?;
                    let alter_id = attribute_value(&e, "ALTERID")?
                        .and_then(|s| s.trim().parse().ok())
                        .unwrap_or(0);
                    if let Some(n) = name {
                        if !n.trim().is_empty() && !companies.iter().any(|c: &DiscoveredCompany| c.company_name.eq_ignore_ascii_case(&n)) {
                            companies.push(DiscoveredCompany {
                                company_name: n,
                                alter_id,
                                company_guid: guid.filter(|g| !g.trim().is_empty()),
                            });
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(TallyError::MalformedXml(e.to_string())),
            _ => {}
        }
        buf.clear();
    }

    Ok(companies)
}

fn parse_company_info_common(xml: &str) -> Result<CompanyInfo, TallyError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut company_name = None;
    let mut alter_id = None;
    let mut company_guid = None;
    let mut buf = Vec::new();
    let mut in_company = false;
    let mut current_tag = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "COMPANY" || name == "COLLECTION" {
                    in_company = true;
                }
                if name == "COMPANY" {
                    if company_name.is_none() {
                        company_name = attribute_value(&e, "NAME")?;
                    }
                    if company_guid.is_none() {
                        company_guid = attribute_value(&e, "GUID")?;
                    }
                }
                current_tag = name;
            }
            Ok(Event::Text(e)) => {
                let text = e.unescape().map_err(|_| {
                    TallyError::MalformedXml("invalid text encoding".into())
                })?;
                match current_tag.to_ascii_uppercase().as_str() {
                    "NAME" | "COMPANYNAME" if in_company && company_name.is_none() => {
                        company_name = Some(text.to_string());
                    }
                    "ALTERID" | "ALTERID.LIST" => {
                        if let Ok(id) = text.trim().parse::<u64>() {
                            alter_id = Some(id);
                        }
                    }
                    "GUID" | "COMPANYGUID" if in_company && company_guid.is_none() => {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            company_guid = Some(trimmed.to_string());
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "COMPANY" {
                    if company_name.is_none() {
                        company_name = attribute_value(&e, "NAME")?;
                    }
                    if company_guid.is_none() {
                        company_guid = attribute_value(&e, "GUID")?;
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(TallyError::MalformedXml(e.to_string()));
            }
            _ => {}
        }
        buf.clear();
    }

    let company_name = company_name.ok_or(TallyError::NoActiveCompany)?;
    let alter_id = alter_id.ok_or(TallyError::MissingAlterId)?;

    Ok(CompanyInfo {
        company_name,
        alter_id,
        company_guid,
    })
}

fn is_ledger_tag(tag_name: &str, expected_tag: &str) -> bool {
    let upper = tag_name.to_ascii_uppercase();
    upper == expected_tag.to_ascii_uppercase()
        || upper == "FININSIGHTLEDGERLINE"
        || upper.ends_with("LEDGERLINE")
        || upper == "CUSTOMLEDGER"
}

fn parse_ledgers_common(xml: &str, tag: &str) -> Result<Vec<LedgerRecord>, TallyError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut records = Vec::new();
    let mut buf = Vec::new();
    let mut in_ledger = false;
    let mut current_tag = String::new();
    let mut name = String::new();
    let mut parent = String::new();
    let mut alter_id = 0u64;
    let mut opening_balance = None;
    let mut guid = None;
    let mut master_id = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if is_ledger_tag(&tag_name, tag) {
                    in_ledger = true;
                    name.clear();
                    parent.clear();
                    alter_id = 0;
                    opening_balance = None;
                    guid = attribute_value(&e, "GUID")?;
                    master_id = attribute_value(&e, "MASTERID")?;
                    if let Some(value) = attribute_value(&e, "NAME")? {
                        name = value;
                    }
                }
                current_tag = tag_name;
            }
            Ok(Event::End(e)) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if is_ledger_tag(&tag_name, tag) && in_ledger {
                    if !name.is_empty() {
                        records.push(LedgerRecord {
                            name: name.clone(),
                            parent: parent.clone(),
                            alter_id,
                            opening_balance,
                            guid: guid.clone(),
                            master_id: master_id.clone(),
                        });
                    }
                    in_ledger = false;
                }
            }
            Ok(Event::Text(e)) if in_ledger => {
                let text = e.unescape().map_err(|_| {
                    TallyError::MalformedXml("invalid text encoding".into())
                })?;
                match current_tag.to_ascii_uppercase().as_str() {
                    "NAME" | "LEDGERNAME" => name = text.to_string(),
                    "PARENT" => parent = text.to_string(),
                    "ALTERID" => {
                        alter_id = text.trim().parse().unwrap_or(0);
                    }
                    "OPENINGBALANCE" => {
                        opening_balance = text.trim().parse().ok();
                    }
                    "GUID" => {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            guid = Some(trimmed.to_string());
                        }
                    }
                    "MASTERID" => {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            master_id = Some(trimmed.to_string());
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(TallyError::MalformedXml(e.to_string())),
            _ => {}
        }
        buf.clear();
    }

    Ok(records)
}

fn attribute_value(
    element: &quick_xml::events::BytesStart<'_>,
    key: &str,
) -> Result<Option<String>, TallyError> {
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|e| TallyError::MalformedXml(e.to_string()))?;
        if attribute.key.as_ref().eq_ignore_ascii_case(key.as_bytes()) {
            let value = attribute
                .unescape_value()
                .map_err(|e| TallyError::MalformedXml(e.to_string()))?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}

fn is_voucher_tag(tag_name: &str, expected_tag: &str) -> bool {
    let upper = tag_name.to_ascii_uppercase();
    upper == expected_tag.to_ascii_uppercase()
        || upper == "FININSIGHTVOUCHERLINE"
        || upper.ends_with("VOUCHERLINE")
        || upper == "CUSTOMVOUCHER"
}

fn parse_vouchers_common(xml: &str, tag: &str) -> Result<Vec<VoucherRecord>, TallyError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut records = Vec::new();
    let mut buf = Vec::new();
    let mut in_voucher = false;
    let mut current_tag = String::new();
    let mut voucher_number = String::new();
    let mut voucher_type = String::new();
    let mut date = String::new();
    let mut alter_id = 0u64;
    let mut amount = None;
    let mut party_name = None;
    let mut guid = None;
    let mut master_id = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if is_voucher_tag(&tag_name, tag) {
                    in_voucher = true;
                    voucher_number.clear();
                    voucher_type.clear();
                    date.clear();
                    alter_id = 0;
                    amount = None;
                    party_name = None;
                    guid = attribute_value(&e, "GUID")?;
                    master_id = attribute_value(&e, "MASTERID")?;
                }
                current_tag = tag_name;
            }
            Ok(Event::End(e)) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if is_voucher_tag(&tag_name, tag) && in_voucher {
                    records.push(VoucherRecord {
                        voucher_number: voucher_number.clone(),
                        voucher_type: voucher_type.clone(),
                        date: date.clone(),
                        alter_id,
                        amount,
                        party_name: party_name.clone(),
                        party_ledger_name: party_name.clone(),
                        guid: guid.clone(),
                        master_id: master_id.clone(),
                    });
                    in_voucher = false;
                }
            }
            Ok(Event::Text(e)) if in_voucher => {
                let text = e.unescape().map_err(|_| {
                    TallyError::MalformedXml("invalid text encoding".into())
                })?;
                match current_tag.to_ascii_uppercase().as_str() {
                    "VOUCHERNUMBER" => voucher_number = text.to_string(),
                    "VOUCHERTYPENAME" => voucher_type = text.to_string(),
                    "DATE" => date = text.to_string(),
                    "ALTERID" => alter_id = text.trim().parse().unwrap_or(0),
                    "AMOUNT" => amount = text.trim().parse().ok(),
                    "PARTYLEDGERNAME" | "PARTYNAME" => {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() && party_name.is_none() {
                            party_name = Some(trimmed.to_string());
                        }
                    }
                    "GUID" => {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            guid = Some(trimmed.to_string());
                        }
                    }
                    "MASTERID" => {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            master_id = Some(trimmed.to_string());
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(TallyError::MalformedXml(e.to_string())),
            _ => {}
        }
        buf.clear();
    }

    Ok(records)
}

fn parse_delta_common(xml: &str) -> Result<Vec<DeltaRecord>, TallyError> {
    let adapter = adapter_for_variant(detect_variant(xml));
    let mut records = Vec::new();

    let ledgers = adapter.parse_ledgers(xml)?;
    for l in ledgers {
        records.push(DeltaRecord {
            entity_type: "ledger".into(),
            alter_id: l.alter_id,
            payload: serde_json::to_value(&l).map_err(|e| TallyError::MalformedXml(e.to_string()))?,
        });
    }

    let vouchers = adapter.parse_vouchers(xml)?;
    for v in vouchers {
        records.push(DeltaRecord {
            entity_type: "voucher".into(),
            alter_id: v.alter_id,
            payload: serde_json::to_value(&v).map_err(|e| TallyError::MalformedXml(e.to_string()))?,
        });
    }

    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_erp9_company_fixture() {
        let xml = include_str!("../tests/fixtures/erp9_company_info.xml");
        let adapter = Erp9Adapter;
        let info = adapter.parse_company_info(xml).expect("parse company");
        assert_eq!(info.company_name, "Acme Traders");
        assert_eq!(info.alter_id, 4821);
    }

    #[test]
    fn parse_prime_company_fixture() {
        let xml = include_str!("../tests/fixtures/prime_company_info.xml");
        let adapter = PrimeAdapter;
        let info = adapter.parse_company_info(xml).expect("parse company");
        assert_eq!(info.company_name, "Prime Corp Ltd");
        assert_eq!(info.alter_id, 9102);
    }

    #[test]
    fn parse_ledgers_fixture() {
        let xml = include_str!("../tests/fixtures/erp9_ledgers.xml");
        let adapter = Erp9Adapter;
        let ledgers = adapter.parse_ledgers(xml).expect("parse ledgers");
        assert_eq!(ledgers.len(), 2);
        assert_eq!(ledgers[0].name, "Cash");
    }

    #[test]
    fn parse_custom_tdl_vouchers_xml() {
        let xml = r#"<ENVELOPE>
  <BODY>
    <DATA>
      <FININSIGHTVOUCHERREPORT>
        <FININSIGHTVOUCHERLINE>
          <VOUCHERNUMBER>INV-1001</VOUCHERNUMBER>
          <VOUCHERTYPENAME>Sales</VOUCHERTYPENAME>
          <PARTYLEDGERNAME>Aarav Textiles</PARTYLEDGERNAME>
          <DATE>20260401</DATE>
          <ALTERID>1234</ALTERID>
          <AMOUNT>15000.50</AMOUNT>
        </FININSIGHTVOUCHERLINE>
        <FININSIGHTVOUCHERLINE>
          <VOUCHERNUMBER>INV-1002</VOUCHERNUMBER>
          <VOUCHERTYPENAME>Receipt</VOUCHERTYPENAME>
          <PARTYLEDGERNAME>ABC Technologies Pvt Ltd</PARTYLEDGERNAME>
          <DATE>20260402</DATE>
          <ALTERID>1235</ALTERID>
          <AMOUNT>5000.00</AMOUNT>
        </FININSIGHTVOUCHERLINE>
      </FININSIGHTVOUCHERREPORT>
    </DATA>
  </BODY>
</ENVELOPE>"#;

        let adapter = PrimeAdapter;
        let vouchers = adapter.parse_vouchers(xml).expect("parse vouchers");
        assert_eq!(vouchers.len(), 2);
        assert_eq!(vouchers[0].voucher_number, "INV-1001");
        assert_eq!(vouchers[0].voucher_type, "Sales");
        assert_eq!(vouchers[0].party_name, Some("Aarav Textiles".to_string()));
        assert_eq!(vouchers[0].party_ledger_name, Some("Aarav Textiles".to_string()));
        assert_eq!(vouchers[0].date, "20260401");
        assert_eq!(vouchers[0].alter_id, 1234);
        assert_eq!(vouchers[0].amount, Some(15000.50));

        assert_eq!(vouchers[1].voucher_number, "INV-1002");
        assert_eq!(vouchers[1].voucher_type, "Receipt");
        assert_eq!(vouchers[1].party_name, Some("ABC Technologies Pvt Ltd".to_string()));
        assert_eq!(vouchers[1].party_ledger_name, Some("ABC Technologies Pvt Ltd".to_string()));
        assert_eq!(vouchers[1].date, "20260402");
        assert_eq!(vouchers[1].alter_id, 1235);
        assert_eq!(vouchers[1].amount, Some(5000.00));
    }

    #[test]
    fn parse_custom_tdl_ledgers_xml() {
        let xml = r#"<ENVELOPE>
  <BODY>
    <DATA>
      <FININSIGHTLEDGERREPORT>
        <FININSIGHTLEDGERLINE>
          <NAME>HDFC Bank</NAME>
          <PARENT>Bank Accounts</PARENT>
          <ALTERID>990</ALTERID>
          <OPENINGBALANCE>125000.00</OPENINGBALANCE>
        </FININSIGHTLEDGERLINE>
      </FININSIGHTLEDGERREPORT>
    </DATA>
  </BODY>
</ENVELOPE>"#;

        let adapter = Erp9Adapter;
        let ledgers = adapter.parse_ledgers(xml).expect("parse ledgers");
        assert_eq!(ledgers.len(), 1);
        assert_eq!(ledgers[0].name, "HDFC Bank");
        assert_eq!(ledgers[0].parent, "Bank Accounts");
        assert_eq!(ledgers[0].alter_id, 990);
        assert_eq!(ledgers[0].opening_balance, Some(125000.00));
    }

    #[test]
    fn parse_multi_company_discovery_xml() {
        let xml = r#"<ENVELOPE>
  <HEADER>
    <VERSION>1</VERSION>
    <TALLYPRIME>1</TALLYPRIME>
  </HEADER>
  <BODY>
    <DATA>
      <COLLECTION>
        <COMPANY NAME="DemoCorp" GUID="guid-demo-111">
          <NAME>DemoCorp</NAME>
          <ALTERID>230</ALTERID>
          <GUID>guid-demo-111</GUID>
        </COMPANY>
        <COMPANY NAME="ThunderClaps" GUID="guid-thunder-222">
          <NAME>ThunderClaps</NAME>
          <ALTERID>55</ALTERID>
          <GUID>guid-thunder-222</GUID>
        </COMPANY>
      </COLLECTION>
    </DATA>
  </BODY>
</ENVELOPE>"#;

        let adapter = PrimeAdapter;
        let companies = adapter.parse_company_list(xml).expect("parse company list");
        assert_eq!(companies.len(), 2);
        assert_eq!(companies[0].company_name, "DemoCorp");
        assert_eq!(companies[0].alter_id, 230);
        assert_eq!(companies[0].company_guid.as_deref(), Some("guid-demo-111"));

        assert_eq!(companies[1].company_name, "ThunderClaps");
        assert_eq!(companies[1].alter_id, 55);
        assert_eq!(companies[1].company_guid.as_deref(), Some("guid-thunder-222"));
    }

    #[test]
    fn parse_single_company_discovery_xml() {
        let xml = r#"<ENVELOPE>
  <BODY>
    <DATA>
      <COLLECTION>
        <COMPANY NAME="Solo Corp" GUID="guid-solo-999">
          <ALTERID>12</ALTERID>
        </COMPANY>
      </COLLECTION>
    </DATA>
  </BODY>
</ENVELOPE>"#;

        let adapter = Erp9Adapter;
        let companies = adapter.parse_company_list(xml).expect("parse company list");
        assert_eq!(companies.len(), 1);
        assert_eq!(companies[0].company_name, "Solo Corp");
        assert_eq!(companies[0].alter_id, 12);
        assert_eq!(companies[0].company_guid.as_deref(), Some("guid-solo-999"));
    }
}
