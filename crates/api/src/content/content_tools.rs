//! Scoped content execution and explicit frozen-source reads for the workflow.

use geo_domain::{AppError, ContentExecution, ContentItem, TenantScope, sha256_hex};
use geo_worker::{
    ContentCloseRequest, ContentExecutionReadRequest, ContentExecutionRef, ContentHandoffRef,
    ContentItemRef, ContentItemsPage, ContentItemsReadRequest, ContentPrepareRequest,
    ContentSourcePage, ContentSourceReadRequest, ContentStartRequest, ContentStepRequest,
};
use uuid::Uuid;

use crate::AppState;

fn execution_ref(execution: ContentExecution) -> ContentExecutionRef {
    ContentExecutionRef {
        execution_id: execution.execution_id,
        cycle_id: execution.cycle_id,
        status: execution.status,
        coverage: execution.coverage,
    }
}

async fn item_ref(
    state: &AppState,
    scope: &TenantScope,
    item: ContentItem,
) -> Result<ContentItemRef, AppError> {
    let source_refs = state.content_service().source_refs(scope, &item).await?;
    let reusable_evidence = state
        .content_service()
        .reusable_evidence(scope, &item)
        .await?;
    Ok(ContentItemRef {
        item_id: item.item_id,
        branch_key: item.branch_key,
        document_key: item.document_key,
        source_refs,
        reusable_evidence,
        status: item.status,
        automatic_repair_count: item.automatic_repair_count,
    })
}

async fn scoped_execution(
    state: &AppState,
    scope: &TenantScope,
    execution_id: Uuid,
) -> Result<ContentExecution, AppError> {
    let execution = state
        .content_service()
        .repository()
        .get_execution(scope, execution_id)
        .await?
        .ok_or_else(|| AppError::not_found("content execution not found"))?;
    if scope.project_id != Some(execution.project_id) {
        return Err(AppError::forbidden(
            "content execution is outside project scope",
        ));
    }
    Ok(execution)
}

pub(crate) async fn start(
    state: &AppState,
    scope: &TenantScope,
    request: ContentStartRequest,
) -> Result<ContentExecutionRef, AppError> {
    let project_id = scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))?;
    if !state.content_executor_available() {
        return Err(AppError::capability_missing(
            "content workflow executor is not configured",
        ));
    }
    let cycle_id = match request.cycle_id {
        Some(cycle_id) => {
            state
                .project_repository()
                .get_report_cycle(scope, project_id, cycle_id)
                .await?
                .ok_or_else(|| AppError::not_found("cycle not found"))?;
            cycle_id
        }
        None => state
            .project_repository()
            .get_current_cycle(scope, project_id)
            .await?
            .map(|cycle| cycle.cycle_id)
            .ok_or_else(|| AppError::not_found("current cycle not found"))?,
    };
    let execution = state.content_service().start(scope, cycle_id).await?;
    // Acceptance means the real, configured engine accepted dispatch. The
    // durable execution remains resumable if a subsequent run is interrupted.
    state.dispatch_content_execution(scope.clone(), execution.execution_id)?;
    Ok(execution_ref(execution))
}

pub(crate) async fn read(
    state: &AppState,
    scope: &TenantScope,
    request: ContentExecutionReadRequest,
) -> Result<ContentExecutionRef, AppError> {
    Ok(execution_ref(
        scoped_execution(state, scope, request.execution_id).await?,
    ))
}

fn cursor_digest(scope: &TenantScope, execution: &ContentExecution, offset: usize) -> String {
    sha256_hex(
        format!(
            "geo.content.items.v1|{}|{}|{}|{offset}",
            scope.storage_key(),
            execution.execution_id,
            execution.input_hash,
        )
        .as_bytes(),
    )
}

fn page_offset(
    scope: &TenantScope,
    execution: &ContentExecution,
    cursor: Option<&str>,
    count: usize,
) -> Result<usize, AppError> {
    let Some(cursor) = cursor else { return Ok(0) };
    let mut parts = cursor.split('.');
    let (Some("v1"), Some(position), Some(digest), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(AppError::invalid_request("invalid content cursor"));
    };
    let offset = position
        .parse::<usize>()
        .map_err(|_| AppError::invalid_request("invalid content cursor"))?;
    if offset >= count || digest != cursor_digest(scope, execution, offset) {
        return Err(AppError::invalid_request(
            "content cursor does not match the scoped execution",
        ));
    }
    Ok(offset)
}

