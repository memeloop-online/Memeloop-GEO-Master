//! Read-only project availability and resource-admin-only connector settings.
//! Neither HTTP route can create or modify publication verification evidence.

use std::collections::BTreeSet;

use axum::{
    Json,
    extract::{Extension, Path, State},
};
use geo_domain::{
    AppError, ConnectorAvailability, ConnectorKey, ConnectorSettings, PLAIN_TEXT_ARTICLE_FORMAT,
    ProjectId, TenantScope, publication_format_for_semantic_type,
};
use serde::{Deserialize, Serialize};

use crate::{ApiError, AppState, AuthContext, RequestContext, api_error, channels::pool_tenant};

#[derive(Debug, Serialize)]
pub struct ConnectorCapabilityList {
    pub items: Vec<ConnectorCapabilityView>,
}

#[derive(Debug, Serialize)]
pub struct ConnectorCapabilityView {
    pub platform_id: String,
    pub placement_slot: String,
    pub revision: i32,
    pub enabled: bool,
    pub content_types: Vec<String>,
    pub availability: ConnectorAvailability,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployed_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_content_types: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigureConnector {
    pub expected_revision: i32,
    pub enabled: bool,
    pub content_types: Vec<String>,
}

fn catalog(settings: &[ConnectorSettings]) -> Vec<ConnectorKey> {
    let mut keys: BTreeSet<(String, String)> = ["baidu_creator", "xiaohongshu", "zhihu"]
        .into_iter()
        .map(|id| (id.to_owned(), "primary".to_owned()))
        .collect();
    keys.extend(
        settings
            .iter()
            .map(|row| (row.key.platform_id.clone(), row.key.placement_slot.clone())),
    );
    keys.into_iter()
        .map(|(platform_id, placement_slot)| ConnectorKey {
            platform_id,
            placement_slot,
        })
        .collect()
}

/// Resolve a generated document's semantic type to the proven publication
/// wire format. Old explicitly proven semantic settings remain readable; an
/// unrecognized/media semantic never inherits a plain-text article proof.
pub(crate) fn configured_publication_format<'a>(
    settings: &ConnectorSettings,
    semantic: &'a str,
) -> Option<&'a str> {
    let wire = publication_format_for_semantic_type(semantic)?;
    if settings
        .content_types
        .iter()
        .any(|configured| configured == wire)
    {
        Some(wire)
    } else if settings
        .content_types
        .iter()
        .any(|configured| configured == semantic)
    {
        Some(semantic)
    } else {
        None
    }
}

/// Source articles carry no document semantic label. Unlike generated
/// documents, old semantic proofs cannot authorize their plain-text payload.
pub(crate) fn configured_source_format(settings: &ConnectorSettings) -> Option<&'static str> {
    settings
        .content_types
        .iter()
        .any(|configured| configured == PLAIN_TEXT_ARTICLE_FORMAT)
        .then_some(PLAIN_TEXT_ARTICLE_FORMAT)
}

pub(crate) async fn deployed_versions(
    state: &AppState,
) -> Vec<crate::browser_bridge::RunnerConnector> {
    let Some(browser) = state.channel_service().browser.as_ref() else {
        return Vec::new();
    };
    // Runner failure/old protocol is unavailable, not permission to trust a
    // saved proof from another binary or a user-supplied version.
    browser
        .capabilities()
        .await
        .map(|result| result.connectors)
        .unwrap_or_default()
}

pub(crate) fn deployed_version<'a>(
    connectors: &'a [crate::browser_bridge::RunnerConnector],
    key: &ConnectorKey,
) -> Option<&'a str> {
    let mut matching = connectors
        .iter()
        .filter(|entry| {
            entry.platform == key.platform_id
                && entry.placement_slot == key.placement_slot
                && entry
                    .operations
                    .iter()
                    .any(|operation| operation == "publish")
                && !entry.connector_version.is_empty()
        })
        .map(|entry| entry.connector_version.as_str());
    let first = matching.next()?;
    matching.next().is_none().then_some(first)
}

