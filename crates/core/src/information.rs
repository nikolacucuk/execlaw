//! Typed, non-model-editable information labels for provenance and egress policy.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Confidentiality carried with data as it moves through a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    Public,
    Internal,
    Sensitive,
    Secret,
}

/// One immutable source or transformation in a data item's history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceEntry {
    pub source_kind: String,
    pub source_id: String,
    pub owner_id: Option<String>,
    pub trust_class: String,
    pub transformation: Option<String>,
}

/// Audited exception allowing one otherwise restricted export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclassificationRecord {
    pub actor_id: String,
    pub destination: String,
    pub scope: String,
    pub authorized_at: i64,
}

/// Information-flow metadata kept beside data, never embedded in model text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InformationLabel {
    pub sensitivity: Sensitivity,
    pub owner_id: Option<String>,
    pub source_trust_class: String,
    pub allowed_destinations: BTreeSet<String>,
    pub provenance: Vec<ProvenanceEntry>,
    pub declassification: Option<DeclassificationRecord>,
    /// Dispatch-time grant identity for sinks that can revalidate authority.
    #[serde(default)]
    pub authority_principal_id: Option<String>,
    #[serde(default)]
    pub authority_fingerprint: Option<String>,
    /// Append-only operator policy revision captured when the effect was
    /// authorized. Sinks reject an effect if policy changed while queued.
    #[serde(default)]
    pub authority_policy_revision: Option<i64>,
}

impl InformationLabel {
    /// Label an observed value with its original owner, trust, and source.
    pub fn observed(
        sensitivity: Sensitivity,
        owner_id: Option<String>,
        trust_class: impl Into<String>,
        source_kind: impl Into<String>,
        source_id: impl Into<String>,
        allowed_destinations: impl IntoIterator<Item = String>,
    ) -> Self {
        let trust_class = trust_class.into();
        Self {
            sensitivity,
            owner_id: owner_id.clone(),
            source_trust_class: trust_class.clone(),
            allowed_destinations: allowed_destinations.into_iter().collect(),
            provenance: vec![ProvenanceEntry {
                source_kind: source_kind.into(),
                source_id: source_id.into(),
                owner_id,
                trust_class,
                transformation: None,
            }],
            declassification: None,
            authority_principal_id: None,
            authority_fingerprint: None,
            authority_policy_revision: None,
        }
    }

    /// Bind this label to the immutable authority snapshot that allowed the
    /// originating tool call. A delivery worker can reject it after revocation.
    pub fn with_authority(
        mut self,
        principal_id: impl Into<String>,
        fingerprint: impl Into<String>,
    ) -> Self {
        self.authority_principal_id = Some(principal_id.into());
        self.authority_fingerprint = Some(fingerprint.into());
        self
    }

    /// Bind this label to the current append-only tool-policy revision.
    pub fn with_policy_revision(mut self, revision: i64) -> Self {
        self.authority_policy_revision = Some(revision);
        self
    }

    /// Propagate the original restrictions and append a transformation receipt.
    pub fn transformed(&self, operation: impl Into<String>) -> Self {
        let mut next = self.clone();
        let operation = operation.into();
        for entry in &mut next.provenance {
            entry.transformation = Some(operation.clone());
        }
        next.declassification = None;
        next
    }

    /// Join multiple inputs conservatively: highest sensitivity, lowest trust,
    /// intersection of permitted destinations, and union of source lineage.
    pub fn combine(labels: &[Self], operation: impl Into<String>) -> Option<Self> {
        let mut iter = labels.iter();
        let mut combined = iter.next()?.clone();
        let operation = operation.into();
        for label in iter {
            combined.sensitivity = combined.sensitivity.max(label.sensitivity);
            if combined.owner_id != label.owner_id {
                combined.owner_id = None;
            }
            if trust_rank(&label.source_trust_class) < trust_rank(&combined.source_trust_class) {
                combined.source_trust_class = label.source_trust_class.clone();
            }
            combined.allowed_destinations = destination_intersection(
                &combined.allowed_destinations,
                &label.allowed_destinations,
            );
            for entry in &label.provenance {
                if !combined.provenance.contains(entry) {
                    combined.provenance.push(entry.clone());
                }
            }
        }
        for entry in &mut combined.provenance {
            entry.transformation = Some(operation.clone());
        }
        combined.declassification = None;
        combined.authority_policy_revision = labels
            .iter()
            .map(|label| label.authority_policy_revision)
            .reduce(|left, right| (left == right).then_some(left).flatten())
            .flatten();
        Some(combined)
    }

