//! Independent service-key HTTP boundary. Never derives scope from user sessions.
use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Path, Query, Request, State, rejection::PathRejection},
    http::{HeaderMap, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use geo_domain::{
    AppError, ErrorCode, PARTNER_SYNC_MAX_REQUEST_BYTES, PartnerPrincipal, PartnerRepository,
    PartnerSyncRequest, validate_partner_sync_request,
};
use serde::Deserialize;
use tower::{
    ServiceBuilder, ServiceExt, limit::ConcurrencyLimit, load_shed::LoadShed, util::BoxCloneService,
};
use uuid::Uuid;

use crate::{RequestContext, api_error};

/// Merge outside first-party cookie/session/CSRF layers. The repository is the
/// authority for connection scope and must recheck grants in accept/results.
pub fn partner_router<S>(
    repository: Arc<dyn PartnerRepository>,
    auth_capacity: NonZeroUsize,
) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/api/v1/partner/connections/{id}/knowledge-syncs",
            post(accept),
        )
        .route(
            "/api/v1/partner/connections/{id}/knowledge-syncs/{operation_id}",
            get(results),
        )
        .route(
            "/api/v1/partner/connections/{id}/content-exports/{revision_id}",
            get(content_export),
        )
        .with_state(PartnerState::new(repository, auth_capacity))
        .layer(middleware::from_fn(crate::context::no_store_middleware))
        .layer(middleware::from_fn(
            crate::context::request_context_middleware,
        ))
}

struct AuthenticationInput {
    connection_id: Uuid,
    key_id: Uuid,
    secret: String,
}

type AuthenticationService =
    LoadShed<ConcurrencyLimit<BoxCloneService<AuthenticationInput, PartnerPrincipal, AppError>>>;

#[derive(Clone)]
struct PartnerState {
    repository: Arc<dyn PartnerRepository>,
    // Only cloning uses this lock, never verification or a database/network await.
    authentication: Arc<Mutex<AuthenticationService>>,
}

impl PartnerState {
    fn new(repository: Arc<dyn PartnerRepository>, auth_capacity: NonZeroUsize) -> Self {
        let verifier = repository.clone();
        let service = tower::service_fn(move |input: AuthenticationInput| {
            let verifier = verifier.clone();
            async move {
                verifier
                    .authenticate(input.connection_id, input.key_id, &input.secret)
                    .await
            }
        });
        let authentication = ServiceBuilder::new()
            .load_shed()
            .concurrency_limit(auth_capacity.get())
            .service(BoxCloneService::new(service));
        Self {
            repository,
            authentication: Arc::new(Mutex::new(authentication)),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultsQuery {
    after_index: Option<u32>,
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_limit() -> u32 {
    50
}

fn request_id(request: &Request) -> Uuid {
    request
        .extensions()
        .get::<RequestContext>()
        .map(|context| context.request_id)
        .unwrap_or_else(Uuid::new_v4)
}

/// Never return repository diagnostics, which may contain private input or SQL.
fn failure(error: AppError, request_id: Uuid) -> Response {
    let message = match error.code {
        ErrorCode::InvalidRequest => "invalid partner request",
        ErrorCode::Unauthorized => "service authentication required",
        ErrorCode::Forbidden => "service permission denied",
        ErrorCode::NotFound => "partner resource not found",
        ErrorCode::Conflict => "partner resource state conflicts with this request",
        ErrorCode::NotReady => "partner resource is not ready",
        ErrorCode::CapabilityMissing => "partner capability unavailable",
        ErrorCode::DependencyUnavailable => "partner service unavailable",
        ErrorCode::Internal => "partner request failed",
    };
    let mut response = api_error(AppError::new(error.code, message), request_id).into_response();
    if error.code == ErrorCode::Unauthorized {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            header::HeaderValue::from_static("Bearer"),
        );
    }
    response
}

fn invalid() -> AppError {
    AppError::invalid_request("invalid knowledge sync request")
}

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let first = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    Some(first)
}

async fn authenticate(
    state: &PartnerState,
    headers: &HeaderMap,
    connection_id: Uuid,
) -> Result<PartnerPrincipal, AppError> {
    let unauthorized = || AppError::unauthorized("service authentication required");
    let value = single_header(headers, header::AUTHORIZATION.as_str()).ok_or_else(unauthorized)?;
    let (scheme, token) = value.split_once(' ').ok_or_else(unauthorized)?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return Err(unauthorized());
    }
    let (key, secret) = token.split_once('.').ok_or_else(unauthorized)?;
    let key_id = Uuid::parse_str(key).map_err(|_| unauthorized())?;
    // Bound verifier work and reject ambiguous tokens before repository access.
    if secret.len() != 64 || !secret.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(unauthorized());
    }
    let service = state
        .authentication
        .lock()
        .map_err(|_| AppError::not_ready("service authentication unavailable"))?
        .clone();
    // Tower's shared concurrency permit covers authentication only. It is dropped
    // before reading a body, accepting work or reading a result page.
    let principal = service
        .oneshot(AuthenticationInput {
            connection_id,
            key_id,
            secret: secret.to_owned(),
        })
        .await
        .map_err(|error| match error.downcast::<AppError>() {
            Ok(error) => *error,
            Err(_) => AppError::not_ready("service authentication unavailable"),
        })?;
    if principal.connection_id != connection_id || principal.key_id != key_id {
        return Err(unauthorized());
    }
    Ok(principal)
}

