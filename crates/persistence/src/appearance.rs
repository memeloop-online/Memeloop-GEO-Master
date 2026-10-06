//! Durable, operator-keyed appearance with atomic optimistic revision checks.

use geo_domain::{AppError, ErrorCode, OperatorAppearance, OperatorId, UpdateOperatorAppearance};
use sqlx::{PgPool, Row};

pub(crate) async fn get(
    pool: &PgPool,
    operator_id: OperatorId,
) -> Result<Option<OperatorAppearance>, AppError> {
    let row = sqlx::query(
        "SELECT display_name, primary_color, default_locale, appearance_revision \
         FROM operators WHERE operator_id = $1",
    )
    .bind(operator_id.as_uuid())
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?;
    row.map(|row| {
        Ok(OperatorAppearance {
            display_name: row.try_get("display_name").map_err(storage_error)?,
            logo_url: None,
            primary_color: row.try_get("primary_color").map_err(storage_error)?,
            default_locale: row.try_get("default_locale").map_err(storage_error)?,
            revision: row.try_get("appearance_revision").map_err(storage_error)?,
        })
    })
    .transpose()
}

pub(crate) async fn update(
    pool: &PgPool,
    operator_id: OperatorId,
    expected_revision: i64,
    input: UpdateOperatorAppearance,
) -> Result<OperatorAppearance, AppError> {
    let input = input.validate()?;
    if expected_revision < 1 || expected_revision == i64::MAX {
        return Err(AppError::invalid_request("invalid appearance revision"));
    }
    let row = sqlx::query(
        "UPDATE operators \
         SET display_name = $1, primary_color = $2, default_locale = $3, \
             appearance_revision = appearance_revision + 1, updated_at = now() \
         WHERE operator_id = $4 AND appearance_revision = $5 \
         RETURNING display_name, primary_color, default_locale, appearance_revision",
    )
    .bind(input.display_name)
    .bind(input.primary_color)
    .bind(input.default_locale)
    .bind(operator_id.as_uuid())
    .bind(expected_revision)
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?;
    let Some(row) = row else {
        return if get(pool, operator_id).await?.is_some() {
            Err(AppError::conflict("operator appearance revision changed"))
        } else {
            Err(AppError::not_found("operator is not configured"))
        };
    };
    Ok(OperatorAppearance {
        display_name: row.try_get("display_name").map_err(storage_error)?,
        logo_url: None,
        primary_color: row.try_get("primary_color").map_err(storage_error)?,
        default_locale: row.try_get("default_locale").map_err(storage_error)?,
        revision: row.try_get("appearance_revision").map_err(storage_error)?,
    })
}

fn storage_error(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        format!("operator appearance persistence is unavailable: {error}"),
    )
}
