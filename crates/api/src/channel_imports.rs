//! Bounded operator onboarding. No browser interaction or session verification.
use crate::channels::{ChannelService, pool_tenant};
use crate::{ApiError, AppState, AuthContext, RequestContext, api_error};
use axum::{
    Json,
    extract::{Extension, Path, State, rejection::JsonRejection},
};
use chrono::Utc;
use geo_domain::{
    AppError, ChannelImportRecord, ChannelImportRow, ChannelImportStatus, ChannelStatus, ErrorCode,
    PoolAccount, TenantScope, supported_channel,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

// Deliberately no Debug. Serialize is internal, solely for AEAD input.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportInputRow {
    row_key: String,
    platform: String,
    username: String,
    password: String,
    display_alias: Option<String>,
    group_id: Option<Uuid>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportInput {
    import_key: String,
    rows: Vec<ImportInputRow>,
}

#[derive(Serialize)]
pub(crate) struct ImportResult {
    import_key: String,
    rows: Vec<ChannelImportRow>,
}

fn check_key(value: &str) -> Result<(), AppError> {
    if value.is_empty()
        || matches!(value, "." | "..")
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(AppError::invalid_request(
            "import and row keys must be 1 to 128 ASCII letters, digits, dots, hyphens or underscores",
        ));
    }
    Ok(())
}

fn canonical(row: &ImportInputRow) -> Result<Vec<u8>, AppError> {
    serde_json::to_vec(row)
        .map_err(|_| AppError::new(ErrorCode::Internal, "cannot encode import row"))
}

fn purpose(import_key: &str, row_key: &str) -> String {
    // Length-delimited keys prevent ambiguous AAD concatenation.
    format!("onboarding-v1:{}:{import_key}:{row_key}", import_key.len())
}

fn scope(service: &ChannelService, auth: &AuthContext) -> Result<TenantScope, AppError> {
    Ok(TenantScope::new(
        auth.operator.id,
        pool_tenant(service, auth)?,
        None,
    ))
}

impl ChannelService {
    async fn import_rows(
        &self,
        scope: &TenantScope,
        input: ImportInput,
    ) -> Result<ImportResult, AppError> {
        check_key(&input.import_key)?;
        if input.rows.is_empty() || input.rows.len() > 100 {
            return Err(AppError::invalid_request("import requires 1 to 100 rows"));
        }
        let mut keys = HashSet::new();
        for row in &input.rows {
            check_key(&row.row_key)?;
            if !keys.insert(&row.row_key) {
                return Err(AppError::invalid_request("duplicate import row key"));
            }
            if row.platform.len() > 64
                || row.username.len() > 512
                || row.password.len() > 4096
                || row.display_alias.as_ref().is_some_and(|v| v.len() > 160)
            {
                return Err(AppError::invalid_request("import field exceeds size limit"));
            }
        }
        let existing = self
            .repository
            .list_import_rows(scope.operator_id, &input.import_key)
            .await?;
        // Detect known conflicts before any writes. A concurrent new conflict
        // can still leave earlier independent rows committed; GET recovers them.
        for row in &input.rows {
            if let Some(stored) = existing.iter().find(|r| r.row.row_key == row.row_key) {
                self.compare_import(scope, &input.import_key, row, stored)?;
            }
        }
        if existing.len()
            + input
                .rows
                .iter()
                .filter(|r| !existing.iter().any(|old| old.row.row_key == r.row_key))
                .count()
            > 100
        {
            return Err(AppError::invalid_request("import batch exceeds 100 rows"));
        }
        for (ordinal, row) in input.rows.iter().enumerate() {
            if existing.iter().any(|r| r.row.row_key == row.row_key) {
                continue; // replay returns stored failure; retry is explicit
            }
            let id = Uuid::new_v4();
            let encrypted = self.encrypt(
                scope,
                id,
                &purpose(&input.import_key, &row.row_key),
                &canonical(row)?,
            )?;
            let record = ChannelImportRecord {
                secret_id: id,
                encrypted_input: encrypted,
                row: ChannelImportRow {
                    row_key: row.row_key.clone(),
                    ordinal: ordinal as u32,
                    account_id: None,
                    status: ChannelImportStatus::Failed,
                    error_code: None,
                    masked_username: Some(if row.username.chars().count() > 2 {
                        format!("{}***", row.username.chars().next().unwrap())
                    } else {
                        "***".into()
                    }),
                    display_alias: row.display_alias.clone(),
                },
            };
            let (record, account) = prepare(row, record);
            let stored = self
                .repository
                .save_import_row(scope.operator_id, &input.import_key, None, record, account)
                .await?;
            self.compare_import(scope, &input.import_key, row, &stored)?;
        }
        self.import_result(scope, input.import_key).await
    }