fn idempotency_key(headers: &HeaderMap) -> Result<&str, AppError> {
    let value = single_header(headers, crate::IDEMPOTENCY_KEY_HEADER).ok_or_else(invalid)?;
    if value.is_empty() || value.len() > 256 || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(invalid());
    }
    Ok(value)
}

async fn accept(
    State(state): State<PartnerState>,
    path: Result<Path<Uuid>, PathRejection>,
    request: Request,
) -> Response {
    let request_id = request_id(&request);
    let Ok(Path(connection_id)) = path else {
        return failure(invalid(), request_id);
    };
    let principal = match authenticate(&state, request.headers(), connection_id).await {
        Ok(principal) => principal,
        Err(error) => return failure(error, request_id),
    };
    let key = match idempotency_key(request.headers()) {
        Ok(key) => key.to_owned(),
        Err(error) => return failure(error, request_id),
    };
    if request.uri().query().is_some() {
        return failure(invalid(), request_id);
    }
    let json_media_type = single_header(request.headers(), header::CONTENT_TYPE.as_str())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"));
    if !json_media_type || request.headers().contains_key(header::CONTENT_ENCODING) {
        return failure(invalid(), request_id);
    }
    let bytes = match to_bytes(request.into_body(), PARTNER_SYNC_MAX_REQUEST_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => return failure(invalid(), request_id),
    };
    let input: PartnerSyncRequest = match serde_json::from_slice(&bytes) {
        Ok(input) => input,
        Err(_) => return failure(invalid(), request_id),
    };
    if let Err(error) = validate_partner_sync_request(&input) {
        return failure(error, request_id);
    }
    // The repository owns canonicalization, deduplication and the durable commit.
    match state.repository.accept(&principal, &key, input).await {
        Ok(receipt) => (StatusCode::ACCEPTED, Json(receipt)).into_response(),
        Err(error) => failure(error, request_id),
    }
}

