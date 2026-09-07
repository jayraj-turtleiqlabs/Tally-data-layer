//! Log redaction — strips tokens, XML bodies, and financial field values.

use once_cell::sync::Lazy;
use regex::Regex;

static TOKEN_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(bearer\s+|token[=:\s]+|agent[_-]?token[=:\s]+)[A-Za-z0-9_\-\.]{20,}")
        .expect("token regex")
});

static XML_BODY_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?s)<ENVELOPE>.*?</ENVELOPE>").expect("xml regex")
});

static BALANCE_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)(balance|amount|openingbalance)[=:\s\"']*[0-9,\.]+"#)
        .expect("balance regex")
});

static CUSTOMER_NAME_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)(customer|vendor|ledger|party)[=:\s\"']*[A-Za-z][A-Za-z0-9\s\-\.&]{2,}"#)
        .expect("name regex")
});

static GSTIN_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b[0-9]{2}[A-Z]{5}[0-9]{4}[A-Z]{1}[1-9A-Z]{1}Z[0-9A-Z]{1}\b")
        .expect("gstin regex")
});

static PAN_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b[A-Z]{5}[0-9]{4}[A-Z]{1}\b").expect("pan regex")
});

/// Redact sensitive content from a log message before writing.
pub fn redact(message: &str) -> String {
    let mut out = message.to_string();
    out = TOKEN_PATTERN.replace_all(&out, "$1[REDACTED_TOKEN]").to_string();
    out = XML_BODY_PATTERN
        .replace_all(&out, "<ENVELOPE>[REDACTED_XML]</ENVELOPE>")
        .to_string();
    out = BALANCE_PATTERN
        .replace_all(&out, "$1[REDACTED_AMOUNT]")
        .to_string();
    out = CUSTOMER_NAME_PATTERN
        .replace_all(&out, "$1[REDACTED_NAME]")
        .to_string();
    out = GSTIN_PATTERN.replace_all(&out, "[REDACTED_GSTIN]").to_string();
    out = PAN_PATTERN.replace_all(&out, "[REDACTED_PAN]").to_string();
    out
}

/// Macro-friendly wrapper for logging redacted messages.
pub fn redact_fmt(args: std::fmt::Arguments<'_>) -> String {
    redact(&args.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_token_shaped_string() {
        let line = "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.abc123";
        let result = redact(line);
        assert!(!result.contains("eyJhbGci"));
        assert!(result.contains("[REDACTED_TOKEN]"));
    }

    #[test]
    fn redacts_xml_body() {
        let line = "Response: <ENVELOPE><BODY><LEDGER NAME=\"Cash\"/></BODY></ENVELOPE>";
        let result = redact(line);
        assert!(!result.contains("Cash"));
        assert!(result.contains("[REDACTED_XML]"));
    }

    #[test]
    fn redacts_balance_and_names() {
        let line = "ledger=Accounts Receivable balance=12345.67";
        let result = redact(line);
        assert!(!result.contains("Accounts Receivable"));
        assert!(!result.contains("12345.67"));
    }

    #[test]
    fn redacts_gstin_and_pan() {
        let line = "GSTIN: 27ABCDE1234F1Z5, PAN: ABCDE1234F";
        let result = redact(line);
        assert!(!result.contains("27ABCDE1234F1Z5"));
        assert!(!result.contains("ABCDE1234F"));
        assert!(result.contains("[REDACTED_GSTIN]"));
        assert!(result.contains("[REDACTED_PAN]"));
    }
}
