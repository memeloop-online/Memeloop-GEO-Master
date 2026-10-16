//! A one-time, cookie-bound WebSocket relay to the server-selected runner session.
//! RFB endpoints and runner authorization never cross the public API boundary.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::{HeaderMap, Uri, header},
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use geo_domain::{AppError, Role};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

use crate::{
    ApiError, AuthContext, RequestContext, SharedAuthRepository, api_error,
    browser_bridge::BrowserBridge,
};

#[derive(Clone, Default)]
pub(crate) struct DesktopGrants {
    inner: Arc<Mutex<GrantState>>,
}

#[derive(Default)]
struct GrantState {
    owners: HashMap<Uuid, OwnerEntry>,
    grants: HashMap<Uuid, Grant>,
}

struct OwnerEntry {
    identity: Owner,
    expires: Instant,
}

#[derive(Clone, PartialEq, Eq)]
struct Owner {
    operator: String,
    tenant: String,
    user: String,
    session: String,
}

struct Grant {
    owner: Owner,
    login: Uuid,
    origin: String,
    expires: Instant,
}

pub(crate) struct DesktopWatch {
    pub(crate) repository: SharedAuthRepository,
    pub(crate) headers: HeaderMap,
    pub(crate) uri: Uri,
    pub(crate) customer: bool,
    pub(crate) tenant: String,
    pub(crate) expires_at: chrono::DateTime<chrono::Utc>,
}

fn owner(auth: &AuthContext, tenant: &str) -> Owner {
    Owner {
        operator: auth.operator.id.to_string(),
        tenant: tenant.to_owned(),
        user: auth.user.id.to_string(),
        session: auth.session.id.to_string(),
    }
}

impl DesktopGrants {
    pub(crate) async fn bind(&self, id: Uuid, auth: &AuthContext, tenant: &str) {
        let mut state = self.inner.lock().await;
        let now = Instant::now();
        state.owners.retain(|_, entry| entry.expires > now);
        state.grants.retain(|_, grant| grant.expires > now);
        state.owners.insert(
            id,
            OwnerEntry {
                identity: owner(auth, tenant),
                expires: now + Duration::from_secs(20 * 60),
            },
        );
        drop(state);
        let grants = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(20 * 60)).await;
            let mut state = grants.inner.lock().await;
            if state
                .owners
                .get(&id)
                .is_some_and(|entry| entry.expires <= Instant::now())
            {
                state.owners.remove(&id);
                state.grants.retain(|_, grant| grant.login != id);
            }
        });
    }

    pub(crate) async fn revoke(&self, id: Uuid) {
        let mut state = self.inner.lock().await;
        state.owners.remove(&id);
        state.grants.retain(|_, grant| grant.login != id);
    }

    pub(crate) async fn verify(
        &self,
        id: Uuid,
        auth: &AuthContext,
        tenant: &str,
    ) -> Result<(), AppError> {
        if !self
            .inner
            .lock()
            .await
            .owners
            .get(&id)
            .is_some_and(|entry| {
                entry.expires > Instant::now() && entry.identity == owner(auth, tenant)
            })
        {
            return Err(AppError::forbidden(
                "login session belongs to another user or session",
            ));
        }
        Ok(())
    }

    pub(crate) async fn issue(
        &self,
        id: Uuid,
        auth: &AuthContext,
        tenant: &str,
        origin: &str,
    ) -> Result<String, AppError> {
        let mut state = self.inner.lock().await;
        let identity = owner(auth, tenant);
        if !state
            .owners
            .get(&id)
            .is_some_and(|entry| entry.expires > Instant::now() && entry.identity == identity)
        {
            return Err(AppError::forbidden(
                "login session belongs to another user or session",
            ));
        }
        state
            .grants
            .retain(|_, grant| grant.expires > Instant::now());
        let grant_id = Uuid::new_v4();
        state.grants.insert(
            grant_id,
            Grant {
                owner: identity,
                login: id,
                origin: origin.to_owned(),
                expires: Instant::now() + Duration::from_secs(30),
            },
        );
        Ok(format!("geo-desktop.{grant_id}"))
    }

    async fn consume(
        &self,
        protocol: &str,
        id: Uuid,
        auth: &AuthContext,
        tenant: &str,
        origin: &str,
    ) -> Result<(), AppError> {
        let grant_id: Uuid = protocol
            .strip_prefix("geo-desktop.")
            .ok_or_else(|| AppError::forbidden("desktop authorization required"))?
            .parse()
            .map_err(|_| AppError::forbidden("desktop authorization invalid"))?;
        let mut state = self.inner.lock().await;
        // Consume on any attempt, including a mismatched session, origin or scope.
        let grant = state
            .grants
            .remove(&grant_id)
            .ok_or_else(|| AppError::forbidden("desktop authorization expired"))?;
        let identity = owner(auth, tenant);
        if grant.expires <= Instant::now()
            || grant.login != id
            || grant.owner != identity
            || !state
                .owners
                .get(&id)
                .is_some_and(|entry| entry.expires > Instant::now() && entry.identity == identity)
            || grant.origin != origin
        {
            return Err(AppError::forbidden(
                "desktop authorization expired or revoked",
            ));
        }
        Ok(())
    }
}

