//! Scoped recommendation read shared by human evidence inspection and AI optimization.
use std::collections::BTreeMap;

use axum::{
    Json,
    extract::{Extension, Path, Query, State},
};
use geo_domain::{
    AppError, ChannelPublicationStatus, ChannelStatus, ConnectorAvailability, ConnectorKey,
    PLAIN_TEXT_ARTICLE_FORMAT, ProjectId, RecommendationAudience, SourceChannelRecommendationPage,
    TenantScope, mapped_source_channel_keys, recommend_sources,
};

use crate::{ApiError, AppState, RequestContext, api_error, citation_insights};

pub(crate) async fn read(
    state: &AppState,
    scope: &TenantScope,
    after: Option<uuid::Uuid>,
    limit: Option<usize>,
    audience: RecommendationAudience,
) -> Result<SourceChannelRecommendationPage, AppError> {
    let project_id = scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))?;
    let project = state
        .project_repository()
        .get(scope, project_id)
        .await?
        .ok_or_else(|| AppError::not_found("project not found"))?;
    let citations = match audience {
        RecommendationAudience::Human => {
            citation_insights::read_citation_page(state, scope, after, limit).await?
        }
        RecommendationAudience::Optimization => {
            citation_insights::read_optimization_citation_page(state, scope, after, limit).await?
        }
    };
    // The filtered repository guarantees these IDs belong to plans containing
    // an immutable optimization-bound target. Even a plan without accepted
    // citations is a safe cursor; it must not falsely terminate pagination.
    let safe_cursor = citations.next_after;
    let safe_plan_ids = citations.plan_ids.clone();
    let connectors = crate::connector_capabilities::deployed_versions(state).await;
    let channels = &state.channel_service().repository;
    let mut accounts = channels.list_accounts(scope).await?;
    accounts.extend(
        channels
            .list_assigned_pool_accounts(scope)
            .await?
            .into_iter()
            .map(|account| account.assigned_view(project_id)),
    );
    let mut statuses = BTreeMap::new();
    for (platform, placement) in mapped_source_channel_keys() {
        let key = ConnectorKey {
            platform_id: platform.to_owned(),
            placement_slot: placement.to_owned(),
        };
        let version =
            crate::connector_capabilities::deployed_version(&connectors, &key).unwrap_or("");
        let resolved = state
            .connector_capability_repository()
            .resolve(scope.operator_id, &key, version, PLAIN_TEXT_ARTICLE_FORMAT)
            .await?;
        let account_ready = accounts.iter().any(|account| {
            account.platform == platform
                && account.enabled
                && account.status == ChannelStatus::Ready
        });
        let availability = match resolved.availability {
            ConnectorAvailability::Unavailable => "unavailable",
            ConnectorAvailability::Disabled => "disabled",
            ConnectorAvailability::VersionMismatch => "version_mismatch",
            ConnectorAvailability::UnsupportedContentType => "unsupported_content_type",
            ConnectorAvailability::Available => "available",
        };
        let reason = if availability != "available" {
            Some(format!("connector_{availability}"))
        } else if !account_ready {
            Some("project_account_not_ready".into())
        } else {
            // Distribution still checks source, account, budget and publication
            // eligibility at materialization/send time. Never claim ready here.
            Some("send_time_eligibility_required".into())
        };
        statuses.insert(
            (platform, placement),
            ChannelPublicationStatus {
                connector_availability: availability.into(),
                account_ready,
                reason,
            },
        );
    }
    let mut result = recommend_sources(
        citations,
        &project.settings.distribution_scope,
        audience,
        |platform, placement| {
            statuses
                .get(&(platform, placement))
                .cloned()
                .expect("mapped rule has status")
        },
    );
    if matches!(audience, RecommendationAudience::Optimization) {
        result.next_after = safe_cursor;
        result.plan_ids = safe_plan_ids;
    }
    Ok(result)
}

pub async fn get(
    State(state): State<AppState>,
    Path(project_id): Path<ProjectId>,
    Query(query): Query<citation_insights::CitationInsightsQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<SourceChannelRecommendationPage>, ApiError> {
    let result = async {
        let _ = (query.tenant_id, query.project_id);
        let scope = crate::channel_jobs::scope(&state, &tenant, project_id).await?;
        read(
            &state,
            &scope,
            query.after,
            query.limit,
            RecommendationAudience::Human,
        )
        .await
    }
    .await;
    result
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}