    /// Test whether a destination is permitted by the label or an audited
    /// declassification for this exact destination.
    pub fn permits_destination(&self, destination: &str) -> bool {
        self.allowed_destinations.iter().any(|allowed| {
            allowed == destination
                || allowed
                    .strip_suffix('*')
                    .is_some_and(|prefix| destination.starts_with(prefix))
        }) || self
            .declassification
            .as_ref()
            .is_some_and(|record| record.destination == destination)
    }

    /// Record an explicit, scoped operator export decision.
    pub fn declassify(
        &mut self,
        actor_id: &str,
        destination: &str,
        scope: &str,
        authorized_at: i64,
    ) -> Result<(), &'static str> {
        if actor_id.is_empty() || destination.is_empty() || scope.is_empty() {
            return Err("declassification requires actor, destination, and scope");
        }
        self.declassification = Some(DeclassificationRecord {
            actor_id: actor_id.to_owned(),
            destination: destination.to_owned(),
            scope: scope.to_owned(),
            authorized_at,
        });
        Ok(())
    }
}

fn destination_intersection(left: &BTreeSet<String>, right: &BTreeSet<String>) -> BTreeSet<String> {
    let mut intersection = BTreeSet::new();
    for first in left {
        for second in right {
            if first == second {
                intersection.insert(first.clone());
            } else if let Some(prefix) = first.strip_suffix('*')
                && second.starts_with(prefix)
            {
                intersection.insert(second.clone());
            } else if let Some(prefix) = second.strip_suffix('*')
                && first.starts_with(prefix)
            {
                intersection.insert(first.clone());
            }
        }
    }
    intersection
}

fn trust_rank(class: &str) -> u8 {
    match class {
        "Controller" => 5,
        "Delegated" => 4,
        "KnownTrusted" => 3,
        "KnownLimited" => 2,
        "UnknownPending" => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(
        sensitivity: Sensitivity,
        trust: &str,
        destinations: &[&str],
        source: &str,
    ) -> InformationLabel {
        InformationLabel::observed(
            sensitivity,
            Some("owner-1".into()),
            trust,
            "message",
            source,
            destinations.iter().map(|value| (*value).to_owned()),
        )
    }

    #[test]
    fn transformation_preserves_sensitivity_and_destination_limits() {
        let original = label(
            Sensitivity::Sensitive,
            "KnownLimited",
            &["conversation:a"],
            "e1",
        );
        let summary = original.transformed("summarize");
        assert_eq!(summary.sensitivity, Sensitivity::Sensitive);
        assert!(!summary.permits_destination("transport:recipient-b"));
        assert_eq!(
            summary.provenance[0].transformation.as_deref(),
            Some("summarize")
        );
    }

    #[test]
    fn combined_data_uses_highest_sensitivity_lowest_trust_and_destination_intersection() {
        let first = label(Sensitivity::Internal, "Controller", &["a", "b"], "e1");
        let second = label(Sensitivity::Secret, "KnownLimited", &["b", "c"], "e2");
        let merged = InformationLabel::combine(&[first, second], "merge").unwrap();
        assert_eq!(merged.sensitivity, Sensitivity::Secret);
        assert_eq!(merged.source_trust_class, "KnownLimited");
        assert_eq!(merged.allowed_destinations, BTreeSet::from(["b".into()]));
        assert_eq!(merged.provenance.len(), 2);
    }

    #[test]
    fn only_explicit_scoped_declassification_adds_an_export_exception() {
        let mut value = label(Sensitivity::Secret, "KnownLimited", &[], "e1");
        assert!(!value.permits_destination("mail:alice"));
        assert!(value.declassify("", "mail:alice", "report-1", 10).is_err());
        value
            .declassify("controller-1", "mail:alice", "report-1", 10)
            .unwrap();
        assert!(value.permits_destination("mail:alice"));
        assert!(!value.permits_destination("mail:bob"));
    }

    #[test]
    fn destination_join_narrows_wildcard_grants_and_denies_other_scopes() {
        let broad = label(
            Sensitivity::Sensitive,
            "Controller",
            &["transport:*"],
            "run-1",
        );
        let effect = label(
            Sensitivity::Sensitive,
            "Controller",
            &["transport:signal:alice"],
            "effect-1",
        );
        let joined = InformationLabel::combine(&[broad, effect], "send").unwrap();
        assert!(joined.permits_destination("transport:signal:alice"));
        assert!(!joined.permits_destination("transport:signal:bob"));
    }

    #[test]
    fn information_label_round_trips_through_the_outbox_messagepack_codec() {
        let value = label(
            Sensitivity::Sensitive,
            "KnownTrusted",
            &["transport:signal:recipient"],
            "event-10",
        )
        .with_authority("principal-1", "fingerprint-1");
        let encoded = rmp_serde::to_vec(&value).unwrap();
        let decoded: InformationLabel = rmp_serde::from_slice(&encoded).unwrap();
        assert_eq!(decoded, value);
    }
}
