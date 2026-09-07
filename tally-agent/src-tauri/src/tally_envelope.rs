//! Tally XML envelope builder — Export-only by design.
//!
//! There is intentionally no Import variant anywhere in this module.
//! Any attempt to write to Tally must be impossible at compile time.

/// The only envelope direction this agent supports.
/// There is no `Import` variant — this is enforced at the type level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeDirection {
    Export,
}

/// Specific Export report/collection identifiers supported by the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportReport {
    CompanyInfo,
    Ledgers,
    Groups,
    Vouchers,
    DeltaCollection,
    CustomLedgers,
    CustomVouchers,
    CustomDeltaCollection,
}

/// A fully-built Tally XML request envelope (always Export).
#[derive(Debug, Clone)]
pub struct ExportEnvelope {
    pub direction: EnvelopeDirection,
    pub report: ExportReport,
    pub xml: String,
}

pub const TDL_CUSTOM_VOUCHERS_REPORT: &str = r#"          <REPORT NAME="FinInsightVoucherReport">
            <FORMS>FinInsightVoucherForm</FORMS>
          </REPORT>
          <FORM NAME="FinInsightVoucherForm">
            <PARTS>FinInsightVoucherPart</PARTS>
          </FORM>
          <PART NAME="FinInsightVoucherPart">
            <LINES>FinInsightVoucherLine</LINES>
            <REPEAT>FinInsightVoucherLine : FinInsightVoucherColl</REPEAT>
            <SCROLLED>Vertical</SCROLLED>
          </PART>
          <LINE NAME="FinInsightVoucherLine">
            <FIELDS>FldVoucherNumber, FldVoucherType, FldDate, FldAlterId, FldAmount, FldPartyName</FIELDS>
          </LINE>
          <FIELD NAME="FldVoucherNumber">
            <SET>$VoucherNumber</SET>
            <XMLTAG>VOUCHERNUMBER</XMLTAG>
          </FIELD>
          <FIELD NAME="FldVoucherType">
            <SET>$VoucherTypeName</SET>
            <XMLTAG>VOUCHERTYPENAME</XMLTAG>
          </FIELD>
          <FIELD NAME="FldDate">
            <SET>$Date</SET>
            <XMLTAG>DATE</XMLTAG>
          </FIELD>
          <FIELD NAME="FldAlterId">
            <SET>$AlterID</SET>
            <XMLTAG>ALTERID</XMLTAG>
          </FIELD>
          <FIELD NAME="FldAmount">
            <SET>$Amount</SET>
            <XMLTAG>AMOUNT</XMLTAG>
          </FIELD>
          <FIELD NAME="FldPartyName">
            <SET>if $$IsEmpty:$PartyLedgerName then $PartyName else $PartyLedgerName</SET>
            <XMLTAG>PARTYLEDGERNAME</XMLTAG>
          </FIELD>"#;

pub const TDL_CUSTOM_LEDGERS_REPORT: &str = r#"          <REPORT NAME="FinInsightLedgerReport">
            <FORMS>FinInsightLedgerForm</FORMS>
          </REPORT>
          <FORM NAME="FinInsightLedgerForm">
            <PARTS>FinInsightLedgerPart</PARTS>
          </FORM>
          <PART NAME="FinInsightLedgerPart">
            <LINES>FinInsightLedgerLine</LINES>
            <REPEAT>FinInsightLedgerLine : FinInsightLedgerColl</REPEAT>
            <SCROLLED>Vertical</SCROLLED>
          </PART>
          <LINE NAME="FinInsightLedgerLine">
            <FIELDS>FldLedgerName, FldParent, FldAlterId, FldOpeningBalance</FIELDS>
          </LINE>
          <FIELD NAME="FldLedgerName">
            <SET>$Name</SET>
            <XMLTAG>NAME</XMLTAG>
          </FIELD>
          <FIELD NAME="FldParent">
            <SET>$Parent</SET>
            <XMLTAG>PARENT</XMLTAG>
          </FIELD>
          <FIELD NAME="FldAlterId">
            <SET>$AlterID</SET>
            <XMLTAG>ALTERID</XMLTAG>
          </FIELD>
          <FIELD NAME="FldOpeningBalance">
            <SET>$OpeningBalance</SET>
            <XMLTAG>OPENINGBALANCE</XMLTAG>
          </FIELD>"#;

