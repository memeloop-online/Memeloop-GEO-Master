use async_trait::async_trait;
use axum::{
    body::{Body, Bytes, to_bytes},
    extract::{Request, State},
    http::{HeaderValue, Method, StatusCode, header::CONTENT_TYPE, response::Parts},
    middleware::Next,
    response::Response,
};
use geo_domain::{AppError, TenantScope};
pub use geo_domain::{IdempotencyDecision, IdempotencyStore, IdempotencyToken, StoredResponse};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

use crate::{context::RequestContext, error::error_response};

/// Header required for a state-changing JSON request.
pub const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";

/// Maximum buffered request size for the generic JSON idempotency middleware.
pub const MAX_IDEMPOTENCY_REQUEST_BYTES: usize = 1_048_576;

/// Maximum cached response size for the generic JSON idempotency middleware.
pub const MAX_IDEMPOTENCY_RESPONSE_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct StorageKey {
    scope: String,
    key: String,
}

#[derive(Debug, Clone)]
enum Record {
    InFlight {
        body_hash: String,
    },
    Completed {
        body_hash: String,
        response: StoredResponse,
    },
}

/// Development-only in-memory idempotency store.
///
/// A production implementation must move this contract to a durable,
/// transactionally unique store (normally PostgreSQL or Redis backed by a
/// durable record), rather than relying on process memory.
#[derive(Debug, Default)]
pub struct MemoryIdempotencyStore {
    records: Mutex<HashMap<StorageKey, Record>>,
}

impl MemoryIdempotencyStore {
    pub async fn len(&self) -> usize {
        self.records.lock().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.records.lock().await.is_empty()
    }
}

#[async_trait]
impl IdempotencyStore for MemoryIdempotencyStore {
    async fn begin(
        &self,
        scope: &TenantScope,
        key: &str,
        body_hash: &str,
    ) -> Result<IdempotencyDecision, AppError> {
        if key.trim().is_empty() {
            return Err(AppError::invalid_request(
                "Idempotency-Key must not be empty",
            ));
        }
        if body_hash.trim().is_empty() {
            return Err(AppError::invalid_request(
                "request body hash must not be empty",
            ));
        }
        let storage_key = StorageKey {
            scope: scope.storage_key(),
            key: key.to_owned(),
        };
        let token = IdempotencyToken {
            scope: storage_key.scope.clone(),
            key: storage_key.key.clone(),
            body_hash: body_hash.to_owned(),
        };
        let mut records = self.records.lock().await;
        match records.get(&storage_key) {
            None => {
                records.insert(
                    storage_key,
                    Record::InFlight {
                        body_hash: body_hash.to_owned(),
                    },
                );
                Ok(IdempotencyDecision::New(token))
            }
            Some(Record::InFlight {
                body_hash: existing,
            }) if existing == body_hash => Ok(IdempotencyDecision::InFlight),
            Some(Record::Completed {
                body_hash: existing,
                response,
            }) if existing == body_hash => Ok(IdempotencyDecision::Replay(response.clone())),
            Some(_) => Err(AppError::conflict(
                "Idempotency-Key was already used with a different request body",
            )),
        }
    }

    async fn complete(
        &self,
        token: &IdempotencyToken,
        response: StoredResponse,
    ) -> Result<(), AppError> {
        let storage_key = StorageKey {
            scope: token.scope.clone(),
            key: token.key.clone(),
        };
        let mut records = self.records.lock().await;
        match records.get(&storage_key) {
            Some(Record::InFlight { body_hash }) if body_hash == &token.body_hash => {
                records.insert(
                    storage_key,
                    Record::Completed {
                        body_hash: token.body_hash.clone(),
                        response,
                    },
                );
                Ok(())
            }
            Some(Record::Completed { body_hash, .. }) if body_hash == &token.body_hash => Ok(()),
            _ => Err(AppError::conflict(
                "idempotency reservation is missing or belongs to another request",
            )),
        }
    }
}

pub fn body_hash(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    hex::encode(digest)
}

pub type SharedIdempotencyStore = Arc<dyn IdempotencyStore>;