async fn view(
    state: &AppState,
    settings: Option<ConnectorSettings>,
    key: ConnectorKey,
    connectors: &[crate::browser_bridge::RunnerConnector],
    operator: geo_domain::OperatorId,
    admin: bool,
) -> Result<ConnectorCapabilityView, AppError> {
    let version = deployed_version(connectors, &key);
    let history = state
        .connector_capability_repository()
        .history(operator, &key)
        .await?;
    let verified: BTreeSet<String> = history
        .iter()
        .filter(|proof| Some(proof.connector_version.as_str()) == version)
        .map(|proof| proof.content_type.clone())
        .collect();
    let availability = if let Some(ref current) = settings {
        if !current.enabled {
            ConnectorAvailability::Disabled
        } else if version.is_none() || verified.is_empty() {
            ConnectorAvailability::VersionMismatch
        } else if current
            .content_types
            .iter()
            .any(|kind| verified.contains(kind))
        {
            ConnectorAvailability::Available
        } else {
            ConnectorAvailability::UnsupportedContentType
        }
    } else {
        ConnectorAvailability::Unavailable
    };
    Ok(ConnectorCapabilityView {
        platform_id: key.platform_id,
        placement_slot: key.placement_slot,
        revision: settings.as_ref().map_or(0, |row| row.revision),
        enabled: settings.as_ref().is_some_and(|row| row.enabled),
        content_types: settings.map_or_else(Vec::new, |row| {
            if admin {
                row.content_types
            } else if availability == ConnectorAvailability::Available {
                row.content_types
                    .into_iter()
                    .filter(|kind| verified.contains(kind))
                    .collect()
            } else {
                Vec::new()
            }
        }),
        availability,
        deployed_version: admin.then(|| version.map(str::to_owned)).flatten(),
        verified_content_types: admin.then(|| verified.into_iter().collect()),
    })
}

pub async fn list_operator(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<ConnectorCapabilityList>, ApiError> {
    pool_tenant(state.channel_service(), &auth).map_err(|e| api_error(e, context.request_id))?;
    let operator = auth.operator.id;
    let settings = state
        .connector_capability_repository()
        .list(operator)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    let connectors = deployed_versions(&state).await;
    let mut items = Vec::new();
    for key in catalog(&settings) {
        let current = settings.iter().find(|row| row.key == key).cloned();
        items.push(
            view(&state, current, key, &connectors, operator, true)
                .await
                .map_err(|e| api_error(e, context.request_id))?,
        );
    }
    Ok(Json(ConnectorCapabilityList { items }))
}

pub async fn configure_operator(
    State(state): State<AppState>,
    Path((platform_id, placement_slot)): Path<(String, String)>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<ConfigureConnector>,
) -> Result<Json<ConnectorCapabilityView>, ApiError> {
    pool_tenant(state.channel_service(), &auth).map_err(|e| api_error(e, context.request_id))?;
    let key = ConnectorKey {
        platform_id,
        placement_slot,
    };
    key.validate()
        .map_err(|e| api_error(e, context.request_id))?;
    let connectors = deployed_versions(&state).await;
    let version = deployed_version(&connectors, &key).unwrap_or("");
    let settings = state
        .connector_capability_repository()
        .configure(
            auth.operator.id,
            key.clone(),
            input.expected_revision,
            input.enabled,
            input.content_types,
            version,
        )
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    Ok(Json(
        view(
            &state,
            Some(settings),
            key,
            &connectors,
            auth.operator.id,
            true,
        )
        .await
        .map_err(|e| api_error(e, context.request_id))?,
    ))
}

pub async fn list_project(
    State(state): State<AppState>,
    Path(project_id): Path<ProjectId>,
    Extension(scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<ConnectorCapabilityList>, ApiError> {
    state
        .project_repository()
        .get(&scope, project_id)
        .await
        .map_err(|e| api_error(e, context.request_id))?
        .ok_or_else(|| api_error(AppError::not_found("project not found"), context.request_id))?;
    let operator = scope.operator_id;
    let settings = state
        .connector_capability_repository()
        .list(operator)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    let connectors = deployed_versions(&state).await;
    let mut items = Vec::new();
    for key in catalog(&settings) {
        let current = settings.iter().find(|row| row.key == key).cloned();
        items.push(
            view(&state, current, key, &connectors, operator, false)
                .await
                .map_err(|e| api_error(e, context.request_id))?,
        );
    }
    Ok(Json(ConnectorCapabilityList { items }))
}

#[cfg(test)]
#[path = "connector_capabilities_tests.rs"]
mod tests;
