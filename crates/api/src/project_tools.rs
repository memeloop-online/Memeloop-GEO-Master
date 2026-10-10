//! Project onboarding uses the same repositories and validation as HTTP.
//! Scope comes exclusively from the authorised Rust run.

use geo_domain::{
    AppError, ErrorCode, EventEnvelope, IdempotencyDecision, InitialSource, InitialSourceKind,
    InitialSourceVisibility, KnowledgePurpose, Operation, Project, ProjectCreate,
    ProjectStartAcceptance, ProjectStartCommand, SourceState, StoredResponse, TenantScope,
    hash_idempotency_key, settings_hash, start_request_hash,
};
use geo_worker::{ProjectCurrentResult, ProjectReviseRequest};
use serde_json::{Value, json};

use crate::{AppState, ProjectPatchRequest, estimate_project, project_start_operation_id};

fn serialization_error(_: serde_json::Error) -> AppError {
    AppError::new(
        ErrorCode::Internal,
        "project tool response serialization failed",
    )
}

fn projection(mut project: Project) -> ProjectCurrentResult {
    let settings = &project.settings;
    let mut missing_fields = Vec::new();
    if settings.brand_name.is_empty() {
        missing_fields.push("brand_name".to_owned());
    }
    if settings.initial_sources.is_empty() {
        missing_fields.push("initial_sources".to_owned());
    }
    if settings.effective_markets().is_empty() {
        missing_fields.push("document_scope.markets".to_owned());
    }
    if settings.effective_languages().is_empty() {
        missing_fields.push("document_scope.languages".to_owned());
    }
    if !settings.document_scope.all_active_products {
        missing_fields.push("document_scope.all_active_products".to_owned());
    }
    if settings.distribution_scope.mode == geo_domain::DistributionScopeMode::Explicit
        && settings.distribution_scope.included_platform_ids.is_empty()
    {
        missing_fields.push("distribution_scope.included_platform_ids".to_owned());
    }
    let initial_source_count = project.settings.initial_sources.len();
    // Original source bodies and locator query strings belong in the scoped
    // knowledge evidence tools, not wholesale in the model's onboarding read.
    project.settings.initial_sources.clear();
    ProjectCurrentResult {
        project,
        missing_fields,
        initial_source_count,
        initial_sources_redacted: initial_source_count > 0,
    }
}

pub(crate) async fn current(
    state: &AppState,
    scope: &TenantScope,
) -> Result<ProjectCurrentResult, AppError> {
    Ok(projection(load_project(state, scope).await?))
}

async fn load_project(state: &AppState, scope: &TenantScope) -> Result<Project, AppError> {
    let id = scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))?;
    state
        .project_repository()
        .get(scope, id)
        .await?
        .ok_or_else(|| AppError::not_found("project not found"))
}

pub(crate) async fn estimate(state: &AppState, scope: &TenantScope) -> Result<Value, AppError> {
    let project = load_project(state, scope).await?;
    serde_json::to_value(estimate_project(
        ProjectCreate {
            slug: Some(project.slug),
            display_name: project.display_name,
            settings: project.settings,
        },
        scope,
    )?)
    .map_err(serialization_error)
}

pub(crate) async fn revise(
    state: &AppState,
    scope: &TenantScope,
    request: ProjectReviseRequest,
) -> Result<ProjectCurrentResult, AppError> {
    request.validate().map_err(AppError::invalid_request)?;
    let project = load_project(state, scope).await?;
    let key = format!(
        "project.revise.v1:{}",
        hash_idempotency_key(request.idempotency_key.trim())
    );
    let body = serde_json::to_vec(&request).map_err(serialization_error)?;
    let hash = crate::idempotency::body_hash(&body);
    let store = state.idempotency_store();
    let token = match store.begin(scope, &key, &hash).await? {
        IdempotencyDecision::New(token) => token,
        IdempotencyDecision::InFlight => {
            return Err(AppError::conflict(
                "project revision request is in progress",
            ));
        }
        IdempotencyDecision::Replay(response) => {
            let result: Result<ProjectCurrentResult, AppError> =
                serde_json::from_slice(&response.body).map_err(serialization_error)?;
            return result;
        }
    };
    let result = apply_revision(state, scope, project, request).await;
    store
        .complete(
            &token,
            StoredResponse {
                status: result
                    .as_ref()
                    .map_or_else(|error| error.code.default_status(), |_| 200),
                content_type: "application/json".to_owned(),
                body: serde_json::to_vec(&result).map_err(serialization_error)?,
            },
        )
        .await?;
    result
}

