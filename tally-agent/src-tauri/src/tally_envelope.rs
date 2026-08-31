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
}

/// A fully-built Tally XML request envelope (always Export).
#[derive(Debug, Clone)]
pub struct ExportEnvelope {
    pub direction: EnvelopeDirection,
    pub report: ExportReport,
    pub xml: String,
}

impl ExportEnvelope {
    pub fn build(report: ExportReport, alter_id_from: Option<u64>) -> Self {
        let (collection_name, entity_type, fetch_fields) = match report {
            ExportReport::CompanyInfo => ("Collection of Companies", "Company", "NAME, ALTERID"),
            ExportReport::Ledgers => ("Ledgers", "Ledger", "NAME, PARENT, ALTERID, OPENINGBALANCE"),
            ExportReport::Groups => ("Groups", "Group", "NAME, PARENT, ALTERID"),
            ExportReport::Vouchers => ("Vouchers", "Voucher", "VOUCHERNUMBER, VOUCHERTYPENAME, DATE, ALTERID, AMOUNT"),
            ExportReport::DeltaCollection => ("AlterIds", "Voucher", "VOUCHERNUMBER, VOUCHERTYPENAME, DATE, ALTERID, AMOUNT"),
        };

        let (filter_element, system_formula) = match alter_id_from {
            Some(id) => (
                "\n            <FILTERS>AlterIdFilter</FILTERS>".to_string(),
                format!("\n          <SYSTEM TYPE=\"Formulae\" NAME=\"AlterIdFilter\">$AlterID &gt; {id}</SYSTEM>"),
            ),
            None => (String::new(), String::new()),
        };

        let xml = format!(
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
        );

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