    fn compare_import(
        &self,
        scope: &TenantScope,
        key: &str,
        row: &ImportInputRow,
        stored: &ChannelImportRecord,
    ) -> Result<(), AppError> {
        let original = self.decrypt(
            scope,
            stored.secret_id,
            &purpose(key, &row.row_key),
            &stored.encrypted_input,
        )?;
        if original != canonical(row)? {
            return Err(AppError::conflict(
                "import row key already has different content",
            ));
        }
        Ok(())
    }

    async fn import_result(
        &self,
        scope: &TenantScope,
        import_key: String,
    ) -> Result<ImportResult, AppError> {
        check_key(&import_key)?;
        let rows = self
            .repository
            .list_import_rows(scope.operator_id, &import_key)
            .await?;
        if rows.is_empty() {
            return Err(AppError::not_found("import not found"));
        }
        Ok(ImportResult {
            import_key,
            rows: rows.into_iter().map(|r| r.row).collect(),
        })
    }

    async fn retry_import(
        &self,
        scope: &TenantScope,
        import_key: String,
        row_key: String,
    ) -> Result<ImportResult, AppError> {
        check_key(&import_key)?;
        check_key(&row_key)?;
        let record = self
            .repository
            .list_import_rows(scope.operator_id, &import_key)
            .await?
            .into_iter()
            .find(|r| r.row.row_key == row_key)
            .ok_or_else(|| AppError::not_found("import row not found"))?;
        if record.row.status == ChannelImportStatus::Failed {
            let plain = self.decrypt(
                scope,
                record.secret_id,
                &purpose(&import_key, &row_key),
                &record.encrypted_input,
            )?;
            let input: ImportInputRow = serde_json::from_slice(&plain)
                .map_err(|_| AppError::new(ErrorCode::Internal, "stored import input invalid"))?;
            let expected = record.encrypted_input.clone();
            let (record, account) = prepare(&input, record);
            self.repository
                .save_import_row(
                    scope.operator_id,
                    &import_key,
                    Some(expected),
                    record,
                    account,
                )
                .await?;
        }
        self.import_result(scope, import_key).await
    }
}

fn prepare(
    input: &ImportInputRow,
    mut record: ChannelImportRecord,
) -> (ChannelImportRecord, Option<PoolAccount>) {
    let error = if !supported_channel(&input.platform) {
        Some("unsupported_platform")
    } else if input.username.trim().is_empty() || input.password.is_empty() {
        Some("invalid_credentials")
    } else {
        None
    };
    record.row.error_code = error.map(str::to_owned);
    if error.is_some() {
        record.row.status = ChannelImportStatus::Failed;
        record.row.account_id = None;
        return (record, None);
    }
    record.row.status = ChannelImportStatus::NeedsLogin;
    record.row.account_id = Some(record.secret_id);
    let now = Utc::now();
    let account = PoolAccount {
        account_id: record.secret_id,
        platform: input.platform.clone(),
        group_id: input.group_id,
        status: ChannelStatus::NeedsLogin,
        display_name: input.display_alias.clone(),
        platform_account_id: None,
        avatar_url: None,
        enabled: true,
        proxy_configured: false,
        proxy_server: None,
        created_at: now,
        updated_at: now,
    };
    (record, Some(account))
}

pub(crate) async fn create(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    input: Result<Json<ImportInput>, JsonRejection>,
) -> Result<Json<ImportResult>, ApiError> {
    let service = state.channel_service();
    let scope = scope(service, &auth).map_err(|e| api_error(e, context.request_id))?;
    let Json(input) = input.map_err(|_| {
        api_error(
            AppError::invalid_request("invalid import request"),
            context.request_id,
        )
    })?;
    service
        .import_rows(&scope, input)
        .await
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}

pub(crate) async fn get(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Path(import_key): Path<String>,
) -> Result<Json<ImportResult>, ApiError> {
    let service = state.channel_service();
    let scope = scope(service, &auth).map_err(|e| api_error(e, context.request_id))?;
    service
        .import_result(&scope, import_key)
        .await
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}

