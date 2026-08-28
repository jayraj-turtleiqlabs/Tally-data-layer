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
    fn parse_ledgers(&self, xml: &str) -> Result<Vec<LedgerRecord>, TallyError>;
    fn parse_vouchers(&self, xml: &str) -> Result<Vec<VoucherRecord>, TallyError>;
    fn parse_delta_records(&self, xml: &str) -> Result<Vec<DeltaRecord>, TallyError>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompanyInfo {
    pub company_name: String,
    pub alter_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LedgerRecord {
    pub name: String,
    pub parent: String,
    pub alter_id: u64,
    pub opening_balance: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoucherRecord {
    pub voucher_number: String,
    pub voucher_type: String,
    pub date: String,
    pub alter_id: u64,
    pub amount: Option<f64>,
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

fn parse_company_info_common(xml: &str) -> Result<CompanyInfo, TallyError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut company_name = None;
    let mut alter_id = None;
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
                if name == "COMPANY" && company_name.is_none() {
                    company_name = attribute_value(&e, "NAME")?;
                }
                current_tag = name;
            }
            Ok(Event::Text(e)) => {
                let text = e.unescape().map_err(|_| {
                    TallyError::MalformedXml("invalid text encoding".into())
                })?;
                match current_tag.as_str() {
                    "NAME" | "COMPANYNAME" if in_company && company_name.is_none() => {
                        company_name = Some(text.to_string());
                    }
                    "ALTERID" | "ALTERID.LIST" => {
                        if let Ok(id) = text.trim().parse::<u64>() {
                            alter_id = Some(id);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "ALTERID" {
                    // self-closing tag variant
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
    })
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

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag_name == tag {
                    in_ledger = true;
                    name.clear();
                    parent.clear();
                    alter_id = 0;
                    opening_balance = None;
                    if let Some(value) = attribute_value(&e, "NAME")? {
                        name = value;
                    }
                }
                current_tag = tag_name;
            }
            Ok(Event::End(e)) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag_name == tag && in_ledger {
                    if !name.is_empty() {
                        records.push(LedgerRecord {
                            name: name.clone(),
                            parent: parent.clone(),
                            alter_id,
                            opening_balance,
                        });
                    }
                    in_ledger = false;
                }
            }
            Ok(Event::Text(e)) if in_ledger => {
                let text = e.unescape().map_err(|_| {
                    TallyError::MalformedXml("invalid text encoding".into())
                })?;
                match current_tag.as_str() {
                    "NAME" => name = text.to_string(),
                    "PARENT" => parent = text.to_string(),
                    "ALTERID" => {
                        alter_id = text.trim().parse().unwrap_or(0);
                    }
                    "OPENINGBALANCE" => {
                        opening_balance = text.trim().parse().ok();
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
        if attribute.key.as_ref() == key.as_bytes() {
            let value = attribute
                .unescape_value()
                .map_err(|e| TallyError::MalformedXml(e.to_string()))?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
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

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag_name == tag {
                    in_voucher = true;
                    voucher_number.clear();
                    voucher_type.clear();
                    date.clear();
                    alter_id = 0;
                    amount = None;
                }
                current_tag = tag_name;
            }
            Ok(Event::End(e)) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag_name == tag && in_voucher {
                    records.push(VoucherRecord {
                        voucher_number: voucher_number.clone(),
                        voucher_type: voucher_type.clone(),
                        date: date.clone(),
                        alter_id,
                        amount,
                    });
                    in_voucher = false;
                }
            }
            Ok(Event::Text(e)) if in_voucher => {
                let text = e.unescape().map_err(|_| {
                    TallyError::MalformedXml("invalid text encoding".into())
                })?;
                match current_tag.as_str() {
                    "VOUCHERNUMBER" => voucher_number = text.to_string(),
                    "VOUCHERTYPENAME" => voucher_type = text.to_string(),
                    "DATE" => date = text.to_string(),
                    "ALTERID" => alter_id = text.trim().parse().unwrap_or(0),
                    "AMOUNT" => amount = text.trim().parse().ok(),
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
}