pub(crate) fn request_origin(headers: &HeaderMap) -> Result<&str, AppError> {
    headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .filter(|origin| !origin.is_empty() && *origin != "null")
        .ok_or_else(|| AppError::forbidden("Origin header is required"))
}

pub(crate) fn validate_desktop_origin(
    headers: &HeaderMap,
    config: &crate::OriginConfig,
) -> Result<(), AppError> {
    crate::context::validate_origin_headers_with_config(headers, config)?;
    let origin = request_origin(headers)?;
    let host = crate::context::host_from_headers(headers)?;
    if !origin.eq_ignore_ascii_case(&format!("http://{host}"))
        && !origin.eq_ignore_ascii_case(&format!("https://{host}"))
    {
        return Err(AppError::forbidden(
            "Origin does not match the request host",
        ));
    }
    Ok(())
}

// Keep the browser upgrade, identity proof, private runner, and liveness
// watcher explicit at this trust boundary.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn upgrade(
    upgrade: WebSocketUpgrade,
    headers: HeaderMap,
    grants: DesktopGrants,
    browser: BrowserBridge,
    id: Uuid,
    auth: &AuthContext,
    tenant: &str,
    context: RequestContext,
    watch: DesktopWatch,
) -> Result<Response, ApiError> {
    let origin = request_origin(&headers).map_err(|e| api_error(e, context.request_id))?;
    let raw = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|header| header.to_str().ok())
        .ok_or_else(|| {
            api_error(
                AppError::forbidden("desktop authorization required"),
                context.request_id,
            )
        })?;
    // Never accept multiple protocols: the browser offers only the exact one-time grant.
    if raw.contains(',') {
        return Err(api_error(
            AppError::forbidden("desktop authorization invalid"),
            context.request_id,
        ));
    }
    let protocol = raw.to_owned();
    grants
        .consume(&protocol, id, auth, tenant, origin)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    let (url, token) = browser
        .desktop_connection(id)
        .map_err(|e| api_error(e, context.request_id))?;
    let identity = owner(auth, tenant);
    Ok(upgrade
        .max_message_size(8 * 1024 * 1024)
        .max_frame_size(8 * 1024 * 1024)
        .protocols([protocol])
        .on_upgrade(move |socket| async move {
            relay(socket, url, token, grants, id, identity, watch).await;
        }))
}

