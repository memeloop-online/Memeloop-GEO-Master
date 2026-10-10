//! Versioned project question sets and irreversible project-wide evaluation assignment.
//!
//! `QuestionProjectState` is a compact transition input. A database implementation
//! locks the project, loads its enrollment count and seed, the current touched set,
//! and identities found through indexed aliases/explicit IDs. It then applies the
//! same transition used by memory mode, and persists the resulting delta atomically.
//! It must not interpret an identity absent from this *partial* input as proof that
//! an alias does not exist: the indexed lookup is part of the transaction boundary.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use unicode_normalization::UnicodeNormalization;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{AppError, TenantScope};

pub const QUESTION_SPLIT_POLICY_VERSION: &str = "project_registry_nfkc_v1";
pub const MAX_QUESTION_PAGE_SIZE: u32 = 100;
pub const MAX_SET_QUESTIONS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuestionPurpose {
    Optimization,
    FrozenEvaluation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuestionSourceKind {
    UserProvided,
    SalesConsultation,
    Product,
    Faq,
    Generated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionSource {
    pub kind: QuestionSourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionDraft {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_id: Option<Uuid>,
    pub text: String,
    pub intent: String,
    #[serde(default)]
    pub product_refs: Vec<Uuid>,
    pub market: String,
    pub language: String,
    pub source: QuestionSource,
    pub weight: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateQuestionSet {
    pub idempotency_key: String,
    pub name: String,
    pub questions: Vec<QuestionDraft>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviseQuestionSet {
    pub idempotency_key: String,
    pub base_version_id: Uuid,
    pub name: String,
    pub questions: Vec<QuestionDraft>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionRevision {
    pub id: Uuid,
    pub question_id: Uuid,
    pub text: String,
    pub intent: String,
    pub product_refs: Vec<Uuid>,
    pub market: String,
    pub language: String,
    pub source: QuestionSource,
    pub weight: u32,
    pub purpose: QuestionPurpose,
    pub split_policy_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionSetVersion {
    pub id: Uuid,
    pub question_set_id: Uuid,
    pub revision: u32,
    pub parent_version_id: Option<Uuid>,
    pub name: String,
    pub questions: Vec<QuestionRevision>,
    pub optimization_count: u32,
    pub evaluation_count: u32,
    pub split_policy_version: String,
    pub content_hash: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionSetSummary {
    pub id: Uuid,
    pub name: String,
    pub current_version_id: Uuid,
    pub current_revision: u32,
    pub question_count: u32,
    pub optimization_count: u32,
    pub evaluation_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionSetVersionSummary {
    pub id: Uuid,
    pub question_set_id: Uuid,
    pub revision: u32,
    pub parent_version_id: Option<Uuid>,
    pub name: String,
    pub question_count: u32,
    pub optimization_count: u32,
    pub evaluation_count: u32,
    pub split_policy_version: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionSetPage {
    pub items: Vec<QuestionSetSummary>,
    pub next_cursor: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionSetVersionPage {
    pub items: Vec<QuestionSetVersionSummary>,
    pub next_cursor: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionReference {
    pub question_set_id: Uuid,
    pub question_set_version_id: Uuid,
    pub question_id: Uuid,
    pub question_revision_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FrozenQuestionBinding {
    pub reference: QuestionReference,
    pub purpose: QuestionPurpose,
    pub split_policy_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolvedQuestion {
    pub binding: FrozenQuestionBinding,
    pub revision: QuestionRevision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionIdentity {
    pub id: Uuid,
    pub purpose: QuestionPurpose,
    pub split_policy_version: String,
    /// Every historical spelling is permanently reserved for this identity.
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionSetRecord {
    pub id: Uuid,
    /// Memory mode holds all versions; PostgreSQL only needs the touched current
    /// version and, for an idempotent replay, its indexed response version.
    pub versions: Vec<QuestionSetVersion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionIdempotencyRecord {
    pub key: String,
    pub request_hash: String,
    pub question_set_id: Uuid,
    pub version_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionProjectState {
    pub seed: Uuid,
    /// Authoritative total of all distinct enrolled identities, including ones
    /// not loaded into `identities` by an indexed persistence lookup.
    pub enrolled_count: u64,
    pub identities: Vec<QuestionIdentity>,
    pub sets: Vec<QuestionSetRecord>,
    pub idempotency: Vec<QuestionIdempotencyRecord>,
}

/// NFKC, Unicode lowercase, and collapse/trim Unicode whitespace. Punctuation
/// remains significant. This is exact-text identity, not semantic deduplication.
pub fn normalize_question_text(text: &str) -> String {
    let lower: String = text.nfkc().flat_map(char::to_lowercase).collect();
    lower.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl QuestionProjectState {
    pub fn new(seed: Uuid) -> Self {
        Self::from_parts(seed, 0, Vec::new(), Vec::new(), Vec::new())
    }

    pub fn from_parts(
        seed: Uuid,
        enrolled_count: u64,
        identities: Vec<QuestionIdentity>,
        sets: Vec<QuestionSetRecord>,
        idempotency: Vec<QuestionIdempotencyRecord>,
    ) -> Self {
        Self {
            seed,
            enrolled_count,
            identities,
            sets,
            idempotency,
        }
    }

    pub fn create_set(
        &mut self,
        command: CreateQuestionSet,
        now: DateTime<Utc>,
    ) -> Result<QuestionSetVersion, AppError> {
        let hash = create_question_request_hash(&command)?;
        if let Some(version) = self.replay(&command.idempotency_key, &hash)? {
            return Ok(version);
        }
        validate_command(&command.idempotency_key, &command.name, &command.questions)?;
        if command
            .questions
            .iter()
            .any(|question| question.question_id.is_some())
        {
            return Err(AppError::invalid_request(
                "new sets cannot claim question identities by ID",
            ));
        }
        let mut candidate = self.clone();
        let set_id = Uuid::new_v4();
        let version =
            candidate.build_version(set_id, 1, None, command.name, command.questions, None, now)?;
        candidate.sets.push(QuestionSetRecord {
            id: set_id,
            versions: vec![version.clone()],
        });
        candidate.idempotency.push(QuestionIdempotencyRecord {
            key: command.idempotency_key,
            request_hash: hash,
            question_set_id: set_id,
            version_id: version.id,
        });
        *self = candidate;
        Ok(version)
    }

    pub fn revise_set(
        &mut self,
        set_id: Uuid,
        command: ReviseQuestionSet,
        now: DateTime<Utc>,
    ) -> Result<QuestionSetVersion, AppError> {
        let hash = revise_question_request_hash(set_id, &command)?;
        if let Some(version) = self.replay(&command.idempotency_key, &hash)? {
            return Ok(version);
        }
        validate_command(&command.idempotency_key, &command.name, &command.questions)?;
        let base = self
            .sets
            .iter()
            .find(|set| set.id == set_id)
            .and_then(|set| set.versions.last())
            .cloned()
            .ok_or_else(|| AppError::not_found("question set not found"))?;
        if base.id != command.base_version_id {
            return Err(AppError::conflict("question set base version has changed"));
        }
        let next_revision = base
            .revision
            .checked_add(1)
            .ok_or_else(|| AppError::conflict("question set version limit reached"))?;
        let mut candidate = self.clone();
        let version = candidate.build_version(
            set_id,
            next_revision,
            Some(base.id),
            command.name,
            command.questions,
            Some(&base),
            now,
        )?;
        candidate
            .sets
            .iter_mut()
            .find(|set| set.id == set_id)
            .expect("target set exists")
            .versions
            .push(version.clone());
        candidate.idempotency.push(QuestionIdempotencyRecord {
            key: command.idempotency_key,
            request_hash: hash,
            question_set_id: set_id,
            version_id: version.id,
        });
        *self = candidate;
        Ok(version)
    }

    pub fn list_sets(&self, after: Option<Uuid>, limit: u32) -> Result<QuestionSetPage, AppError> {
        let limit = validate_page_limit(limit)?;
        let mut sets: Vec<_> = self
            .sets
            .iter()
            .filter(|set| after.is_none_or(|cursor| set.id > cursor))
            .filter_map(|set| set.versions.last().map(|version| summary(set.id, version)))
            .collect();
        sets.sort_by_key(|set| set.id);
        let next_cursor = (sets.len() > limit).then(|| sets[limit - 1].id);
        sets.truncate(limit);
        Ok(QuestionSetPage {
            items: sets,
            next_cursor,
        })
    }

    pub fn list_versions(
        &self,
        set_id: Uuid,
        after_revision: Option<u32>,
        limit: u32,
    ) -> Result<QuestionSetVersionPage, AppError> {
        let limit = validate_page_limit(limit)?;
        let set = self
            .sets
            .iter()
            .find(|set| set.id == set_id)
            .ok_or_else(|| AppError::not_found("question set not found"))?;
        let mut versions: Vec<_> = set
            .versions
            .iter()
            .filter(|version| after_revision.is_none_or(|cursor| version.revision > cursor))
            .map(version_summary)
            .collect();
        versions.sort_by_key(|version| version.revision);
        let next_cursor = (versions.len() > limit).then(|| versions[limit - 1].revision);
        versions.truncate(limit);
        Ok(QuestionSetVersionPage {
            items: versions,
            next_cursor,
        })
    }

    pub fn get_version(
        &self,
        set_id: Uuid,
        version_id: Uuid,
    ) -> Result<QuestionSetVersion, AppError> {
        self.sets
            .iter()
            .find(|set| set.id == set_id)
            .and_then(|set| set.versions.iter().find(|version| version.id == version_id))
            .cloned()
            .ok_or_else(|| AppError::not_found("question set version not found"))
    }

    pub fn resolve_question(
        &self,
        reference: QuestionReference,
    ) -> Result<ResolvedQuestion, AppError> {
        let version =
            self.get_version(reference.question_set_id, reference.question_set_version_id)?;
        let revision = version
            .questions
            .iter()
            .find(|revision| {
                revision.question_id == reference.question_id
                    && revision.id == reference.question_revision_id
            })
            .cloned()
            .ok_or_else(|| AppError::not_found("question is not a member of this version"))?;
        Ok(ResolvedQuestion {
            binding: FrozenQuestionBinding {
                reference,
                purpose: revision.purpose,
                split_policy_version: revision.split_policy_version.clone(),
            },
            revision,
        })
    }

    pub fn replay(&self, key: &str, hash: &str) -> Result<Option<QuestionSetVersion>, AppError> {
        let Some(entry) = self.idempotency.iter().find(|entry| entry.key == key) else {
            return Ok(None);
        };
        if entry.request_hash != hash {
            return Err(AppError::conflict(
                "idempotency key was used for another question change",
            ));
        }
        // PG should load the indexed response version for a matching key before
        // calling this transition. Missing data must fail closed, not republish.
        self.get_version(entry.question_set_id, entry.version_id)
            .map(Some)
    }

    #[allow(clippy::too_many_arguments)]
    fn build_version(
        &mut self,
        set_id: Uuid,
        revision: u32,
        parent_version_id: Option<Uuid>,
        name: String,
        drafts: Vec<QuestionDraft>,
        base: Option<&QuestionSetVersion>,
        now: DateTime<Utc>,
    ) -> Result<QuestionSetVersion, AppError> {
        let normalized: Vec<_> = drafts
            .iter()
            .map(|draft| normalize_question_text(&draft.text))
            .collect();
        if normalized.iter().collect::<HashSet<_>>().len() != normalized.len() {
            return Err(AppError::conflict(
                "duplicate normalized question within one version",
            ));
        }
        let aliases: HashMap<_, _> = self
            .identities
            .iter()
            .flat_map(|identity| {
                identity
                    .aliases
                    .iter()
                    .map(move |alias| (alias.as_str(), identity.id))
            })
            .collect();
        let base_ids: HashSet<_> = base
            .into_iter()
            .flat_map(|version| version.questions.iter())
            .map(|revision| revision.question_id)
            .collect();
        let mut new_texts = BTreeMap::new();
        let mut resolved = Vec::with_capacity(drafts.len());
        for (draft, norm) in drafts.iter().zip(&normalized) {
            let known_by_alias = aliases.get(norm.as_str()).copied();
            let id = if let Some(id) = draft.question_id {
                if !base_ids.contains(&id) {
                    return Err(AppError::conflict(
                        "edited question is not in the base version",
                    ));
                }
                if known_by_alias.is_some_and(|existing| existing != id) {
                    return Err(AppError::conflict(
                        "question text is reserved by another identity",
                    ));
                }
                if !self.identities.iter().any(|identity| identity.id == id) {
                    return Err(AppError::conflict("question identity is unavailable"));
                }
                Some(id)
            } else {
                known_by_alias
            };
            if id.is_none() {
                new_texts.insert(norm.clone(), ());
            }
            resolved.push(id);
        }
        let new_count = u64::try_from(new_texts.len())
            .map_err(|_| AppError::invalid_request("too many questions"))?;
        let old_evaluation = self.enrolled_count.div_ceil(5);
        let total = self
            .enrolled_count
            .checked_add(new_count)
            .ok_or_else(|| AppError::invalid_request("project question capacity exceeded"))?;
        let evaluation_slots = usize::try_from(total.div_ceil(5) - old_evaluation)
            .map_err(|_| AppError::invalid_request("project question capacity exceeded"))?;
        let mut ranked: Vec<_> = new_texts.keys().cloned().collect();
        ranked.sort_by_key(|text| {
            let digest = Sha256::digest(text.as_bytes());
            let mut hasher = Sha256::new();
            hasher.update(self.seed.as_bytes());
            hasher.update(digest);
            hasher.finalize().to_vec()
        });
        let evaluation: HashSet<_> = ranked.into_iter().take(evaluation_slots).collect();
        for text in new_texts.keys() {
            self.identities.push(QuestionIdentity {
                id: Uuid::new_v4(),
                purpose: if evaluation.contains(text) {
                    QuestionPurpose::FrozenEvaluation
                } else {
                    QuestionPurpose::Optimization
                },
                split_policy_version: QUESTION_SPLIT_POLICY_VERSION.to_owned(),
                aliases: vec![text.clone()],
            });
        }
        self.enrolled_count = total;
        let mut revisions = Vec::with_capacity(drafts.len());
        let mut used_ids = HashSet::new();
        for ((draft, norm), resolved_id) in drafts.into_iter().zip(normalized).zip(resolved) {
            let id = resolved_id.unwrap_or_else(|| {
                self.identities
                    .iter()
                    .find(|identity| identity.aliases.contains(&norm))
                    .expect("newly enrolled identity")
                    .id
            });
            if !used_ids.insert(id) {
                return Err(AppError::conflict(
                    "one identity appears more than once in a version",
                ));
            }
            let identity = self
                .identities
                .iter_mut()
                .find(|identity| identity.id == id)
                .expect("resolved identity loaded");
            if !identity.aliases.contains(&norm) {
                identity.aliases.push(norm);
            }
            let unchanged = base.and_then(|version| {
                version.questions.iter().find(|old| {
                    old.question_id == id
                        && old.text == draft.text
                        && old.intent == draft.intent
                        && old.product_refs == draft.product_refs
                        && old.market == draft.market
                        && old.language == draft.language
                        && old.source == draft.source
                        && old.weight == draft.weight
                })
            });
            revisions.push(unchanged.cloned().unwrap_or_else(|| QuestionRevision {
                id: Uuid::new_v4(),
                question_id: id,
                text: draft.text,
                intent: draft.intent,
                product_refs: draft.product_refs,
                market: draft.market,
                language: draft.language,
                source: draft.source,
                weight: draft.weight,
                purpose: identity.purpose,
                split_policy_version: identity.split_policy_version.clone(),
            }));
        }
        let evaluation_count = revisions
            .iter()
            .filter(|question| question.purpose == QuestionPurpose::FrozenEvaluation)
            .count() as u32;
        // The content digest is independent of the random revision/version IDs.
        let content_hash = request_hash(&(
            &name,
            revisions
                .iter()
                .map(|revision| {
                    (
                        revision.question_id,
                        &revision.text,
                        &revision.intent,
                        &revision.product_refs,
                        &revision.market,
                        &revision.language,
                        &revision.source,
                        revision.weight,
                        revision.purpose,
                    )
                })
                .collect::<Vec<_>>(),
        ))?;
        Ok(QuestionSetVersion {
            id: Uuid::new_v4(),
            question_set_id: set_id,
            revision,
            parent_version_id,
            name,
            optimization_count: revisions.len() as u32 - evaluation_count,
            evaluation_count,
            questions: revisions,
            split_policy_version: QUESTION_SPLIT_POLICY_VERSION.to_owned(),
            content_hash,
            created_at: now,
        })
    }
}

fn summary(id: Uuid, version: &QuestionSetVersion) -> QuestionSetSummary {
    QuestionSetSummary {
        id,
        name: version.name.clone(),
        current_version_id: version.id,
        current_revision: version.revision,
        question_count: version.questions.len() as u32,
        optimization_count: version.optimization_count,
        evaluation_count: version.evaluation_count,
    }
}

fn version_summary(version: &QuestionSetVersion) -> QuestionSetVersionSummary {
    QuestionSetVersionSummary {
        id: version.id,
        question_set_id: version.question_set_id,
        revision: version.revision,
        parent_version_id: version.parent_version_id,
        name: version.name.clone(),
        question_count: version.questions.len() as u32,
        optimization_count: version.optimization_count,
        evaluation_count: version.evaluation_count,
        split_policy_version: version.split_policy_version.clone(),
        created_at: version.created_at,
    }
}

pub fn validate_page_limit(limit: u32) -> Result<usize, AppError> {
    if !(1..=MAX_QUESTION_PAGE_SIZE).contains(&limit) {
        return Err(AppError::invalid_request(
            "question page limit must be between 1 and 100",
        ));
    }
    Ok(limit as usize)
}

fn validate_command(key: &str, name: &str, drafts: &[QuestionDraft]) -> Result<(), AppError> {
    if key.trim().is_empty() || key.len() > 160 {
        return Err(AppError::invalid_request(
            "idempotency key must be 1..160 characters",
        ));
    }
    if name.trim().is_empty() || name.chars().count() > 160 {
        return Err(AppError::invalid_request(
            "question set name must be 1..160 characters",
        ));
    }
    if drafts.is_empty() || drafts.len() > MAX_SET_QUESTIONS {
        return Err(AppError::invalid_request(
            "question set must contain 1..100 questions",
        ));
    }
    for draft in drafts {
        if normalize_question_text(&draft.text).is_empty()
            || draft.text.chars().count() > 2000
            || draft.intent.trim().is_empty()
            || draft.intent.chars().count() > 160
            || draft.market.trim().is_empty()
            || draft.market.chars().count() > 80
            || draft.language.trim().is_empty()
            || draft.language.chars().count() > 80
            || draft.product_refs.len() > 32
            || draft.weight == 0
            || draft.weight > 100
            || (draft.source.kind == QuestionSourceKind::UserProvided
                && draft.source.reference_id.is_some())
        {
            return Err(AppError::invalid_request(
                "question fields are missing or out of bounds",
            ));
        }
        // Product/source IDs require upstream knowledge ownership validation;
        // these raw UUIDs alone do not assert that a referenced fact exists.
    }
    Ok(())
}

fn request_hash<T: Serialize>(input: &T) -> Result<String, AppError> {
    let bytes = serde_json::to_vec(input)
        .map_err(|_| AppError::invalid_request("invalid question payload"))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

/// Persistence uses the same exact hash before loading an indexed historical
/// idempotency response; the key itself is intentionally not part of the hash.
pub fn create_question_request_hash(command: &CreateQuestionSet) -> Result<String, AppError> {
    request_hash(&("create", &command.name, &command.questions))
}

pub fn revise_question_request_hash(
    set_id: Uuid,
    command: &ReviseQuestionSet,
) -> Result<String, AppError> {
    request_hash(&(
        "revise",
        set_id,
        command.base_version_id,
        &command.name,
        &command.questions,
    ))
}

#[async_trait]
pub trait QuestionRepository: Send + Sync {
    /// Read-only exact idempotency lookup. Call before checking *current*
    /// knowledge references, since their validity may have changed after the
    /// original immutable version was published. A miss is not permission to
    /// bypass live validation before creating a new version.
    async fn replay(
        &self,
        scope: &TenantScope,
        key: &str,
        request_hash: &str,
    ) -> Result<Option<QuestionSetVersion>, AppError>;
    async fn list_sets(
        &self,
        scope: &TenantScope,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<QuestionSetPage, AppError>;
    async fn create_set(
        &self,
        scope: &TenantScope,
        command: CreateQuestionSet,
    ) -> Result<QuestionSetVersion, AppError>;
    async fn list_versions(
        &self,
        scope: &TenantScope,
        set_id: Uuid,
        after_revision: Option<u32>,
        limit: u32,
    ) -> Result<QuestionSetVersionPage, AppError>;
    async fn get_version(
        &self,
        scope: &TenantScope,
        set_id: Uuid,
        version_id: Uuid,
    ) -> Result<QuestionSetVersion, AppError>;
    async fn revise_set(
        &self,
        scope: &TenantScope,
        set_id: Uuid,
        command: ReviseQuestionSet,
    ) -> Result<QuestionSetVersion, AppError>;
    async fn resolve_question(
        &self,
        scope: &TenantScope,
        reference: QuestionReference,
    ) -> Result<ResolvedQuestion, AppError>;
}

#[derive(Debug, Default, Clone)]
pub struct MemoryQuestionRepository(Arc<Mutex<HashMap<String, QuestionProjectState>>>);

fn project_key(scope: &TenantScope) -> Result<String, AppError> {
    if scope.project_id.is_none() {
        return Err(AppError::invalid_request(
            "question operations require a project scope",
        ));
    }
    Ok(scope.storage_key())
}

#[async_trait]
impl QuestionRepository for MemoryQuestionRepository {
    async fn replay(
        &self,
        scope: &TenantScope,
        key: &str,
        request_hash: &str,
    ) -> Result<Option<QuestionSetVersion>, AppError> {
        let state = self.0.lock().await;
        match state.get(&project_key(scope)?) {
            Some(project) => project.replay(key, request_hash),
            None => Ok(None),
        }
    }

    async fn list_sets(
        &self,
        scope: &TenantScope,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<QuestionSetPage, AppError> {
        validate_page_limit(limit)?;
        let state = self.0.lock().await;
        match state.get(&project_key(scope)?) {
            Some(project) => project.list_sets(after, limit),
            None => Ok(QuestionSetPage {
                items: Vec::new(),
                next_cursor: None,
            }),
        }
    }
    async fn create_set(
        &self,
        scope: &TenantScope,
        command: CreateQuestionSet,
    ) -> Result<QuestionSetVersion, AppError> {
        let mut state = self.0.lock().await;
        state
            .entry(project_key(scope)?)
            .or_insert_with(|| QuestionProjectState::new(Uuid::new_v4()))
            .create_set(command, Utc::now())
    }
    async fn list_versions(
        &self,
        scope: &TenantScope,
        set_id: Uuid,
        after_revision: Option<u32>,
        limit: u32,
    ) -> Result<QuestionSetVersionPage, AppError> {
        let state = self.0.lock().await;
        state
            .get(&project_key(scope)?)
            .ok_or_else(|| AppError::not_found("question set not found"))?
            .list_versions(set_id, after_revision, limit)
    }
    async fn get_version(
        &self,
        scope: &TenantScope,
        set_id: Uuid,
        version_id: Uuid,
    ) -> Result<QuestionSetVersion, AppError> {
        let state = self.0.lock().await;
        state
            .get(&project_key(scope)?)
            .ok_or_else(|| AppError::not_found("question set version not found"))?
            .get_version(set_id, version_id)
    }
    async fn revise_set(
        &self,
        scope: &TenantScope,
        set_id: Uuid,
        command: ReviseQuestionSet,
    ) -> Result<QuestionSetVersion, AppError> {
        let mut state = self.0.lock().await;
        state
            .get_mut(&project_key(scope)?)
            .ok_or_else(|| AppError::not_found("question set not found"))?
            .revise_set(set_id, command, Utc::now())
    }
    async fn resolve_question(
        &self,
        scope: &TenantScope,
        reference: QuestionReference,
    ) -> Result<ResolvedQuestion, AppError> {
        let state = self.0.lock().await;
        state
            .get(&project_key(scope)?)
            .ok_or_else(|| AppError::not_found("question set version not found"))?
            .resolve_question(reference)
    }
}