async fn apply_revision(
    state: &AppState,
    scope: &TenantScope,
    project: Project,
    request: ProjectReviseRequest,
) -> Result<ProjectCurrentResult, AppError> {
    // Keep strict HTTP null/clear and nested-settings semantics. Domain patch
    // deserialization alone would accept unknown selectors and lifecycle fields.
    let input: ProjectPatchRequest = serde_json::from_value(request.patch.clone())
        .map_err(|_| AppError::invalid_request("invalid project configuration patch"))?;
    let (revision, mut patch) = input.into_domain();
    if revision.is_some_and(|revision| revision != request.expected_revision) {
        return Err(AppError::conflict(
            "request revision does not match expected_revision",
        ));
    }
    if patch.initial_sources.as_ref().is_some_and(|sources| {
        sources.iter().any(|source| {
            !matches!(
                source.kind,
                InitialSourceKind::Text | InitialSourceKind::Url
            ) || source.version_ref.is_some()
                || source.content_hash.is_some()
        })
    }) {
        return Err(AppError::invalid_request(
            "immutable imported sources must use scoped source_version_ids",
        ));
    }
    if !request.source_version_ids.is_empty() {
        if patch.initial_sources.is_some() {
            return Err(AppError::invalid_request(
                "use source_version_ids or initial_sources, not both",
            ));
        }
        let knowledge = state.knowledge_repository();
        let release_id = knowledge
            .current_release(scope)
            .await?
            .knowledge_release_id
            .ok_or_else(|| AppError::not_ready("imported knowledge is not ready"))?;
        let release = knowledge
            .get_release(scope, release_id)
            .await?
            .ok_or_else(|| AppError::not_ready("knowledge release not found"))?;
        let sources = knowledge.list_sources(scope).await?;
        let mut initial_sources = project.settings.initial_sources.clone();
        for version_id in &request.source_version_ids {
            if !release.source_version_refs.contains(version_id) {
                return Err(AppError::invalid_request(
                    "source version is not in the current ready knowledge release",
                ));
            }
            let mut resolved = None;
            for source in &sources {
                if source.state != SourceState::Active {
                    continue;
                }
                if let Some(version) = knowledge
                    .get_source_version(scope, source.source_id, *version_id)
                    .await?
                {
                    resolved = Some(InitialSource {
                        kind: InitialSourceKind::KnowledgeCollection,
                        value: release_id.to_string(),
                        visibility: match source.purpose {
                            KnowledgePurpose::Public => InitialSourceVisibility::Public,
                            KnowledgePurpose::Internal => InitialSourceVisibility::Internal,
                        },
                        version_ref: Some(version.source_version_id.to_string()),
                        content_hash: Some(version.content_sha256),
                    });
                    break;
                }
            }
            let source = resolved
                .ok_or_else(|| AppError::not_found("active scoped source version not found"))?;
            if !initial_sources
                .iter()
                .any(|existing| existing.version_ref == source.version_ref)
            {
                initial_sources.push(source);
            }
        }
        patch.initial_sources = Some(initial_sources);
    }
    state
        .project_repository()
        .update(scope, project.id, request.expected_revision, patch)
        .await
        .map(|updated| projection(updated.project))
}

