//! Hash-bound evidence references for transformed image and audio inputs.
//!
//! Extracted OCR, crops, captions, and transcript segments remain evidence
//! from an untrusted artifact. This module is deliberately independent of
//! model output parsing so callers must resolve every reference against the
//! current caller-owned artifact set before using it.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// Immutable identity and byte digest of the original uploaded artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceArtifact {
    pub artifact_id: String,
    pub sha256: String,
}

impl SourceArtifact {
    /// Bind a caller-owned artifact identity to the digest of its actual bytes.
    pub fn from_bytes(artifact_id: impl Into<String>, bytes: &[u8]) -> Self {
        Self {
            artifact_id: artifact_id.into(),
            sha256: sha256(bytes),
        }
    }
}

/// Selector into the original image or audio artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MediaSelector {
    /// Normalized image rectangle with coordinates in `[0, 1]`.
    ImageRegion {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
    /// Half-open audio interval in milliseconds.
    AudioInterval { start_ms: u64, end_ms: u64 },
    /// Whole-artifact reference, useful for generated captions.
    WholeArtifact,
}

/// Evidence produced by a transformation of an original media artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DerivedEvidence {
    pub evidence_id: String,
    pub source: SourceArtifact,
    pub derived_sha256: String,
    pub selector: MediaSelector,
    pub kind: EvidenceKind,
    /// Always untrusted: media text cannot become policy or authorization.
    pub trust: EvidenceTrust,
}

impl DerivedEvidence {
    /// Create transformation evidence only when the supplied original bytes
    /// still match the source artifact digest.
    pub fn derive(
        evidence_id: impl Into<String>,
        source: SourceArtifact,
        source_bytes: &[u8],
        derived_bytes: &[u8],
        selector: MediaSelector,
        kind: EvidenceKind,
    ) -> Result<Self, GroundingError> {
        if sha256(source_bytes) != source.sha256 {
            return Err(GroundingError::StaleSource(source.artifact_id));
        }
        let evidence = Self {
            evidence_id: evidence_id.into(),
            source,
            derived_sha256: sha256(derived_bytes),
            selector,
            kind,
            trust: EvidenceTrust::UntrustedMedia,
        };
        validate_evidence_shape(&evidence)?;
        Ok(evidence)
    }
}

/// Transformation type represented by an evidence reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Crop,
    OcrSpan,
    Caption,
    AudioSegment,
    Transcript,
}

/// Trust label for information extracted from user controlled media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceTrust {
    UntrustedMedia,
}

/// Evidence citation attached to an answer claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceCitation {
    pub claim_id: String,
    pub evidence_id: String,
}

/// A visual action tied to the exact image state from which coordinates came.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VisualActionReference {
    pub source: SourceArtifact,
    pub selector: MediaSelector,
}

/// Set of artifacts visible to one caller, keyed by artifact id and current digest.
#[derive(Debug, Clone, Default)]
pub struct CallerArtifactScope {
    current: BTreeMap<String, String>,
}

impl CallerArtifactScope {
    /// Build a scope from caller-authorized artifact identities and current digests.
    pub fn new(artifacts: impl IntoIterator<Item = SourceArtifact>) -> Self {
        Self {
            current: artifacts
                .into_iter()
                .map(|artifact| (artifact.artifact_id, artifact.sha256))
                .collect(),
        }
    }

    /// Resolve derived evidence only if its source id and hash remain in scope.
    pub fn resolve(&self, evidence: &DerivedEvidence) -> Result<(), GroundingError> {
        validate_evidence_shape(evidence)?;
        match self.current.get(&evidence.source.artifact_id) {
            Some(hash) if hash == &evidence.source.sha256 => Ok(()),
            Some(_) => Err(GroundingError::StaleSource(
                evidence.source.artifact_id.clone(),
            )),
            None => Err(GroundingError::OutOfScope(
                evidence.source.artifact_id.clone(),
            )),
        }
    }

    /// Permit coordinate-based actions only against a source still current in this scope.
    pub fn validate_visual_action(
        &self,
        action: &VisualActionReference,
    ) -> Result<(), GroundingError> {
        if action.source.artifact_id.trim().is_empty() || !valid_sha256(&action.source.sha256) {
            return Err(GroundingError::Invalid(
                "visual action requires an artifact id and SHA-256 digest".into(),
            ));
        }
        match self.current.get(&action.source.artifact_id) {
            Some(hash) if hash == &action.source.sha256 => {}
            Some(_) => {
                return Err(GroundingError::StaleSource(
                    action.source.artifact_id.clone(),
                ));
            }
            None => {
                return Err(GroundingError::OutOfScope(
                    action.source.artifact_id.clone(),
                ));
            }
        }
        if !matches!(action.selector, MediaSelector::ImageRegion { .. }) {
            return Err(GroundingError::Invalid(
                "coordinate-based actions require an image region selector".into(),
            ));
        }
        validate_selector(&action.selector)
    }