/// Apply idempotency to bounded JSON command endpoints.
///
/// Install this middleware with `middleware::from_fn_with_state`, placing the
/// tenant-scope middleware outside it so that a [`TenantScope`] extension is
/// already present. It leaves non-command methods alone; `GET` and SSE routes
/// therefore retain their normal streaming behaviour.
///
/// The middleware buffers request and response bodies in order to hash and
/// replay them. Both limits are one MiB. Requests above the request limit are
/// rejected before reaching the handler. A response above the cache limit is
/// replaced by a cached internal-error response so a reservation cannot remain
/// permanently in-flight. File uploads and streaming commands need a dedicated
/// idempotency design (for example an upload session or persisted manifest),
/// rather than this bounded JSON middleware.
pub async fn json_command_idempotency_middleware(
    State(store): State<SharedIdempotencyStore>,
    request: Request,
    next: Next,
) -> Response {
    if !is_command_method(request.method()) {
        return next.run(request).await;
    }

    let request_context = request.extensions().get::<RequestContext>().copied();
    let Some(scope) = request.extensions().get::<TenantScope>().cloned() else {
        return error_response(
            AppError::invalid_request("tenant scope is required for idempotent commands"),
            request_context,
        );
    };

    let key = match idempotency_key(&request) {
        Ok(key) => key,
        Err(error) => return error_response(error, request_context),
    };

    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, MAX_IDEMPOTENCY_REQUEST_BYTES).await {
        Ok(body) => body,
        Err(_) => return request_too_large_response(request_context),
    };
    let hash = body_hash(&body);

    let token = match store.begin(&scope, &key, &hash).await {
        Ok(IdempotencyDecision::New(token)) => token,
        Ok(IdempotencyDecision::Replay(response)) => return replay_response(response),
        Ok(IdempotencyDecision::InFlight) => {
            return error_response(
                AppError::conflict(
                    "an identical request with this Idempotency-Key is already being processed",
                ),
                request_context,
            );
        }
        Err(error) => return error_response(error, request_context),
    };

    let response = next.run(Request::from_parts(parts, Body::from(body))).await;
    cache_and_return_response(&*store, token, response, request_context).await
}

fn is_command_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

fn idempotency_key(request: &Request) -> Result<String, AppError> {
    let value = request
        .headers()
        .get(IDEMPOTENCY_KEY_HEADER)
        .ok_or_else(|| AppError::invalid_request("missing Idempotency-Key header"))?
        .to_str()
        .map_err(|_| AppError::invalid_request("invalid Idempotency-Key header"))?
        .trim();

    if value.is_empty() {
        return Err(AppError::invalid_request(
            "Idempotency-Key must not be empty",
        ));
    }
    Ok(value.to_owned())
}

async fn cache_and_return_response(
    store: &dyn IdempotencyStore,
    token: IdempotencyToken,
    response: Response,
    request_context: Option<RequestContext>,
) -> Response {
    let (parts, body) = response.into_parts();
    let body = match to_bytes(body, MAX_IDEMPOTENCY_RESPONSE_BYTES).await {
        Ok(body) => body,
        Err(_) => return cache_overflow_response(store, token, request_context).await,
    };
    let stored = stored_response(&parts, &body);

    if let Err(error) = store.complete(&token, stored).await {
        return error_response(error, request_context);
    }
    Response::from_parts(parts, Body::from(body))
}

async fn cache_overflow_response(
    store: &dyn IdempotencyStore,
    token: IdempotencyToken,
    request_context: Option<RequestContext>,
) -> Response {
    let response = error_response(
        AppError::new(
            geo_domain::ErrorCode::Internal,
            "response body exceeds the idempotency cache limit; this endpoint needs a dedicated streaming or upload idempotency design",
        ),
        request_context,
    );
    let (parts, body) = response.into_parts();
    let body = to_bytes(body, MAX_IDEMPOTENCY_RESPONSE_BYTES)
        .await
        .unwrap_or_default();
    let stored = stored_response(&parts, &body);

    if let Err(error) = store.complete(&token, stored).await {
        return error_response(error, request_context);
    }
    Response::from_parts(parts, Body::from(body))
}

fn stored_response(parts: &Parts, body: &Bytes) -> StoredResponse {
    StoredResponse {
        status: parts.status.as_u16(),
        content_type: parts
            .headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
        body: body.to_vec(),
    }
}

fn replay_response(stored: StoredResponse) -> Response {
    let mut response = Response::new(Body::from(stored.body));
    *response.status_mut() =
        StatusCode::from_u16(stored.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    if !stored.content_type.is_empty()
        && let Ok(content_type) = HeaderValue::try_from(stored.content_type.as_str())
    {
        response.headers_mut().insert(CONTENT_TYPE, content_type);
    }
    response
}

fn request_too_large_response(request_context: Option<RequestContext>) -> Response {
    let mut response = error_response(
        AppError::invalid_request(format!(
            "request body exceeds the {MAX_IDEMPOTENCY_REQUEST_BYTES}-byte idempotency limit; uploads require a dedicated upload protocol"
        )),
        request_context,
    );
    *response.status_mut() = StatusCode::PAYLOAD_TOO_LARGE;
    response
}