/// Shared by HTTP and P00: atomic authoritative start, including stable
/// operation lookup and development events. This never grants paid authority.
pub(crate) async fn start(
    state: &AppState,
    scope: &TenantScope,
    expected_revision: i64,
    idempotency_key: &str,
) -> Result<ProjectStartAcceptance, AppError> {
    let idempotency_key = idempotency_key.trim();
    if idempotency_key.is_empty() {
        return Err(AppError::invalid_request(
            "Idempotency-Key must not be empty",
        ));
    }
    let project = load_project(state, scope).await?;
    let normalized = project.settings.clone().validate_draft()?;
    let frozen_settings_hash = settings_hash(&normalized)?;
    let command = ProjectStartCommand {
        expected_revision,
        idempotency_key_hash: hash_idempotency_key(idempotency_key),
        request_hash: start_request_hash(project.id, expected_revision, &frozen_settings_hash),
        settings_hash: frozen_settings_hash,
        operation_id: project_start_operation_id(scope, idempotency_key),
    };
    let acceptance = state
        .project_repository()
        .start(scope, project.id, command)
        .await
        .map_err(|error| {
            if error.code == ErrorCode::InvalidRequest {
                error.with_details(json!({ "missing_fields": projection(project).missing_fields }))
            } else {
                error
            }
        })?;
    if !state.durable_storage() {
        let mut operation = Operation::queued("project.start", scope.clone());
        operation.id = acceptance.operation_id;
        operation.result = Some(serde_json::to_value(&acceptance).map_err(serialization_error)?);
        state.operation_store().save(operation).await?;
        state.publish_event(EventEnvelope::new(
            "cycle.created",
            scope.clone(),
            acceptance.cycle_id,
            1,
            acceptance.operation_id,
        ));
    }
    Ok(acceptance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_domain::{
        ImportItem, MemoryKnowledgeRepository, OperatorId, ProjectSettings, SourceKind, TenantId,
    };
    use std::sync::Arc;
    use uuid::Uuid;

    async fn fixture() -> (AppState, TenantScope) {
        let state = AppState::development();
        let tenant = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            None,
        );
        let project = state
            .project_repository()
            .create(
                &tenant,
                ProjectCreate {
                    slug: None,
                    display_name: "Draft workspace".to_owned(),
                    settings: ProjectSettings::default(),
                },
            )
            .await
            .unwrap();
        (state, project.scope())
    }

    fn request(revision: i64, key: &str, patch: Value) -> ProjectReviseRequest {
        ProjectReviseRequest {
            expected_revision: revision,
            idempotency_key: key.to_owned(),
            patch,
            source_version_ids: vec![],
        }
    }

    #[tokio::test]
    async fn repository_host_uses_only_bound_project_scope_and_explicit_state() {
        use geo_worker::{HostOpErrorCode, HostOps, ProjectCurrentRequest};
        let (state, scope) = fixture().await;
        let unavailable = crate::RepositoryHostOps::new(state.knowledge_repository());
        assert_eq!(
            unavailable
                .project_current(&scope, ProjectCurrentRequest {})
                .await
                .unwrap_err()
                .code,
            HostOpErrorCode::CapabilityMissing
        );
        let host = unavailable.with_content(state);
        let read = host
            .project_current(&scope, ProjectCurrentRequest {})
            .await
            .unwrap();
        read.validate_for(&scope).unwrap();
        let tenant_scope = TenantScope::new(scope.operator_id, scope.tenant_id, None);
        assert!(
            host.project_current(&tenant_scope, ProjectCurrentRequest {})
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn blank_draft_reports_missing_inputs_and_estimates_without_writes() {
        let (state, scope) = fixture().await;
        let read = current(&state, &scope).await.unwrap();
        assert_eq!(read.missing_fields.len(), 4);
        assert_eq!(read.project.settings.monthly_budget_minor, 0);
        let estimate = estimate(&state, &scope).await.unwrap();
        assert_eq!(estimate["costs"]["total"]["state"], "unknown");
        assert_eq!(current(&state, &scope).await.unwrap().project.revision, 1);
        let error = start(&state, &scope, 1, "blank-start").await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert_eq!(
            error.details.unwrap()["missing_fields"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
        let foreign = TenantScope::new(
            scope.operator_id,
            TenantId::new(Uuid::new_v4()),
            scope.project_id,
        );
        assert_eq!(
            current(&state, &foreign).await.unwrap_err().code,
            ErrorCode::NotFound
        );
    }

    #[tokio::test]
    async fn draft_revision_is_strict_optimistic_and_persistently_replayable() {
        let (state, scope) = fixture().await;
        for (index, patch) in [
            json!({"status":"active"}), json!({"project_id":Uuid::new_v4()}),
            json!({"settings":{"tenant_id":Uuid::new_v4()}}),
            json!({"initial_sources":[{"kind":"object","value":Uuid::new_v4(),"visibility":"public"}]}),
        ].into_iter().enumerate() {
            assert_eq!(revise(&state, &scope, request(1, &format!("invalid-{index}"), patch)).await.unwrap_err().code, ErrorCode::InvalidRequest);
        }
        let update = request(1, "brand", json!({"brand_name":"Example brand"}));
        let first = revise(&state, &scope, update.clone()).await.unwrap();
        let replay = revise(&state, &scope, update.clone()).await.unwrap();
        assert_eq!(first.project, replay.project);
        assert_eq!(first.project.revision, 2);
        assert_eq!(
            revise(
                &state,
                &scope,
                request(1, "stale", json!({"market":"Global"}))
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::Conflict
        );
        assert_eq!(
            revise(
                &state,
                &scope,
                request(1, "brand", json!({"brand_name":"Other"}))
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::Conflict
        );
    }

    #[tokio::test]
    async fn ready_scoped_imports_bind_start_and_replay_survives_unavailable_knowledge() {
        let (mut state, scope) = fixture().await;
        let imported = state
            .knowledge_repository()
            .import_batch(
                &scope,
                vec![ImportItem {
                    client_item_id: "guide".to_owned(),
                    kind: SourceKind::Text,
                    name: "Guide".to_owned(),
                    purpose: KnowledgePurpose::Public,
                    text: Some("Example product has a two year warranty.".to_owned()),
                    url: None,
                    object_id: None,
                    knowledge_release_id: None,
                }],
            )
            .await
            .unwrap();
        let version = imported.items[0].source_version.as_ref().unwrap();
        let mut update = request(
            1,
            "ready",
            json!({"brand_name":"Example", "market":"Global", "language":"en"}),
        );
        update.source_version_ids.push(version.source_version_id);
        let ready = revise(&state, &scope, update.clone()).await.unwrap();
        assert!(ready.missing_fields.is_empty());
        assert!(ready.initial_sources_redacted);
        assert_eq!(ready.initial_source_count, 1);
        assert!(ready.project.settings.initial_sources.is_empty());
        let stored = load_project(&state, &scope).await.unwrap();
        assert_eq!(
            stored.settings.initial_sources[0].version_ref,
            Some(version.source_version_id.to_string())
        );
        assert_eq!(
            stored.settings.initial_sources[0].content_hash,
            Some(version.content_sha256.clone())
        );
        assert_eq!(
            estimate(&state, &scope).await.unwrap()["settings_hash"],
            settings_hash(&stored.settings).unwrap()
        );
        state.knowledge_repository = Arc::new(MemoryKnowledgeRepository::default());
        assert_eq!(
            revise(&state, &scope, update).await.unwrap().project,
            ready.project
        );
        let acceptance = start(&state, &scope, ready.project.revision, "start-ready")
            .await
            .unwrap();
        let replay = start(&state, &scope, ready.project.revision, "start-ready")
            .await
            .unwrap();
        assert_eq!(acceptance.operation_id, replay.operation_id);
        assert_eq!(acceptance.cycle_id, replay.cycle_id);
        assert_eq!(
            current(&state, &scope)
                .await
                .unwrap()
                .project
                .settings
                .monthly_budget_minor,
            0
        );
        assert!(
            state
                .operation_store()
                .get(&scope, acceptance.operation_id)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn source_inputs_are_redacted_only_from_model_projection() {
        let (state, scope) = fixture().await;
        let update = request(
            1,
            "source-input",
            json!({
                "brand_name":"Example", "market":"Global", "language":"en",
                "initial_sources":[{"kind":"text","value":"SYNTHETIC_SOURCE_BODY_MARKER","visibility":"public"}]
            }),
        );
        let read = revise(&state, &scope, update).await.unwrap();
        let serialized = serde_json::to_string(&read).unwrap();
        assert!(!serialized.contains("SYNTHETIC_SOURCE_BODY_MARKER"));
        assert_eq!(read.initial_source_count, 1);
        assert!(read.initial_sources_redacted);
        assert!(read.missing_fields.is_empty());
        let stored = load_project(&state, &scope).await.unwrap();
        assert_eq!(
            estimate(&state, &scope).await.unwrap()["settings_hash"],
            settings_hash(&stored.settings).unwrap()
        );
        start(&state, &scope, read.project.revision, "redacted-start")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn arbitrary_or_foreign_versions_do_not_fill_source_requirement() {
        let (state, scope) = fixture().await;
        let mut update = request(1, "unready", json!({}));
        update.source_version_ids.push(Uuid::new_v4());
        assert_eq!(
            revise(&state, &scope, update.clone())
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotReady
        );
        assert_eq!(
            revise(&state, &scope, update).await.unwrap_err().code,
            ErrorCode::NotReady
        );
        assert!(
            current(&state, &scope)
                .await
                .unwrap()
                .project
                .settings
                .initial_sources
                .is_empty()
        );
    }
}