    /// Resolve citations against current evidence and this caller's artifact scope.
    pub fn resolve_citations(
        &self,
        evidence: &[DerivedEvidence],
        citations: &[EvidenceCitation],
    ) -> Result<BTreeSet<String>, GroundingError> {
        let mut by_id = BTreeMap::new();
        for item in evidence {
            self.resolve(item)?;
            if by_id.insert(item.evidence_id.as_str(), item).is_some() {
                return Err(GroundingError::Invalid(
                    "duplicate evidence id in the supplied set".into(),
                ));
            }
        }
        let mut claims = BTreeSet::new();
        for citation in citations {
            let Some(item) = by_id.get(citation.evidence_id.as_str()) else {
                return Err(GroundingError::Invalid(format!(
                    "citation references unknown evidence '{}'",
                    citation.evidence_id
                )));
            };
            self.resolve(item)?;
            claims.insert(citation.claim_id.clone());
        }
        Ok(claims)
    }
}

/// Noisy/occluded fixture outcomes keep grounding quality distinct from prose quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroundingFixtureScore {
    pub text_quality_pass: bool,
    pub supported_claims: usize,
    pub unsupported_claims: usize,
}

impl GroundingFixtureScore {
    /// Build a score from cited and unsupported claim ids without conflating text quality.
    pub fn measure(
        text_quality_pass: bool,
        citations: &[EvidenceCitation],
        valid_evidence_ids: &BTreeSet<String>,
        answer_claim_ids: &BTreeSet<String>,
    ) -> Self {
        let supported = citations
            .iter()
            .filter(|citation| {
                answer_claim_ids.contains(&citation.claim_id)
                    && valid_evidence_ids.contains(&citation.evidence_id)
            })
            .map(|citation| citation.claim_id.as_str())
            .collect::<BTreeSet<_>>();
        let supported_claims = supported.len();
        Self {
            text_quality_pass,
            supported_claims,
            unsupported_claims: answer_claim_ids.len().saturating_sub(supported_claims),
        }
    }
}

/// Invalid, stale, malformed, or untrusted multimodal evidence.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GroundingError {
    #[error("artifact '{0}' is outside the caller's artifact scope")]
    OutOfScope(String),
    #[error("artifact '{0}' changed after evidence was derived")]
    StaleSource(String),
    #[error("invalid evidence metadata: {0}")]
    Invalid(String),
    #[error("media-derived text cannot be used as trusted policy")]
    UntrustedPolicy,
}

/// Reject any attempt to promote OCR/transcript/caption content into policy.
pub fn require_trusted_policy_source(evidence: &DerivedEvidence) -> Result<(), GroundingError> {
    let _ = evidence;
    Err(GroundingError::UntrustedPolicy)
}

fn validate_evidence_shape(evidence: &DerivedEvidence) -> Result<(), GroundingError> {
    if evidence.evidence_id.trim().is_empty()
        || evidence.source.artifact_id.trim().is_empty()
        || !valid_sha256(&evidence.source.sha256)
        || !valid_sha256(&evidence.derived_sha256)
    {
        return Err(GroundingError::Invalid(
            "evidence id and source/derived SHA-256 digests are required".into(),
        ));
    }
    if evidence.trust != EvidenceTrust::UntrustedMedia {
        return Err(GroundingError::UntrustedPolicy);
    }
    validate_selector(&evidence.selector)?;
    let matches_kind = matches!(
        (evidence.kind, &evidence.selector),
        (
            EvidenceKind::Crop | EvidenceKind::OcrSpan,
            MediaSelector::ImageRegion { .. }
        ) | (
            EvidenceKind::AudioSegment | EvidenceKind::Transcript,
            MediaSelector::AudioInterval { .. }
        ) | (EvidenceKind::Caption, MediaSelector::WholeArtifact)
    );
    if !matches_kind {
        return Err(GroundingError::Invalid(
            "evidence kind does not match its media selector".into(),
        ));
    }
    Ok(())
}