pub(crate) async fn items(
    state: &AppState,
    scope: &TenantScope,
    request: ContentItemsReadRequest,
) -> Result<ContentItemsPage, AppError> {
    let execution = scoped_execution(state, scope, request.execution_id).await?;
    let limit = request.limit.unwrap_or(25);
    if !(1..=100).contains(&limit) {
        return Err(AppError::invalid_request("limit must be between 1 and 100"));
    }
    let mut items = state
        .content_service()
        .repository()
        .list_items(scope, execution.execution_id)
        .await?;
    items.sort_by(|a, b| {
        a.branch_key
            .cmp(&b.branch_key)
            .then_with(|| a.item_id.cmp(&b.item_id))
    });
    let offset = page_offset(scope, &execution, request.cursor.as_deref(), items.len())?;
    let end = offset.saturating_add(limit as usize).min(items.len());
    let next_cursor =
        (end < items.len()).then(|| format!("v1.{end}.{}", cursor_digest(scope, &execution, end)));
    let mut page_items = Vec::new();
    for item in &items[offset..end] {
        page_items.push(item_ref(state, scope, item.clone()).await?);
    }
    Ok(ContentItemsPage {
        execution_id: execution.execution_id,
        total: items.len() as u64,
        items: page_items,
        next_cursor,
    })
}

async fn scoped_item(
    state: &AppState,
    scope: &TenantScope,
    request: &ContentStepRequest,
) -> Result<(), AppError> {
    scoped_execution(state, scope, request.execution_id).await?;
    let item = state
        .content_service()
        .repository()
        .get_item(scope, request.execution_id, request.item_id)
        .await?
        .ok_or_else(|| AppError::not_found("content item not found"))?;
    if item.execution_id != request.execution_id {
        return Err(AppError::forbidden("content item is outside execution"));
    }
    Ok(())
}

pub(crate) async fn prepare(
    state: &AppState,
    scope: &TenantScope,
    request: ContentPrepareRequest,
) -> Result<ContentItemRef, AppError> {
    request.validate().map_err(AppError::invalid_request)?;
    scoped_item(
        state,
        scope,
        &ContentStepRequest {
            execution_id: request.execution_id,
            item_id: request.item_id,
        },
    )
    .await?;
    let item = state
        .content_service()
        .prepare(
            scope,
            request.execution_id,
            request.item_id,
            request.evidence,
        )
        .await?;
    item_ref(state, scope, item).await
}

pub(crate) async fn source_read(
    state: &AppState,
    scope: &TenantScope,
    request: ContentSourceReadRequest,
) -> Result<ContentSourcePage, AppError> {
    scoped_execution(state, scope, request.execution_id).await?;
    state.content_service().source_read(scope, request).await
}

pub(crate) async fn generate(
    state: &AppState,
    scope: &TenantScope,
    request: ContentStepRequest,
) -> Result<ContentItemRef, AppError> {
    scoped_item(state, scope, &request).await?;
    state
        .content_service()
        .generate(scope, request.execution_id, request.item_id)
        .await?;
    // The model transform returns a revision; the op must report the persisted
    // item transition, not infer a status from an in-memory response.
    let item = state
        .content_service()
        .repository()
        .get_item(scope, request.execution_id, request.item_id)
        .await?
        .ok_or_else(|| AppError::not_found("content item not found"))?;
    item_ref(state, scope, item).await
}

pub(crate) async fn check(
    state: &AppState,
    scope: &TenantScope,
    request: ContentStepRequest,
) -> Result<ContentItemRef, AppError> {
    scoped_item(state, scope, &request).await?;
    let item = state
        .content_service()
        .check(scope, request.execution_id, request.item_id)
        .await?;
    item_ref(state, scope, item).await
}

pub(crate) async fn repair(
    state: &AppState,
    scope: &TenantScope,
    request: ContentStepRequest,
) -> Result<ContentItemRef, AppError> {
    scoped_item(state, scope, &request).await?;
    state
        .content_service()
        .repair(scope, request.execution_id, request.item_id)
        .await?;
    // Report fresh persisted state; the model response is never an authority
    // for step status or the repair count exposed to the isolate.
    let item = state
        .content_service()
        .repository()
        .get_item(scope, request.execution_id, request.item_id)
        .await?
        .ok_or_else(|| AppError::not_found("content item not found"))?;
    item_ref(state, scope, item).await
}

pub(crate) async fn close(
    state: &AppState,
    scope: &TenantScope,
    request: ContentCloseRequest,
) -> Result<ContentHandoffRef, AppError> {
    scoped_execution(state, scope, request.execution_id).await?;
    let handoff = state
        .content_service()
        .close(scope, request.execution_id)
        .await?;
    Ok(ContentHandoffRef {
        execution_id: handoff.execution_id,
        handoff_id: handoff.handoff_id,
        total: handoff.coverage.total,
    })
}
