//! Secret-write-only operator proxy inventory. Does not assign or probe routes.
use crate::channels::{ChannelService, ProxyInput, trusted_pool_scope};
use crate::{ApiError, AppState, AuthContext, RequestContext, api_error};
use axum::{
    Json,
    extract::{
        Extension, Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
};
use chrono::Utc;
use geo_domain::{AppError, ChannelSecret, ErrorCode, ProxyPool, ProxyPoolMember, TenantScope};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PageInput {
    limit: Option<u32>,
    cursor: Option<Uuid>,
}
#[derive(Serialize)]
pub(crate) struct Page<T> {
    items: Vec<T>,
    next_cursor: Option<Uuid>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NewPool {
    name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PoolPatch {
    expected_version: i64,
    name: Option<String>,
    enabled: Option<bool>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NewMember {
    name: String,
    proxy: ProxyInput,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemberPatch {
    expected_version: i64,
    name: Option<String>,
    enabled: Option<bool>,
    proxy: Option<ProxyInput>,
}

fn name(value: String) -> Result<String, AppError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || value.len() > 160 || value.chars().any(char::is_control) {
        return Err(AppError::invalid_request(
            "name must be 1 to 160 bytes without control characters",
        ));
    }
    Ok(trimmed.to_owned())
}
fn next_version(expected: i64) -> Result<i64, AppError> {
    if expected < 1 {
        return Err(AppError::invalid_request(
            "expected_version must be positive",
        ));
    }
    expected
        .checked_add(1)
        .ok_or_else(|| AppError::conflict("inventory version exhausted"))
}
fn pagination(
    input: Result<Query<PageInput>, QueryRejection>,
) -> Result<(u32, Option<Uuid>), AppError> {
    let Query(input) =
        input.map_err(|_| AppError::invalid_request("invalid inventory pagination"))?;
    let limit = input.limit.unwrap_or(25);
    if !(1..=100).contains(&limit) {
        return Err(AppError::invalid_request("limit must be 1 to 100"));
    }
    Ok((limit, input.cursor))
}
fn page<T>(mut items: Vec<T>, limit: u32, id: impl Fn(&T) -> Uuid) -> Page<T> {
    let more = items.len() > limit as usize;
    items.truncate(limit as usize);
    let next_cursor = if more { items.last().map(id) } else { None };
    Page { items, next_cursor }
}
fn body<T>(input: Result<Json<T>, JsonRejection>) -> Result<T, AppError> {
    input
        .map(|Json(v)| v)
        .map_err(|_| AppError::invalid_request("invalid proxy inventory request"))
}
fn encrypt_proxy(
    service: &ChannelService,
    scope: &TenantScope,
    pool: Uuid,
    member: Uuid,
    revision: Uuid,
    proxy: ProxyInput,
) -> Result<(String, ChannelSecret), AppError> {
    if proxy.server.trim() != proxy.server || proxy.server.chars().any(char::is_control) {
        return Err(AppError::invalid_request("invalid proxy server"));
    }
    let url = reqwest::Url::parse(&proxy.server)
        .map_err(|_| AppError::invalid_request("invalid proxy server"))?;
    // URL parsing removes explicit standard ports (HTTP :80 / HTTPS :443).
    // Validate the effective port here, without changing legacy account APIs.
    if !matches!(url.scheme(), "http" | "https" | "socks5")
        || url.host_str().is_none()
        || url.port_or_known_default().is_none_or(|port| port == 0)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || proxy.server.len() > 512
        || url.path() != "/" && !url.path().is_empty()
        || proxy
            .username
            .as_ref()
            .is_some_and(|v| v.len() > 512 || v.chars().any(char::is_control))
        || proxy
            .password
            .as_ref()
            .is_some_and(|v| v.len() > 4096 || v.chars().any(char::is_control))
    {
        return Err(AppError::invalid_request(
            "invalid proxy endpoint or credential length",
        ));
    }
    let server = url.as_str().trim_end_matches('/').to_owned();
    let bytes = serde_json::to_vec(&proxy)
        .map_err(|_| AppError::new(ErrorCode::Internal, "proxy encoding failed"))?;
    let secret = service.encrypt(
        scope,
        member,
        &format!("proxy-pool-v1:{pool}:{revision}"),
        &bytes,
    )?;
    Ok((server, secret))
}

pub(crate) async fn list_pools(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    input: Result<Query<PageInput>, QueryRejection>,
) -> Result<Json<Page<ProxyPool>>, ApiError> {
    let result = async {
        let service = state.channel_service();
        let scope = trusted_pool_scope(service, &auth)?;
        let (limit, cursor) = pagination(input)?;
        let items = service
            .repository
            .list_proxy_pools(scope.operator_id, cursor, limit + 1)
            .await?;
        Ok(page(items, limit, |v| v.pool_id))
    }
    .await;
    result
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}
pub(crate) async fn create_pool(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    input: Result<Json<NewPool>, JsonRejection>,
) -> Result<Json<ProxyPool>, ApiError> {
    let result = async {
        let service = state.channel_service();
        let scope = trusted_pool_scope(service, &auth)?;
        let input = body(input)?;
        let now = Utc::now();
        service
            .repository
            .save_proxy_pool(
                scope.operator_id,
                None,
                ProxyPool {
                    pool_id: Uuid::new_v4(),
                    name: name(input.name)?,
                    enabled: true,
                    version: 1,
                    created_at: now,
                    updated_at: now,
                },
            )
            .await
    }
    .await;
    result
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}
pub(crate) async fn patch_pool(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Path(id): Path<Uuid>,
    input: Result<Json<PoolPatch>, JsonRejection>,
) -> Result<Json<ProxyPool>, ApiError> {
    let result = async {
        let service = state.channel_service();
        let scope = trusted_pool_scope(service, &auth)?;
        let input = body(input)?;
        let version = next_version(input.expected_version)?;
        if input.name.is_none() && input.enabled.is_none() {
            return Err(AppError::invalid_request("patch requires a change"));
        }
        let mut pool = service
            .repository
            .get_proxy_pool(scope.operator_id, id)
            .await?;
        if let Some(value) = input.name {
            pool.name = name(value)?;
        }
        if let Some(value) = input.enabled {
            pool.enabled = value;
        }
        pool.version = version;
        pool.updated_at = Utc::now();
        service
            .repository
            .save_proxy_pool(scope.operator_id, Some(input.expected_version), pool)
            .await
    }
    .await;
    result
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}
pub(crate) async fn list_members(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Path(pool): Path<Uuid>,
    input: Result<Query<PageInput>, QueryRejection>,
) -> Result<Json<Page<ProxyPoolMember>>, ApiError> {
    let result = async {
        let service = state.channel_service();
        let scope = trusted_pool_scope(service, &auth)?;
        let (limit, cursor) = pagination(input)?;
        let items = service
            .repository
            .list_proxy_members(scope.operator_id, pool, cursor, limit + 1)
            .await?;
        Ok(page(items, limit, |v| v.member_id))
    }
    .await;
    result
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}
pub(crate) async fn create_member(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Path(pool): Path<Uuid>,
    input: Result<Json<NewMember>, JsonRejection>,
) -> Result<Json<ProxyPoolMember>, ApiError> {
    let result = async {
        let service = state.channel_service();
        let scope = trusted_pool_scope(service, &auth)?;
        let input = body(input)?;
        let name = name(input.name)?;
        service
            .repository
            .get_proxy_pool(scope.operator_id, pool)
            .await?;
        let member = Uuid::new_v4();
        let revision = Uuid::new_v4();
        let (server, secret) = encrypt_proxy(service, &scope, pool, member, revision, input.proxy)?;
        let now = Utc::now();
        service
            .repository
            .save_proxy_member(
                scope.operator_id,
                None,
                ProxyPoolMember {
                    member_id: member,
                    pool_id: pool,
                    name,
                    enabled: true,
                    version: 1,
                    revision_id: revision,
                    proxy_server: server,
                    created_at: now,
                    updated_at: now,
                },
                Some(secret),
            )
            .await
    }
    .await;
    result
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}
pub(crate) async fn patch_member(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Path((pool, id)): Path<(Uuid, Uuid)>,
    input: Result<Json<MemberPatch>, JsonRejection>,
) -> Result<Json<ProxyPoolMember>, ApiError> {
    let result = async {
        let service = state.channel_service();
        let scope = trusted_pool_scope(service, &auth)?;
        let input = body(input)?;
        let version = next_version(input.expected_version)?;
        if input.name.is_none() && input.enabled.is_none() && input.proxy.is_none() {
            return Err(AppError::invalid_request("patch requires a change"));
        }
        let mut member = service
            .repository
            .get_proxy_member(scope.operator_id, pool, id)
            .await?;
        if let Some(value) = input.name {
            member.name = name(value)?;
        }
        if let Some(value) = input.enabled {
            member.enabled = value;
        }
        let revision = if let Some(proxy) = input.proxy {
            member.revision_id = Uuid::new_v4();
            let (server, secret) =
                encrypt_proxy(service, &scope, pool, id, member.revision_id, proxy)?;
            member.proxy_server = server;
            Some(secret)
        } else {
            None
        };
        member.version = version;
        member.updated_at = Utc::now();
        service
            .repository
            .save_proxy_member(
                scope.operator_id,
                Some(input.expected_version),
                member,
                revision,
            )
            .await
    }
    .await;
    result
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inventory_accepts_standard_effective_ports_without_relaxing_secret_redaction() {
        let service = ChannelService::development();
        let scope = TenantScope::new(Uuid::new_v4().into(), Uuid::new_v4().into(), None);
        for (input, expected) in [
            ("http://fixture.invalid:80", "http://fixture.invalid"),
            ("https://fixture.invalid:443", "https://fixture.invalid"),
            ("http://fixture.invalid", "http://fixture.invalid"),
            ("https://fixture.invalid", "https://fixture.invalid"),
            (
                "socks5://fixture.invalid:1080",
                "socks5://fixture.invalid:1080",
            ),
        ] {
            let proxy = ProxyInput {
                server: input.into(),
                username: Some("synthetic-user".into()),
                password: Some("synthetic-secret".into()),
            };
            let (server, _) = encrypt_proxy(
                &service,
                &scope,
                Uuid::new_v4(),
                Uuid::new_v4(),
                Uuid::new_v4(),
                proxy,
            )
            .unwrap();
            assert_eq!(server, expected);
        }
        for input in [
            "socks5://fixture.invalid",
            "http://fixture.invalid:0",
            "https://fixture.invalid:0",
            "socks5://fixture.invalid:0",
            "http://user:secret@fixture.invalid:80",
            "https://fixture.invalid:443/private",
            "https://fixture.invalid:443?secret=value",
            "https://fixture.invalid:443#secret",
            "ftp://fixture.invalid:21",
        ] {
            let proxy = ProxyInput {
                server: input.into(),
                username: None,
                password: None,
            };
            assert!(
                encrypt_proxy(
                    &service,
                    &scope,
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                    proxy
                )
                .is_err()
            );
        }
    }
    #[test]
    fn proxy_credentials_are_encrypted_with_revision_scoped_aad() {
        let service = ChannelService::development();
        let scope = TenantScope::new(Uuid::new_v4().into(), Uuid::new_v4().into(), None);
        let pool = Uuid::new_v4();
        let member = Uuid::new_v4();
        let revision = Uuid::new_v4();
        let proxy = ProxyInput {
            server: "http://fixture.invalid:3128/".into(),
            username: Some("synthetic-user".into()),
            password: Some("synthetic-secret".into()),
        };
        let (server, secret) =
            encrypt_proxy(&service, &scope, pool, member, revision, proxy).unwrap();
        assert_eq!(server, "http://fixture.invalid:3128");
        assert!(!String::from_utf8_lossy(secret.encrypted_bytes()).contains("synthetic-secret"));
        let purpose = format!("proxy-pool-v1:{pool}:{revision}");
        let decoded = service.decrypt(&scope, member, &purpose, &secret).unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(decoded["password"], "synthetic-secret");
        assert!(
            service
                .decrypt(
                    &scope,
                    member,
                    &format!("proxy-pool-v1:{pool}:{}", Uuid::new_v4()),
                    &secret
                )
                .is_err()
        );
        assert!(
            service
                .decrypt(&scope, Uuid::new_v4(), &purpose, &secret)
                .is_err()
        );
    }
    #[test]
    fn validation_and_page_boundaries_are_strict() {
        assert!(name(" ".into()).is_err());
        assert!(name("a\nb".into()).is_err());
        assert!(name("x".repeat(161)).is_err());
        assert!(next_version(0).is_err());
        assert!(next_version(i64::MAX).is_err());
        assert!(serde_json::from_str::<NewMember>(r#"{"name":"a","proxy":{"server":"http://fixture.invalid:3128","unexpected":"secret"}}"#).is_err());
        let first = Uuid::from_u128(1);
        let second = Uuid::from_u128(2);
        let result = page(vec![first, second], 1, |id| *id);
        assert_eq!(result.items, vec![first]);
        assert_eq!(result.next_cursor, Some(first));
    }
}