impl ExportEnvelope {
    pub fn build(report: ExportReport, alter_id_from: Option<u64>) -> Self {
        let (filter_element, system_formula) = match alter_id_from {
            Some(id) => (
                "\n            <FILTERS>AlterIdFilter</FILTERS>".to_string(),
                format!("\n          <SYSTEM TYPE=\"Formulae\" NAME=\"AlterIdFilter\">$AlterID &gt; {id}</SYSTEM>"),
            ),
            None => (String::new(), String::new()),
        };

        let xml = match report {
            ExportReport::CustomVouchers | ExportReport::CustomDeltaCollection => {
                let report_name = match report {
                    ExportReport::CustomVouchers => "FinInsightVoucherReport",
                    ExportReport::CustomDeltaCollection => "FinInsightDeltaVoucherReport",
                    _ => unreachable!(),
                };
                let coll_name = "FinInsightVoucherColl";
                format!(
                    r#"<ENVELOPE>
  <HEADER>
    <VERSION>1</VERSION>
    <TALLYREQUEST>Export</TALLYREQUEST>
    <TYPE>Report</TYPE>
    <ID>{report_name}</ID>
  </HEADER>
  <BODY>
    <DESC>
      <STATICVARIABLES>
        <SVEXPORTFORMAT>$$SysName:XML</SVEXPORTFORMAT>
      </STATICVARIABLES>
      <TDL>
        <TDLMESSAGE>
{TDL_CUSTOM_VOUCHERS_REPORT}
          <COLLECTION NAME="{coll_name}">
            <TYPE>Voucher</TYPE>{filter_element}
          </COLLECTION>{system_formula}
        </TDLMESSAGE>
      </TDL>
    </DESC>
  </BODY>
</ENVELOPE>"#
                )
            }
            ExportReport::CustomLedgers => {
                let report_name = "FinInsightLedgerReport";
                let coll_name = "FinInsightLedgerColl";
                format!(
                    r#"<ENVELOPE>
  <HEADER>
    <VERSION>1</VERSION>
    <TALLYREQUEST>Export</TALLYREQUEST>
    <TYPE>Report</TYPE>
    <ID>{report_name}</ID>
  </HEADER>
  <BODY>
    <DESC>
      <STATICVARIABLES>
        <SVEXPORTFORMAT>$$SysName:XML</SVEXPORTFORMAT>
      </STATICVARIABLES>
      <TDL>
        <TDLMESSAGE>
{TDL_CUSTOM_LEDGERS_REPORT}
          <COLLECTION NAME="{coll_name}">
            <TYPE>Ledger</TYPE>{filter_element}
          </COLLECTION>{system_formula}
        </TDLMESSAGE>
      </TDL>
    </DESC>
  </BODY>
</ENVELOPE>"#
                )
            }
            _ => {
                let (collection_name, entity_type, fetch_fields) = match report {
                    ExportReport::CompanyInfo => ("Collection of Companies", "Company", "NAME, ALTERID"),
                    ExportReport::Ledgers => ("Ledgers", "Ledger", "NAME, PARENT, ALTERID, OPENINGBALANCE"),
                    ExportReport::Groups => ("Groups", "Group", "NAME, PARENT, ALTERID"),
                    ExportReport::Vouchers => ("Vouchers", "Voucher", "VOUCHERNUMBER, VOUCHERTYPENAME, DATE, ALTERID, AMOUNT, PARTYLEDGERNAME, PARTYNAME"),
                    ExportReport::DeltaCollection => ("AlterIds", "Voucher", "VOUCHERNUMBER, VOUCHERTYPENAME, DATE, ALTERID, AMOUNT, PARTYLEDGERNAME, PARTYNAME"),
                    _ => unreachable!(),
                };

                format!(
                    r#"<ENVELOPE>
  <HEADER>
    <VERSION>1</VERSION>
    <TALLYREQUEST>Export</TALLYREQUEST>
    <TYPE>Collection</TYPE>
    <ID>{collection_name}</ID>
  </HEADER>
  <BODY>
    <DESC>
      <STATICVARIABLES>
        <SVEXPORTFORMAT>$$SysName:XML</SVEXPORTFORMAT>
      </STATICVARIABLES>
      <TDL>
        <TDLMESSAGE>
          <COLLECTION NAME="{collection_name}" ISMODIFY="No">
            <TYPE>{entity_type}</TYPE>
            <FETCH>{fetch_fields}</FETCH>{filter_element}
          </COLLECTION>{system_formula}
        </TDLMESSAGE>
      </TDL>
    </DESC>
  </BODY>
</ENVELOPE>"#
                )
            }
        };

        Self {
            direction: EnvelopeDirection::Export,
            report,
            xml,
        }
    }

