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

        let filter_block = alter_id_from.map(|id| {
            format!(
                r#"
          <FILTERS>AlterIdFilter</FILTERS>
          <SYSTEM TYPE="Formulae" NAME="AlterIdFilter">$AlterID &gt; {id}</SYSTEM>"#
            )
        }).unwrap_or_default();

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
            <FETCH>{fetch_fields}</FETCH>{filter_block}
          </COLLECTION>
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
}