fn validate_selector(selector: &MediaSelector) -> Result<(), GroundingError> {
    match selector {
        MediaSelector::ImageRegion {
            x,
            y,
            width,
            height,
        } if [*x, *y, *width, *height]
            .iter()
            .all(|value| value.is_finite())
            && *x >= 0.0
            && *y >= 0.0
            && *width > 0.0
            && *height > 0.0
            && *x + *width <= 1.0
            && *y + *height <= 1.0 =>
        {
            Ok(())
        }
        MediaSelector::AudioInterval { start_ms, end_ms } if start_ms < end_ms => Ok(()),
        MediaSelector::WholeArtifact => Ok(()),
        _ => Err(GroundingError::Invalid(
            "media selector is out of bounds".into(),
        )),
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(source_hash: &str) -> DerivedEvidence {
        DerivedEvidence {
            evidence_id: "ocr-1".into(),
            source: SourceArtifact {
                artifact_id: "image-1".into(),
                sha256: source_hash.into(),
            },
            derived_sha256: "b".repeat(64),
            selector: MediaSelector::ImageRegion {
                x: 0.1,
                y: 0.2,
                width: 0.4,
                height: 0.3,
            },
            kind: EvidenceKind::OcrSpan,
            trust: EvidenceTrust::UntrustedMedia,
        }
    }

    #[test]
    fn edited_screenshot_invalidates_old_coordinate_action() {
        let action = VisualActionReference {
            source: evidence(&"a".repeat(64)).source,
            selector: MediaSelector::ImageRegion {
                x: 0.1,
                y: 0.2,
                width: 0.2,
                height: 0.2,
            },
        };
        let fresh = CallerArtifactScope::new([SourceArtifact {
            artifact_id: "image-1".into(),
            sha256: "c".repeat(64),
        }]);
        assert!(matches!(
            fresh.validate_visual_action(&action),
            Err(GroundingError::StaleSource(id)) if id == "image-1"
        ));
    }

    #[test]
    fn transformed_evidence_is_hash_bound_and_caller_scoped() {
        let item = evidence(&"a".repeat(64));
        let matching = CallerArtifactScope::new([item.source.clone()]);
        assert_eq!(matching.resolve(&item), Ok(()));
        let other = CallerArtifactScope::new([SourceArtifact {
            artifact_id: "other-callers-image".into(),
            sha256: "a".repeat(64),
        }]);
        assert!(matches!(
            other.resolve(&item),
            Err(GroundingError::OutOfScope(_))
        ));
    }

    #[test]
    fn answer_citations_must_resolve_to_current_in_scope_evidence() {
        let item = evidence(&"a".repeat(64));
        let cite = EvidenceCitation {
            claim_id: "claim-1".into(),
            evidence_id: item.evidence_id.clone(),
        };
        let scope = CallerArtifactScope::new([item.source.clone()]);
        assert_eq!(
            scope.resolve_citations(std::slice::from_ref(&item), std::slice::from_ref(&cite)),
            Ok(BTreeSet::from(["claim-1".to_owned()]))
        );
        let stale = CallerArtifactScope::new([SourceArtifact {
            artifact_id: item.source.artifact_id.clone(),
            sha256: "c".repeat(64),
        }]);
        assert!(matches!(
            stale.resolve_citations(std::slice::from_ref(&item), std::slice::from_ref(&cite)),
            Err(GroundingError::StaleSource(_))
        ));
    }

    #[test]
    fn image_text_cannot_be_promoted_to_policy_and_bad_selectors_fail_closed() {
        let mut item = evidence(&"a".repeat(64));
        assert_eq!(
            require_trusted_policy_source(&item),
            Err(GroundingError::UntrustedPolicy)
        );
        item.selector = MediaSelector::ImageRegion {
            x: 0.9,
            y: 0.2,
            width: 0.2,
            height: 0.3,
        };
        let scope = CallerArtifactScope::new([item.source.clone()]);
        assert!(matches!(
            scope.resolve(&item),
            Err(GroundingError::Invalid(_))
        ));
    }

    #[test]
    fn noisy_fixture_reports_unsupported_claims_separately_from_text_quality() {
        let claims = BTreeSet::from(["supported".to_owned(), "hallucinated".to_owned()]);
        let citations = [EvidenceCitation {
            claim_id: "supported".into(),
            evidence_id: "ocr-1".into(),
        }];
        let evidence = BTreeSet::from(["ocr-1".to_owned()]);
        let score = GroundingFixtureScore::measure(false, &citations, &evidence, &claims);
        assert_eq!(
            score,
            GroundingFixtureScore {
                text_quality_pass: false,
                supported_claims: 1,
                unsupported_claims: 1,
            }
        );
    }

    #[test]
    fn audio_segment_preserves_original_digest_and_time_range() {
        let item = DerivedEvidence {
            evidence_id: "transcript-segment-4".into(),
            source: SourceArtifact {
                artifact_id: "audio-1".into(),
                sha256: "d".repeat(64),
            },
            derived_sha256: "e".repeat(64),
            selector: MediaSelector::AudioInterval {
                start_ms: 1200,
                end_ms: 2750,
            },
            kind: EvidenceKind::AudioSegment,
            trust: EvidenceTrust::UntrustedMedia,
        };
        let scope = CallerArtifactScope::new([item.source.clone()]);
        assert_eq!(scope.resolve(&item), Ok(()));
        assert!(matches!(
            scope.validate_visual_action(&VisualActionReference {
                source: item.source,
                selector: item.selector,
            }),
            Err(GroundingError::Invalid(_))
        ));
    }

    #[test]
    fn transformation_constructor_hashes_outputs_and_rejects_changed_original_bytes() {
        let original = b"original screenshot bytes";
        let source = SourceArtifact::from_bytes("image-2", original);
        let derived = DerivedEvidence::derive(
            "crop-1",
            source.clone(),
            original,
            b"cropped image bytes",
            MediaSelector::ImageRegion {
                x: 0.0,
                y: 0.0,
                width: 0.5,
                height: 0.5,
            },
            EvidenceKind::Crop,
        )
        .unwrap();
        assert_eq!(derived.source, source);
        assert_eq!(derived.derived_sha256, sha256(b"cropped image bytes"));
        assert!(matches!(
            DerivedEvidence::derive(
                "crop-2",
                source,
                b"edited screenshot bytes",
                b"cropped image bytes",
                derived.selector,
                EvidenceKind::Crop,
            ),
            Err(GroundingError::StaleSource(_))
        ));
    }
}