async fn results(
    State(state): State<PartnerState>,
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
    request: Request,
) -> Response {
    let request_id = request_id(&request);
    let Ok(Path((connection_id, operation_id))) = path else {
        return failure(invalid(), request_id);
    };
    let principal = match authenticate(&state, request.headers(), connection_id).await {
        Ok(principal) => principal,
        Err(error) => return failure(error, request_id),
    };
    let query = match Query::<ResultsQuery>::try_from_uri(request.uri()) {
        Ok(Query(query)) if (1..=100).contains(&query.limit) => query,
        _ => return failure(invalid(), request_id),
    };
    match state
        .repository
        .results(&principal, operation_id, query.after_index, query.limit)
        .await
    {
        Ok(page) => Json(page).into_response(),
        Err(error) => failure(error, request_id),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentExportQuery {
    #[serde(default = "default_export_format")]
    format: String,
}

fn default_export_format() -> String {
    "markdown".into()
}

async fn content_export(
    State(state): State<PartnerState>,
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
    request: Request,
) -> Response {
    use crate::content::content_export::{ExportManifestContext, bundle_bytes};

    let request_id = request_id(&request);
    let Ok(Path((connection_id, revision_id))) = path else {
        return failure(invalid(), request_id);
    };
    let principal = match authenticate(&state, request.headers(), connection_id).await {
        Ok(principal) => principal,
        Err(error) => return failure(error, request_id),
    };
    let format = match Query::<ContentExportQuery>::try_from_uri(request.uri()) {
        Ok(Query(query)) if matches!(query.format.as_str(), "markdown" | "html") => query.format,
        _ => return failure(invalid(), request_id),
    };
    let snapshot = match state
        .repository
        .content_export(&principal, revision_id)
        .await
    {
        Ok(snapshot) => snapshot,
        Err(error) => return failure(error, request_id),
    };
    // A repository result can never turn an exact-version request into latest.
    if snapshot.revision.revision_id != revision_id {
        return failure(AppError::conflict("content revision changed"), request_id);
    }
    let archive = match tokio::task::spawn_blocking(move || {
        bundle_bytes(
            snapshot.revision,
            &format,
            snapshot.media_keys,
            snapshot.media,
            Some(ExportManifestContext {
                language: snapshot.language,
            }),
        )
    })
    .await
    {
        Ok(Ok(archive)) => archive,
        Ok(Err(error)) => return failure(error, request_id),
        Err(_) => {
            return failure(
                AppError::new(ErrorCode::Internal, "content export failed"),
                request_id,
            );
        }
    };
    (
        [
            (header::CONTENT_TYPE, "application/zip".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{revision_id}.zip\""),
            ),
            (
                header::HeaderName::from_static("x-content-type-options"),
                "nosniff".to_owned(),
            ),
        ],
        archive,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::{body::Body, http::Request as HttpRequest};
    use geo_domain::{
        PartnerAdvance, PartnerSyncPage, PartnerSyncReceipt, PartnerSyncStatus, PartnerSyncWork,
        TenantScope,
    };
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use tower::ServiceExt;

    const SECRET: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    struct Fake {
        principal: PartnerPrincipal,
        operation_id: Uuid,
        revoked: AtomicBool,
        calls: Mutex<Vec<String>>,
        fail_accept: AtomicBool,
        hold_authentication: AtomicBool,
        export_granted: AtomicBool,
        authentication_entered: tokio::sync::Notify,
        authentication_release: tokio::sync::Notify,
        hold_acceptance: AtomicBool,
        acceptance_entered: tokio::sync::Notify,
        acceptance_release: tokio::sync::Notify,
    }

    impl Fake {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                principal: PartnerPrincipal {
                    scope: TenantScope::new(
                        Uuid::new_v4().into(),
                        Uuid::new_v4().into(),
                        Some(Uuid::new_v4().into()),
                    ),
                    connection_id: Uuid::new_v4(),
                    key_id: Uuid::new_v4(),
                },
                operation_id: Uuid::new_v4(),
                revoked: AtomicBool::new(false),
                calls: Mutex::new(vec![]),
                fail_accept: AtomicBool::new(false),
                hold_authentication: AtomicBool::new(false),
                export_granted: AtomicBool::new(false),
                authentication_entered: tokio::sync::Notify::new(),
                authentication_release: tokio::sync::Notify::new(),
                hold_acceptance: AtomicBool::new(false),
                acceptance_entered: tokio::sync::Notify::new(),
                acceptance_release: tokio::sync::Notify::new(),
            })
        }

        fn uri(&self) -> String {
            format!(
                "/api/v1/partner/connections/{}/knowledge-syncs",
                self.principal.connection_id
            )
        }

        fn receipt(&self) -> PartnerSyncReceipt {
            PartnerSyncReceipt {
                operation_id: self.operation_id,
                status: PartnerSyncStatus::Pending,
                item_count: 1,
                completed_count: 0,
            }
        }

        fn check(&self, principal: &PartnerPrincipal) -> Result<(), AppError> {
            assert_eq!(principal.scope, self.principal.scope);
            assert_eq!(principal.connection_id, self.principal.connection_id);
            if self.revoked.load(Ordering::SeqCst) {
                return Err(AppError::forbidden(SECRET));
            }
            Ok(())
        }

        fn request(&self, method: &str, uri: &str, body: String) -> HttpRequest<Body> {
            HttpRequest::builder()
                .method(method)
                .uri(uri)
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}.{SECRET}", self.principal.key_id),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .header(crate::IDEMPOTENCY_KEY_HEADER, "synthetic-batch")
                .body(Body::from(body))
                .unwrap()
        }
    }

    #[async_trait]
    impl PartnerRepository for Fake {
        async fn content_export(
            &self,
            principal: &PartnerPrincipal,
            revision_id: Uuid,
        ) -> Result<geo_domain::PartnerContentExport, AppError> {
            self.check(principal)?;
            self.calls.lock().unwrap().push("content_export".into());
            if !self.export_granted.load(Ordering::SeqCst) {
                return Err(AppError::forbidden(SECRET));
            }
            if revision_id != self.operation_id {
                return Err(AppError::not_found(SECRET));
            }
            Ok(geo_domain::PartnerContentExport {
                revision: geo_domain::ContentRevision {
                    revision_id,
                    asset_id: Uuid::new_v4(),
                    revision: 1,
                    base_revision_id: None,
                    derived_from_revision_id: None,
                    document: geo_domain::StructuredDocument {
                        schema_version: 2,
                        title: "Saved title".into(),
                        blocks: vec![geo_domain::ContentBlock {
                            block_id: Uuid::new_v4(),
                            kind: geo_domain::ContentBlockKind::Paragraph,
                            text: "Saved content".into(),
                            citation_ids: vec![],
                            items: vec![],
                            rich: None,
                        }],
                    },
                    markdown: "Saved exact content.\r\n".into(),
                    evidence: vec![],
                    quotes: vec![],
                    findings: vec![],
                    created_at: chrono::Utc::now(),
                },
                language: Some("en".into()),
                media_keys: vec![],
                media: vec![],
            })
        }

        async fn authenticate(
            &self,
            connection_id: Uuid,
            key_id: Uuid,
            secret: &str,
        ) -> Result<PartnerPrincipal, AppError> {
            self.calls.lock().unwrap().push("authenticate".into());
            if self.hold_authentication.load(Ordering::SeqCst) {
                self.authentication_entered.notify_one();
                self.authentication_release.notified().await;
            }
            if connection_id != self.principal.connection_id
                || key_id != self.principal.key_id
                || secret != SECRET
            {
                return Err(AppError::unauthorized(SECRET));
            }
            self.check(&self.principal)?;
            Ok(self.principal.clone())
        }

        async fn accept(
            &self,
            principal: &PartnerPrincipal,
            idempotency_key: &str,
            _request: PartnerSyncRequest,
        ) -> Result<PartnerSyncReceipt, AppError> {
            self.check(principal)?;
            self.calls
                .lock()
                .unwrap()
                .push(format!("accept:{idempotency_key}"));
            if self.hold_acceptance.load(Ordering::SeqCst) {
                self.acceptance_entered.notify_one();
                self.acceptance_release.notified().await;
            }
            if self.fail_accept.load(Ordering::SeqCst) {
                return Err(AppError::new(ErrorCode::DependencyUnavailable, SECRET));
            }
            Ok(self.receipt())
        }

        async fn results(
            &self,
            principal: &PartnerPrincipal,
            operation_id: Uuid,
            after_index: Option<u32>,
            limit: u32,
        ) -> Result<PartnerSyncPage, AppError> {
            self.check(principal)?;
            self.calls
                .lock()
                .unwrap()
                .push(format!("results:{after_index:?}:{limit}"));
            if operation_id != self.operation_id {
                return Err(AppError::not_found(SECRET));
            }
            Ok(PartnerSyncPage {
                operation: self.receipt(),
                results: vec![],
                next_index: None,
            })
        }

        async fn candidates(&self, _limit: u32) -> Result<Vec<PartnerSyncWork>, AppError> {
            unreachable!()
        }
        async fn advance_one(&self, _work: &PartnerSyncWork) -> Result<PartnerAdvance, AppError> {
            unreachable!()
        }
    }

    fn body() -> String {
        serde_json::json!({
            "schema_version": 1,
            "items": [{
                "external_source_id": "synthetic-source",
                "external_version": "version-one",
                "source_sequence": 1,
                "effective_at": "2026-01-01T00:00:00Z",
                "action": {"operation": "revoke"}
            }]
        })
        .to_string()
    }

    async fn send(fake: Arc<Fake>, request: HttpRequest<Body>) -> (StatusCode, serde_json::Value) {
        let response = partner_router::<()>(fake, NonZeroUsize::new(1).unwrap())
            .oneshot(request)
            .await
            .unwrap();
        inspect_response(response).await
    }

    async fn inspect_response(response: Response) -> (StatusCode, serde_json::Value) {
        let status = response.status();
        let request_id = response.headers()[crate::REQUEST_ID_HEADER]
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let bytes = to_bytes(response.into_body(), 2 * PARTNER_SYNC_MAX_REQUEST_BYTES)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains(SECRET));
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        if status.is_client_error() || status.is_server_error() {
            assert_eq!(value["request_id"], request_id);
        }
        (status, value)
    }

    #[tokio::test]
    async fn export_requires_its_grant_and_returns_exact_archive_without_publication() {
        use std::io::{Cursor, Read};

        let fake = Fake::new();
        let uri = format!(
            "/api/v1/partner/connections/{}/content-exports/{}",
            fake.principal.connection_id, fake.operation_id
        );
        assert_eq!(
            send(fake.clone(), fake.request("GET", &uri, String::new()))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        fake.export_granted.store(true, Ordering::SeqCst);
        for format in ["markdown", "html"] {
            let response = partner_router::<()>(fake.clone(), NonZeroUsize::new(1).unwrap())
                .oneshot(fake.request("GET", &format!("{uri}?format={format}"), String::new()))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CONTENT_TYPE], "application/zip");
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            let bytes = to_bytes(response.into_body(), 128 * 1024).await.unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains(SECRET));
            let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
            assert_eq!(zip.len(), 2);
            let mut manifest = String::new();
            zip.by_name("manifest.json")
                .unwrap()
                .read_to_string(&mut manifest)
                .unwrap();
            let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
            assert_eq!(
                manifest["content_revision_id"],
                fake.operation_id.to_string()
            );
            assert_eq!(manifest["language"], "en");
            let mut payload = String::new();
            zip.by_name(manifest["payload_filename"].as_str().unwrap())
                .unwrap()
                .read_to_string(&mut payload)
                .unwrap();
            if format == "markdown" {
                assert_eq!(payload, "Saved exact content.\r\n");
            }
            assert_eq!(
                manifest["payload_sha256"],
                geo_domain::sha256_hex(payload.as_bytes())
            );
        }
        for query in [
            "format=pdf",
            "variant_id=forged",
            "tenant_id=forged",
            "format=html&format=markdown",
        ] {
            assert_eq!(
                send(
                    fake.clone(),
                    fake.request("GET", &format!("{uri}?{query}"), String::new())
                )
                .await
                .0,
                StatusCode::BAD_REQUEST
            );
        }
        let missing = uri.replace(&fake.operation_id.to_string(), &Uuid::new_v4().to_string());
        assert_eq!(
            send(fake.clone(), fake.request("GET", &missing, String::new()))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        let foreign = uri.replace(
            &fake.principal.connection_id.to_string(),
            &Uuid::new_v4().to_string(),
        );
        assert_eq!(
            send(fake.clone(), fake.request("GET", &foreign, String::new()))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        fake.revoked.store(true, Ordering::SeqCst);
        assert_eq!(
            send(fake.clone(), fake.request("GET", &uri, String::new()))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert!(
            fake.calls
                .lock()
                .unwrap()
                .iter()
                .all(|call| call == "authenticate" || call == "content_export")
        );
    }

    #[tokio::test]
    async fn saturated_authentication_is_shed_without_queuing_or_leaking_secrets() {
        let fake = Fake::new();
        fake.hold_authentication.store(true, Ordering::SeqCst);
        let router = partner_router::<()>(fake.clone(), NonZeroUsize::new(1).unwrap());
        let request = fake.request("POST", &fake.uri(), body());
        let first = tokio::spawn(router.clone().oneshot(request));
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            fake.authentication_entered.notified(),
        )
        .await
        .unwrap();
        let request = fake.request("POST", &fake.uri(), body());
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            router.clone().oneshot(request),
        )
        .await
        .unwrap()
        .unwrap();
        let (status, error) = inspect_response(response).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error["code"], "not_ready");
        assert_eq!(error["retryable"], true);
        assert_eq!(*fake.calls.lock().unwrap(), ["authenticate"]);
        fake.hold_authentication.store(false, Ordering::SeqCst);
        fake.authentication_release.notify_one();
        let response = tokio::time::timeout(std::time::Duration::from_secs(2), first)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(inspect_response(response).await.0, StatusCode::ACCEPTED);
        let request = fake.request("POST", &fake.uri(), body());
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(inspect_response(response).await.0, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn authentication_permit_is_not_held_during_durable_acceptance() {
        let fake = Fake::new();
        fake.hold_acceptance.store(true, Ordering::SeqCst);
        let router = partner_router::<()>(fake.clone(), NonZeroUsize::new(1).unwrap());
        let request = fake.request("POST", &fake.uri(), body());
        let first = tokio::spawn(router.clone().oneshot(request));
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            fake.acceptance_entered.notified(),
        )
        .await
        .unwrap();
        let uri = format!("{}/{}", fake.uri(), fake.operation_id);
        let request = fake.request("GET", &uri, String::new());
        let response =
            tokio::time::timeout(std::time::Duration::from_secs(2), router.oneshot(request))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(inspect_response(response).await.0, StatusCode::OK);
        fake.acceptance_release.notify_one();
        let response = tokio::time::timeout(std::time::Duration::from_secs(2), first)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(inspect_response(response).await.0, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn service_key_is_required_before_body_and_cookie_is_not_authority() {
        let fake = Fake::new();
        let mut request = fake.request("POST", &fake.uri(), "invalid JSON".into());
        request.headers_mut().remove(header::AUTHORIZATION);
        request
            .headers_mut()
            .insert(header::COOKIE, "geo_dev_session=synthetic".parse().unwrap());
        assert_eq!(
            send(fake.clone(), request).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert!(fake.calls.lock().unwrap().is_empty());
        let uri = fake.uri().replace(
            &fake.principal.connection_id.to_string(),
            &Uuid::new_v4().to_string(),
        );
        let request = fake.request("POST", &uri, body());
        assert_eq!(
            send(fake.clone(), request).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(*fake.calls.lock().unwrap(), ["authenticate"]);
    }

    #[tokio::test]
    async fn accepted_only_after_commit_and_current_grants_checked_on_each_request() {
        let fake = Fake::new();
        let request = fake.request("POST", &fake.uri(), body());
        let (status, receipt) = send(fake.clone(), request).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(receipt["operation_id"], fake.operation_id.to_string());
        assert_eq!(
            *fake.calls.lock().unwrap(),
            ["authenticate", "accept:synthetic-batch"]
        );
        fake.fail_accept.store(true, Ordering::SeqCst);
        let request = fake.request("POST", &fake.uri(), body());
        assert_eq!(
            send(fake.clone(), request).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        fake.revoked.store(true, Ordering::SeqCst);
        let request = fake.request("POST", &fake.uri(), body());
        assert_eq!(send(fake.clone(), request).await.0, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn invalid_json_unknown_scope_media_and_body_bounds_never_accept() {
        for input in [
            "invalid".to_owned(),
            body().replace(
                "\"schema_version\":1",
                "\"schema_version\":1,\"tenant_id\":\"forged\"",
            ),
            body().replace(
                "\"operation\":\"revoke\"",
                "\"operation\":\"revoke\",\"text\":\"forged\"",
            ),
            body().replace("\"schema_version\":1", "\"schema_version\":99"),
            " ".repeat(PARTNER_SYNC_MAX_REQUEST_BYTES + 1),
        ] {
            let fake = Fake::new();
            let request = fake.request("POST", &fake.uri(), input);
            assert_eq!(send(fake.clone(), request).await.0, StatusCode::BAD_REQUEST);
            assert_eq!(*fake.calls.lock().unwrap(), ["authenticate"]);
        }
        let fake = Fake::new();
        let mut request = fake.request("POST", &fake.uri(), body());
        request
            .headers_mut()
            .insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());
        assert_eq!(send(fake.clone(), request).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(*fake.calls.lock().unwrap(), ["authenticate"]);
    }

    #[tokio::test]
    async fn keys_must_be_single_and_bounded() {
        for value in ["", "has spaces", &"a".repeat(257)] {
            let fake = Fake::new();
            let mut request = fake.request("POST", &fake.uri(), body());
            request
                .headers_mut()
                .insert(crate::IDEMPOTENCY_KEY_HEADER, value.parse().unwrap());
            assert_eq!(send(fake.clone(), request).await.0, StatusCode::BAD_REQUEST);
        }
        let fake = Fake::new();
        let mut request = fake.request("POST", &fake.uri(), body());
        request.headers_mut().remove(crate::IDEMPOTENCY_KEY_HEADER);
        assert_eq!(send(fake.clone(), request).await.0, StatusCode::BAD_REQUEST);
        let mut request = fake.request("POST", &fake.uri(), body());
        request
            .headers_mut()
            .append(header::AUTHORIZATION, "Bearer malformed".parse().unwrap());
        assert_eq!(
            send(fake.clone(), request).await.0,
            StatusCode::UNAUTHORIZED
        );
        let mut request = fake.request("POST", &fake.uri(), body());
        request.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {}.{}", fake.principal.key_id, "b".repeat(64))
                .parse()
                .unwrap(),
        );
        assert_eq!(
            send(fake.clone(), request).await.0,
            StatusCode::UNAUTHORIZED
        );
        let mut request = fake.request("POST", &fake.uri(), body());
        request
            .headers_mut()
            .append(crate::IDEMPOTENCY_KEY_HEADER, "second".parse().unwrap());
        assert_eq!(send(fake.clone(), request).await.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn result_reads_are_scoped_paginated_and_reauthenticated() {
        let fake = Fake::new();
        let uri = format!("{}/{}?after_index=2&limit=7", fake.uri(), fake.operation_id);
        let request = fake.request("GET", &uri, String::new());
        assert_eq!(send(fake.clone(), request).await.0, StatusCode::OK);
        assert_eq!(
            *fake.calls.lock().unwrap(),
            ["authenticate", "results:Some(2):7"]
        );
        for query in [
            "limit=0",
            "limit=101",
            "after_index=-1",
            "tenant_id=forged",
            "limit=2&limit=3",
        ] {
            let uri = format!("{}/{}?{query}", fake.uri(), fake.operation_id);
            let request = fake.request("GET", &uri, String::new());
            assert_eq!(send(fake.clone(), request).await.0, StatusCode::BAD_REQUEST);
        }
        let uri = format!("{}/{}", fake.uri(), Uuid::new_v4());
        let request = fake.request("GET", &uri, String::new());
        assert_eq!(send(fake.clone(), request).await.0, StatusCode::NOT_FOUND);
        fake.revoked.store(true, Ordering::SeqCst);
        let uri = format!("{}/{}", fake.uri(), fake.operation_id);
        let request = fake.request("GET", &uri, String::new());
        assert_eq!(send(fake.clone(), request).await.0, StatusCode::FORBIDDEN);
    }
}