    /// Ping/status envelope — still Export, never Import.
    /// Uses TDL Collection query compatible with both TallyPrime and ERP 9.
    pub fn ping() -> Self {
        Self::build(ExportReport::CompanyInfo, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_direction_only_has_export() {
        // Compile-time guarantee: EnvelopeDirection has exactly one variant.
        let dir = EnvelopeDirection::Export;
        assert_eq!(dir, EnvelopeDirection::Export);
    }

    #[test]
    fn all_envelopes_contain_export_request_type() {
        for report in [
            ExportReport::CompanyInfo,
            ExportReport::Ledgers,
            ExportReport::Groups,
            ExportReport::Vouchers,
            ExportReport::DeltaCollection,
            ExportReport::CustomLedgers,
            ExportReport::CustomVouchers,
            ExportReport::CustomDeltaCollection,
        ] {
            let env = ExportEnvelope::build(report, Some(100));
            assert!(
                env.xml.contains("<TALLYREQUEST>Export</TALLYREQUEST>"),
                "Report {:?} must use Export",
                report
            );
            assert!(
                !env.xml.to_ascii_lowercase().contains("import"),
                "Report {:?} must never contain Import",
                report
            );
        }
    }

    #[test]
    fn custom_vouchers_envelope_contains_inline_tdl() {
        let env = ExportEnvelope::build(ExportReport::CustomVouchers, None);
        assert!(env.xml.contains("<TYPE>Report</TYPE>"));
        assert!(env.xml.contains("<ID>FinInsightVoucherReport</ID>"));
        assert!(env.xml.contains("<REPORT NAME=\"FinInsightVoucherReport\">"));
        assert!(env.xml.contains("<FIELD NAME=\"FldVoucherNumber\">"));
        assert!(env.xml.contains("<XMLTAG>VOUCHERNUMBER</XMLTAG>"));
        assert!(env.xml.contains("<FIELD NAME=\"FldPartyName\">"));
        assert!(env.xml.contains("<XMLTAG>PARTYLEDGERNAME</XMLTAG>"));
        assert!(env.xml.contains("<COLLECTION NAME=\"FinInsightVoucherColl\">"));
        assert!(!env.xml.to_ascii_lowercase().contains("import"));
    }

    #[test]
    fn custom_ledgers_envelope_contains_inline_tdl() {
        let env = ExportEnvelope::build(ExportReport::CustomLedgers, None);
        assert!(env.xml.contains("<TYPE>Report</TYPE>"));
        assert!(env.xml.contains("<ID>FinInsightLedgerReport</ID>"));
        assert!(env.xml.contains("<REPORT NAME=\"FinInsightLedgerReport\">"));
        assert!(env.xml.contains("<FIELD NAME=\"FldLedgerName\">"));
        assert!(env.xml.contains("<XMLTAG>NAME</XMLTAG>"));
        assert!(env.xml.contains("<COLLECTION NAME=\"FinInsightLedgerColl\">"));
    }

    #[test]
    fn custom_delta_envelope_contains_filter_and_formula() {
        let env = ExportEnvelope::build(ExportReport::CustomDeltaCollection, Some(500));
        assert!(env.xml.contains("<TYPE>Report</TYPE>"));
        assert!(env.xml.contains("<ID>FinInsightDeltaVoucherReport</ID>"));
        assert!(env.xml.contains("<FILTERS>AlterIdFilter</FILTERS>"));
        assert!(env.xml.contains(r#"<SYSTEM TYPE="Formulae" NAME="AlterIdFilter">$AlterID &gt; 500</SYSTEM>"#));
    }

    #[test]
    fn ping_envelope_is_export_only() {
        let env = ExportEnvelope::ping();
        assert!(env.xml.contains("<TALLYREQUEST>Export</TALLYREQUEST>"));
        assert_eq!(env.direction, EnvelopeDirection::Export);
    }

    #[test]
    fn delta_sync_envelope_defines_system_formula_outside_collection() {
        let env = ExportEnvelope::build(ExportReport::DeltaCollection, Some(42));

        // Check collection references the filter formula
        assert!(
            env.xml.contains("<FILTERS>AlterIdFilter</FILTERS>"),
            "COLLECTION must reference AlterIdFilter"
        );
        // Check system formula definition exists with matching name and escaping
        assert!(
            env.xml.contains(r#"<SYSTEM TYPE="Formulae" NAME="AlterIdFilter">$AlterID &gt; 42</SYSTEM>"#),
            "TDLMESSAGE must contain the SYSTEM formula definition"
        );

        // Verify structure: SYSTEM formula must be a sibling of COLLECTION inside TDLMESSAGE,
        // placed outside </COLLECTION> rather than nested inside it.
        let collection_end = env.xml.find("</COLLECTION>").expect("missing </COLLECTION>");
        let formula_pos = env
            .xml
            .find(r#"<SYSTEM TYPE="Formulae" NAME="AlterIdFilter">"#)
            .expect("missing SYSTEM formula");
        let tdl_message_end = env.xml.find("</TDLMESSAGE>").expect("missing </TDLMESSAGE>");

        assert!(
            formula_pos > collection_end,
            "SYSTEM formula must be placed outside the COLLECTION definition"
        );
        assert!(
            formula_pos < tdl_message_end,
            "SYSTEM formula must be placed inside TDLMESSAGE"
        );
    }

    #[test]
    fn non_filtered_envelope_does_not_include_filters_or_formula() {
        let env = ExportEnvelope::build(ExportReport::Ledgers, None);
        assert!(!env.xml.contains("<FILTERS>"));
        assert!(!env.xml.contains("<SYSTEM TYPE=\"Formulae\""));
    }
}