async fn relay(
    mut client: WebSocket,
    url: String,
    token: String,
    grants: DesktopGrants,
    id: Uuid,
    identity: Owner,
    watch: DesktopWatch,
) {
    let mut request = match url.as_str().into_client_request() {
        Ok(request) => request,
        Err(_) => return,
    };
    let Ok(value) = format!("Bearer {token}").parse() else {
        return;
    };
    request.headers_mut().insert("Authorization", value);
    let Ok(Ok((mut upstream, _))) = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::connect_async(request),
    )
    .await
    else {
        let _ = client.close().await;
        return;
    };
    let mut check = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = check.tick() => {
                if !grants.inner.lock().await.owners.get(&id).is_some_and(|entry| {
                    entry.expires > Instant::now() && entry.identity == identity
                })
                    || chrono::Utc::now() >= watch.expires_at { break; }
                let Ok(current) = crate::context::resolve_auth_context_from_parts(
                    &*watch.repository, &watch.headers, &watch.uri, watch.customer,
                    None,
                ).await else { break; };
                if owner(&current, &watch.tenant) != identity { break; }
                if watch.customer && crate::require_project_writer(&current).is_err() { break; }
                if !watch.customer && !current.memberships.iter().any(|membership| {
                    membership.active
                        && membership.tenant_id.to_string() == watch.tenant
                        && matches!(membership.role, Role::ResourceAdmin | Role::OemAdmin)
                }) { break; }
            }
            frame = client.next() => {
                let outgoing = match frame {
                    Some(Ok(Message::Binary(bytes))) => tokio_tungstenite::tungstenite::Message::Binary(bytes),
                    Some(Ok(Message::Text(text))) => tokio_tungstenite::tungstenite::Message::Text(text.to_string().into()),
                    Some(Ok(Message::Ping(bytes))) => tokio_tungstenite::tungstenite::Message::Ping(bytes),
                    Some(Ok(Message::Pong(bytes))) => tokio_tungstenite::tungstenite::Message::Pong(bytes),
                    _ => break,
                };
                if !matches!(
                    tokio::time::timeout(Duration::from_secs(3), upstream.send(outgoing)).await,
                    Ok(Ok(()))
                ) { break; }
            }
            frame = upstream.next() => {
                let outgoing = match frame {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(bytes))) => Message::Binary(bytes),
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => Message::Text(text.to_string().into()),
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(bytes))) => Message::Ping(bytes),
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Pong(bytes))) => Message::Pong(bytes),
                    _ => break,
                };
                if !matches!(
                    tokio::time::timeout(Duration::from_secs(3), client.send(outgoing)).await,
                    Ok(Ok(()))
                ) { break; }
            }
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), client.close()).await;
    let _ = tokio::time::timeout(Duration::from_secs(1), upstream.close(None)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_domain::{Operator, OperatorId, TenantId, TenantScope, User, UserId};

    fn auth() -> AuthContext {
        let operator = Operator::new(OperatorId::from(Uuid::new_v4()), "fixture", "Fixture")
            .expect("operator");
        let user = User::from_password_hash(
            UserId::from(Uuid::new_v4()),
            operator.id,
            "fixture@example.invalid",
            "Fixture",
            true,
            "fixture-hash",
            chrono::Utc::now(),
            chrono::Utc::now(),
        )
        .expect("user");
        AuthContext {
            session: geo_domain::Session::new(operator.id, user.id, chrono::Duration::minutes(5))
                .session,
            memberships: Vec::new(),
            scope: TenantScope::new(operator.id, TenantId::from(Uuid::new_v4()), None),
            operator,
            user,
        }
    }

    #[tokio::test]
    async fn one_time_grants_bind_cookie_operator_tenant_and_origin() {
        let grants = DesktopGrants::default();
        let owner = auth();
        let login = Uuid::new_v4();
        let tenant = owner.scope.tenant_id.to_string();
        grants.bind(login, &owner, &tenant).await;
        let protocol = grants
            .issue(login, &owner, &tenant, "https://example.invalid")
            .await
            .expect("issue");
        assert!(
            grants
                .consume(&protocol, login, &owner, &tenant, "https://evil.invalid")
                .await
                .is_err()
        );
        assert!(
            grants
                .consume(&protocol, login, &owner, &tenant, "https://example.invalid")
                .await
                .is_err()
        );
        let protocol = grants
            .issue(login, &owner, &tenant, "https://example.invalid")
            .await
            .expect("new issue");
        let different_cookie = auth();
        assert!(
            grants
                .consume(
                    &protocol,
                    login,
                    &different_cookie,
                    &tenant,
                    "https://example.invalid"
                )
                .await
                .is_err()
        );
        let protocol = grants
            .issue(login, &owner, &tenant, "https://example.invalid")
            .await
            .expect("third issue");
        assert!(
            grants
                .consume(&protocol, login, &owner, &tenant, "https://example.invalid")
                .await
                .is_ok()
        );
        assert!(
            grants
                .consume(&protocol, login, &owner, &tenant, "https://example.invalid")
                .await
                .is_err()
        );
        grants.revoke(login).await;
        assert!(
            grants
                .issue(login, &owner, &tenant, "https://example.invalid")
                .await
                .is_err()
        );
    }

    #[test]
    fn desktop_origin_must_match_host_even_when_allowed_elsewhere() {
        let config = crate::OriginConfig::from_allowed_origins([
            "https://example.invalid",
            "https://other.invalid",
        ]);
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "example.invalid".parse().unwrap());
        headers.insert(header::ORIGIN, "https://other.invalid".parse().unwrap());
        assert!(validate_desktop_origin(&headers, &config).is_err());
        headers.insert(header::ORIGIN, "https://example.invalid".parse().unwrap());
        assert!(validate_desktop_origin(&headers, &config).is_ok());
        headers.insert(header::ORIGIN, "null".parse().unwrap());
        assert!(validate_desktop_origin(&headers, &config).is_err());
    }
}