pub(crate) async fn retry(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Path((import_key, row_key)): Path<(String, String)>,
) -> Result<Json<ImportResult>, ApiError> {
    let service = state.channel_service();
    let scope = scope(service, &auth).map_err(|e| api_error(e, context.request_id))?;
    service
        .retry_import(&scope, import_key, row_key)
        .await
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_domain::{OperatorId, PoolGroup, TenantId};

    fn test_scope() -> TenantScope {
        TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            None,
        )
    }

    fn input(key: &str, group_id: Option<Uuid>) -> ImportInput {
        ImportInput {
            import_key: key.into(),
            rows: vec![ImportInputRow {
                row_key: "row-1".into(),
                platform: "zhihu".into(),
                username: "synthetic-user".into(),
                password: "synthetic-password".into(),
                display_alias: Some("Test account".into()),
                group_id,
            }],
        }
    }

    #[tokio::test]
    async fn encrypted_import_replay_conflict_and_operator_isolation() {
        let service = ChannelService::development();
        let scope = test_scope();
        let first = service
            .import_rows(&scope, input("batch", None))
            .await
            .unwrap();
        let second = service
            .import_rows(&scope, input("batch", None))
            .await
            .unwrap();
        assert_eq!(first.rows[0].account_id, second.rows[0].account_id);
        let json = serde_json::to_string(&first).unwrap();
        assert!(!json.contains("synthetic-user"));
        assert!(!json.contains("synthetic-password"));
        let stored = service
            .repository
            .list_import_rows(scope.operator_id, "batch")
            .await
            .unwrap();
        let encrypted = stored[0].encrypted_input.encrypted_bytes();
        assert!(!encrypted.windows(18).any(|w| w == b"synthetic-password"));
        let account = service
            .repository
            .get_pool_account(scope.operator_id, first.rows[0].account_id.unwrap())
            .await
            .unwrap();
        assert!(account.session.is_none());
        assert_eq!(account.account.status, ChannelStatus::NeedsLogin);
        assert!(account.account.platform_account_id.is_none());
        assert!(service.browser.is_none());
        let mut changed = input("batch", None);
        changed.rows[0].password = "changed-synthetic-password".into();
        assert!(
            matches!(service.import_rows(&scope, changed).await, Err(e) if e.code == ErrorCode::Conflict)
        );
        assert_eq!(
            service
                .repository
                .list_pool_accounts(scope.operator_id)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            matches!(service.import_result(&test_scope(), "batch".into()).await, Err(e) if e.code == ErrorCode::NotFound)
        );
        assert!(
            service
                .decrypt(
                    &test_scope(),
                    stored[0].secret_id,
                    &purpose("batch", "row-1"),
                    &stored[0].encrypted_input
                )
                .is_err()
        );
    }

    #[tokio::test]
    async fn failed_replay_is_stable_and_explicit_concurrent_retry_creates_once() {
        let service = ChannelService::development();
        let scope = test_scope();
        let group_id = Uuid::new_v4();
        let failed = service
            .import_rows(&scope, input("batch", Some(group_id)))
            .await
            .unwrap();
        assert_eq!(
            failed.rows[0].error_code.as_deref(),
            Some("group_not_found")
        );
        assert!(failed.rows[0].account_id.is_none());
        service
            .repository
            .save_pool_group(
                scope.operator_id,
                PoolGroup {
                    group_id,
                    name: "Synthetic group".into(),
                    created_at: Utc::now(),
                },
            )
            .await
            .unwrap();
        let replay = service
            .import_rows(&scope, input("batch", Some(group_id)))
            .await
            .unwrap();
        assert_eq!(replay.rows[0].status, ChannelImportStatus::Failed);
        let (a, b) = tokio::join!(
            service.retry_import(&scope, "batch".into(), "row-1".into()),
            service.retry_import(&scope, "batch".into(), "row-1".into()),
        );
        assert_eq!(a.unwrap().rows[0].account_id, b.unwrap().rows[0].account_id);
        assert_eq!(
            service
                .repository
                .list_pool_accounts(scope.operator_id)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn concurrent_create_same_or_changed_content_never_creates_extra_accounts() {
        let service = ChannelService::development();
        let scope = test_scope();
        let (a, b) = tokio::join!(
            service.import_rows(&scope, input("same", None)),
            service.import_rows(&scope, input("same", None)),
        );
        assert_eq!(a.unwrap().rows[0].account_id, b.unwrap().rows[0].account_id);
        let mut different = input("changed", None);
        different.rows[0].username = "other-synthetic-user".into();
        let (a, b) = tokio::join!(
            service.import_rows(&scope, input("changed", None)),
            service.import_rows(&scope, different),
        );
        assert_ne!(a.is_ok(), b.is_ok());
        assert_eq!(
            service
                .repository
                .list_pool_accounts(scope.operator_id)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn invalid_keys_and_duplicate_rows_fail_before_writes_platform_failure_is_durable() {
        let service = ChannelService::development();
        let scope = test_scope();
        let mut duplicate = input("duplicate", None);
        duplicate.rows.push(input("unused", None).rows.remove(0));
        assert!(service.import_rows(&scope, duplicate).await.is_err());
        assert!(
            service
                .repository
                .list_import_rows(scope.operator_id, "duplicate")
                .await
                .unwrap()
                .is_empty()
        );
        let mut invalid = input("invalid", None);
        invalid.rows[0].row_key.clear();
        assert!(service.import_rows(&scope, invalid).await.is_err());
        let mut oversized = input("oversized", None);
        oversized.rows[0].password = "x".repeat(4097);
        assert!(service.import_rows(&scope, oversized).await.is_err());
        let mut unknown = input("unknown", None);
        unknown.rows[0].platform = "unsupported".into();
        let outcome = service.import_rows(&scope, unknown).await.unwrap();
        assert_eq!(
            outcome.rows[0].error_code.as_deref(),
            Some("unsupported_platform")
        );
        assert!(
            service
                .repository
                .list_pool_accounts(scope.operator_id)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
