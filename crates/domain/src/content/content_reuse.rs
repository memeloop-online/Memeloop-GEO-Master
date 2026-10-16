//! Frozen semantic inputs for cross-cycle reuse. No cycle, execution, mutable
//! account settings, measurement answers, or operational timestamps belong here.
use crate::{AppError, ContentEvidence, ContentItem, StepLease, TenantScope};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const CONTENT_SEMANTIC_DESCRIPTOR_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentSemanticDescriptor {
    pub version: u32,
    pub scope: TenantScope,
    pub document_key: String,
    pub content_type: String,
    pub product_id: Option<Uuid>,
    pub market: String,
    pub language: String,
    pub planner_version: String,
    pub source_version_ids: Vec<Uuid>,
    /// Evidence selection order may affect generation and is deliberately kept.
    pub evidence: Vec<ContentEvidence>,
    pub brand_name: String,
    pub product_name: Option<String>,
    pub target_audience: Option<String>,
    pub objective: Option<String>,
    pub question_clusters: Vec<String>,
    pub brief_title: String,
    pub brief_objective: String,
    pub generation_policy_version: String,
    pub evidence_policy_version: String,
    pub check_policy_version: String,
    pub repair_policy_version: String,
    pub output_schema_version: String,
    pub generation_policy_revision: String,
}

impl ContentSemanticDescriptor {
    pub fn canonical(&self) -> Result<Self, AppError> {
        if self.version != CONTENT_SEMANTIC_DESCRIPTOR_VERSION
            || self.scope.project_id.is_none()
            || [
                &self.document_key,
                &self.content_type,
                &self.market,
                &self.language,
                &self.planner_version,
                &self.brief_title,
                &self.brief_objective,
                &self.generation_policy_version,
                &self.evidence_policy_version,
                &self.check_policy_version,
                &self.repair_policy_version,
                &self.output_schema_version,
                &self.generation_policy_revision,
            ]
            .iter()
            .any(|part| part.trim().is_empty())
            || self.source_version_ids.is_empty()
            || self.evidence.is_empty()
            || self.evidence.iter().any(|e| {
                e.exact_quote.trim().is_empty()
                    || e.reference.chunk_id.is_none()
                    || !self
                        .source_version_ids
                        .contains(&e.reference.source_version_id)
            })
        {
            return Err(AppError::invalid_request(
                "complete frozen semantic input and located public evidence required",
            ));
        }
        let mut canonical = self.clone();
        canonical.source_version_ids.sort_unstable();
        canonical.source_version_ids.dedup();
        canonical.question_clusters.sort();
        canonical.question_clusters.dedup();
        Ok(canonical)
    }

    pub fn fingerprint(&self) -> Result<String, AppError> {
        let canonical = self.canonical()?;
        let bytes = serde_json::to_vec(&canonical)
            .map_err(|_| AppError::invalid_request("semantic input cannot be serialized"))?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentReuseBinding {
    pub origin_execution_id: Uuid,
    pub origin_item_id: Uuid,
    pub asset_id: Uuid,
    pub revision_id: Uuid,
    pub check_id: Uuid,
    pub fingerprint: String,
    pub reused_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentReuseCandidate {
    pub descriptor: ContentSemanticDescriptor,
    pub fingerprint: String,
    pub origin_execution_id: Uuid,
    pub origin_item_id: Uuid,
    pub asset_id: Uuid,
    pub revision_id: Uuid,
    pub check_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentReuseRequest {
    pub execution_id: Uuid,
    pub item_id: Uuid,
    pub descriptor: ContentSemanticDescriptor,
    pub owner: String,
    pub now: DateTime<Utc>,
    pub ttl_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentReuseDecision {
    Ready(ContentItem),
    Reserved { item: ContentItem, lease: StepLease },
    Busy(ContentItem),
    InsufficientEvidence(ContentItem),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChunkLocator, ContentEvidence, EvidenceRef, OperatorId, ProjectId, TenantId};

    fn descriptor() -> ContentSemanticDescriptor {
        let source = Uuid::new_v4();
        ContentSemanticDescriptor {
            version: CONTENT_SEMANTIC_DESCRIPTOR_VERSION,
            scope: TenantScope::new(
                OperatorId(Uuid::new_v4()),
                TenantId(Uuid::new_v4()),
                Some(ProjectId(Uuid::new_v4())),
            ),
            document_key: "faq".into(),
            content_type: "article".into(),
            product_id: None,
            market: "global".into(),
            language: "en".into(),
            planner_version: "planner-v1".into(),
            source_version_ids: vec![source],
            evidence: vec![ContentEvidence {
                reference: EvidenceRef {
                    source_version_id: source,
                    chunk_id: Some(Uuid::new_v4()),
                    locator: ChunkLocator::Text {
                        start_line: 1,
                        end_line: 1,
                        start_char: 0,
                        end_char: 6,
                    },
                },
                exact_quote: "answer".into(),
            }],
            brand_name: "Example".into(),
            product_name: None,
            target_audience: None,
            objective: None,
            question_clusters: vec!["questions".into()],
            brief_title: "FAQ".into(),
            brief_objective: "Answers".into(),
            generation_policy_version: "generate-v1".into(),
            evidence_policy_version: "evidence-v1".into(),
            check_policy_version: "check-v1".into(),
            repair_policy_version: "repair-v1".into(),
            output_schema_version: "output-v1".into(),
            generation_policy_revision: "revision-v1".into(),
        }
    }

    #[test]
    fn fingerprint_excludes_execution_and_normalizes_unordered_dependencies() {
        let d = descriptor();
        let mut other = d.clone();
        other.source_version_ids.push(d.source_version_ids[0]);
        other.question_clusters.push("questions".into());
        assert_eq!(d.fingerprint().unwrap(), other.fingerprint().unwrap());
        other.evidence[0].exact_quote = "changed".into();
        assert_ne!(d.fingerprint().unwrap(), other.fingerprint().unwrap());
    }
}
