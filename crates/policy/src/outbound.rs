//! Deterministic defense-in-depth checks for data crossing a sink boundary.

use execlaw_core::information::{InformationLabel, Sensitivity};

/// Safe-to-log decision details. Findings name only the detector, never a
/// matched credential or source fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundDecision {
    pub allowed: bool,
    pub findings: Vec<&'static str>,
}

/// Inspect outbound content and any host-carried information label.
pub fn inspect_outbound(
    content: &[u8],
    label: Option<&InformationLabel>,
    destination: &str,
) -> OutboundDecision {
    let text = String::from_utf8_lossy(content);
    let operator_authorized_export = label
        .and_then(|label| label.declassification.as_ref())
        .is_some_and(|record| record.destination == destination);
    let mut findings = if operator_authorized_export {
        Vec::new()
    } else {
        secret_indicators(&text)
    };
    if let Some(label) = label {
        if !label.permits_destination(destination) {
            findings.push("destination_not_authorized_by_label");
        }
        if label.sensitivity == Sensitivity::Secret
            && !label
                .declassification
                .as_ref()
                .is_some_and(|record| record.destination == destination)
        {
            findings.push("secret_data_requires_explicit_export");
        }
    }
    findings.sort_unstable();
    findings.dedup();
    OutboundDecision {
        allowed: findings.is_empty(),
        findings,
    }
}

/// Return stable detector IDs for high-confidence credential markers.
pub fn secret_indicators(text: &str) -> Vec<&'static str> {
    let lower = text.to_ascii_lowercase();
    let mut findings = Vec::new();
    if lower.contains("-----begin private key-----")
        || lower.contains("-----begin rsa private key-----")
        || lower.contains("-----begin openssh private key-----")
    {
        findings.push("private_key_block");
    }
    if contains_assignment_secret(&lower) {
        findings.push("credential_assignment");
    }
    if contains_bearer_token(text) {
        findings.push("bearer_token");
    }
    if contains_prefixed_key(text, "sk-", 20) {
        findings.push("sk_key");
    }
    if contains_prefixed_key(text, "akia", 16) {
        findings.push("aws_access_key");
    }
    findings
}

fn contains_assignment_secret(lower: &str) -> bool {
    [
        "api_key",
        "api-key",
        "secret",
        "password",
        "credential",
        "access_token",
    ]
    .iter()
    .any(|key| {
        let mut rest = lower;
        while let Some(index) = rest.find(key) {
            rest = &rest[index + key.len()..];
            let value = rest.trim_start_matches(|ch: char| {
                ch.is_ascii_whitespace() || matches!(ch, '=' | ':' | '\'' | '"')
            });
            let length = value
                .chars()
                .take_while(|ch| {
                    !ch.is_ascii_whitespace() && !matches!(ch, ',' | ';' | '}' | '"' | '\'')
                })
                .count();
            if length >= 8 {
                return true;
            }
        }
        false
    })
}

fn contains_bearer_token(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let mut rest = lower.as_str();
    while let Some(index) = rest.find("bearer ") {
        rest = &rest[index + 7..];
        let length = rest
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '~'))
            .count();
        if length >= 16 {
            return true;
        }
    }
    false
}

fn contains_prefixed_key(text: &str, prefix: &str, suffix_len: usize) -> bool {
    let lower = text.to_ascii_lowercase();
    let mut rest = lower.as_str();
    while let Some(index) = rest.find(prefix) {
        let start = index + prefix.len();
        let suffix = &rest[start..];
        let matched = suffix
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
            .count();
        if matched >= suffix_len {
            return true;
        }
        let skip = rest[index..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(rest.len());
        rest = &rest[index + skip..];
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use execlaw_core::information::{InformationLabel, Sensitivity};

    fn label(destinations: &[&str], sensitivity: Sensitivity) -> InformationLabel {
        InformationLabel::observed(
            sensitivity,
            Some("owner-1".into()),
            "Controller",
            "run_output",
            "run-1",
            destinations.iter().map(|item| (*item).to_owned()),
        )
    }

    #[test]
    fn catches_synthetic_credentials_without_returning_the_value() {
        let text = "key=api_key=sk-test_012345678901234567890123";
        let decision = inspect_outbound(text.as_bytes(), None, "webhook:one");
        assert!(!decision.allowed);
        assert!(decision.findings.contains(&"credential_assignment"));
        assert!(decision.findings.contains(&"sk_key"));
        assert!(!format!("{decision:?}").contains("sk-test"));
    }

    #[test]
    fn rejects_unlisted_destination_and_secret_without_export_record() {
        let secret = label(&["transport:alice"], Sensitivity::Secret);
        let decision = inspect_outbound(b"ordinary text", Some(&secret), "transport:bob");
        assert!(!decision.allowed);
        assert!(
            decision
                .findings
                .contains(&"destination_not_authorized_by_label")
        );
        assert!(
            decision
                .findings
                .contains(&"secret_data_requires_explicit_export")
        );
    }

    #[test]
    fn allows_labeled_destination_when_no_secret_indicator_is_present() {
        let value = label(&["transport:alice"], Sensitivity::Sensitive);
        assert!(inspect_outbound(b"hello", Some(&value), "transport:alice").allowed);
    }

    #[test]
    fn explicit_operator_export_allows_a_secret_only_for_its_approved_destination() {
        let mut value = label(&[], Sensitivity::Secret);
        value
            .declassify("controller-1", "transport:alice", "case-17", 1)
            .unwrap();
        let bytes = b"api_key=synthetic-secret-value";
        assert!(inspect_outbound(bytes, Some(&value), "transport:alice").allowed);
        assert!(!inspect_outbound(bytes, Some(&value), "transport:bob").allowed);
    }
}
