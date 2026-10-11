use super::*;
use axum::{
    Router,
    body::to_bytes,
    extract::Request,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

async fn serve(app: Router) -> (BrowserBridge, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        BrowserBridge::new(format!("http://{address}"), "fixture-token".into()).unwrap(),
        server,
    )
}

async fn authenticated_body(request: Request) -> Value {
    assert_eq!(request.headers()["authorization"], "Bearer fixture-token");
    let bytes = to_bytes(request.into_body(), 8192).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn lifecycle_generation_requires_authenticated_typed_receipt() {
    let generation = Uuid::new_v4();
    for (body, valid) in [
        (json!({"generation": generation}), true),
        (json!({}), false),
        (json!({"generation": "invalid"}), false),
        (json!({"generation": null}), false),
    ] {
        let app = Router::new().route(
            "/v1/lifecycle",
            get(move |request: Request| {
                let body = body.clone();
                async move {
                    assert_eq!(request.headers()["authorization"], "Bearer fixture-token");
                    axum::Json(body)
                }
            }),
        );
        let (bridge, server) = serve(app).await;
        let result = bridge.lifecycle_generation().await;
        if valid {
            assert_eq!(result.unwrap(), generation);
        } else {
            assert!(result.is_err());
        }
        server.abort();
    }
}

#[tokio::test]
async fn disposal_requires_exact_success_receipt_and_sends_generation() {
    let session_id = Uuid::new_v4();
    let generation = Uuid::new_v4();
    let valid = json!({"session_id": session_id, "generation": generation, "state": "disposed"});
    let cases = [
        (StatusCode::OK, valid.clone(), true),
        (
            StatusCode::OK,
            json!({"session_id": Uuid::new_v4(), "generation": generation, "state": "disposed"}),
            false,
        ),
        (
            StatusCode::OK,
            json!({"session_id": session_id, "generation": Uuid::new_v4(), "state": "disposed"}),
            false,
        ),
        (
            StatusCode::OK,
            json!({"session_id": session_id, "generation": generation, "state": "pending"}),
            false,
        ),
        (
            StatusCode::OK,
            json!({"session_id": session_id, "generation": generation}),
            false,
        ),
        (
            StatusCode::OK,
            json!({"session_id": session_id, "state": "disposed"}),
            false,
        ),
        (
            StatusCode::OK,
            json!({"generation": generation, "state": "disposed"}),
            false,
        ),
        (
            StatusCode::OK,
            json!({"session_id": "invalid", "generation": generation, "state": "disposed"}),
            false,
        ),
        (
            StatusCode::OK,
            json!({"session_id": session_id, "generation": generation, "state": "disposed", "extra": true}),
            false,
        ),
        (StatusCode::OK, Value::Null, false),
        (StatusCode::NOT_FOUND, valid.clone(), false),
        (StatusCode::CONFLICT, valid.clone(), false),
        (StatusCode::INTERNAL_SERVER_ERROR, valid, false),
    ];
    for (status, body, accepted) in cases {
        let app = Router::new().route(
            &format!("/v1/sessions/{session_id}/dispose"),
            post(move |request: Request| {
                let body = body.clone();
                async move {
                    assert_eq!(
                        authenticated_body(request).await,
                        json!({"expected_generation": generation})
                    );
                    (status, axum::Json(body))
                }
            }),
        );
        let (bridge, server) = serve(app).await;
        let result = bridge.dispose_in_generation(session_id, generation).await;
        if accepted {
            let receipt = result.expect("exact receipt");
            assert_eq!(receipt.session_id(), session_id);
            assert_eq!(receipt.generation(), generation);
        } else {
            assert!(result.is_err());
        }
        server.abort();
    }
}

#[tokio::test]
async fn pinned_start_rejects_unpinned_receipts_and_only_uses_pinned_cleanup() {
    let session_id = Uuid::new_v4();
    let generation = Uuid::new_v4();
    for (receipt, valid) in [
        (
            json!({"session_id": session_id, "generation": generation, "phase": "awaiting_login"}),
            true,
        ),
        (json!({"session_id": session_id}), false),
        (
            json!({"session_id": session_id, "generation": Uuid::new_v4()}),
            false,
        ),
        (
            json!({"session_id": Uuid::new_v4(), "generation": generation}),
            false,
        ),
        (Value::Null, false),
    ] {
        let disposals = Arc::new(AtomicUsize::new(0));
        let observed = disposals.clone();
        let unexpected = Arc::new(AtomicUsize::new(0));
        let unexpected_observed = unexpected.clone();
        let app = Router::new()
            .route(
                "/v1/sessions",
                post(move |request: Request| {
                    let receipt = receipt.clone();
                    async move {
                        assert_eq!(
                            authenticated_body(request).await,
                            json!({
                                "session_id": session_id, "platform": "fixture",
                                "expected_generation": generation
                            })
                        );
                        axum::Json(receipt)
                    }
                }),
            )
            .route(
                &format!("/v1/sessions/{session_id}/dispose"),
                post(move |request: Request| {
                    let observed = observed.clone();
                    async move {
                        observed.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(
                            authenticated_body(request).await,
                            json!({"expected_generation": generation})
                        );
                        // Failed compensation must not fall back or mask start failure.
                        StatusCode::CONFLICT
                    }
                }),
            )
            .fallback(move || {
                let observed = unexpected_observed.clone();
                async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            });
        let (bridge, server) = serve(app).await;
        assert_eq!(
            bridge
                .start_in_generation(session_id, generation, "fixture", None, None)
                .await
                .is_ok(),
            valid
        );
        assert_eq!(disposals.load(Ordering::SeqCst), usize::from(!valid));
        assert_eq!(unexpected.load(Ordering::SeqCst), 0);
        server.abort();
    }
}

#[tokio::test]
async fn lifecycle_redirects_are_not_followed() {
    let reached = Arc::new(AtomicUsize::new(0));
    let observed = reached.clone();
    let post_observed = reached.clone();
    let session_id = Uuid::new_v4();
    let generation = Uuid::new_v4();
    let app = Router::new()
        .route(
            "/v1/lifecycle",
            get(|| async {
                (
                    StatusCode::TEMPORARY_REDIRECT,
                    [("location", "/destination")],
                )
            }),
        )
        .route(
            "/v1/sessions",
            post(|| async {
                (
                    StatusCode::TEMPORARY_REDIRECT,
                    [("location", "/destination")],
                )
            }),
        )
        .route(
            &format!("/v1/sessions/{session_id}/dispose"),
            post(|| async {
                (
                    StatusCode::TEMPORARY_REDIRECT,
                    [("location", "/destination")],
                )
            }),
        )
        .route(
            "/destination",
            get(move || {
                let observed = observed.clone();
                async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    StatusCode::OK
                }
            })
            .post(move || {
                let reached = post_observed.clone();
                async move {
                    reached.fetch_add(1, Ordering::SeqCst);
                    StatusCode::OK
                }
            }),
        );
    let (bridge, server) = serve(app).await;
    assert!(bridge.lifecycle_generation().await.is_err());
    assert!(
        bridge
            .start_in_generation(session_id, generation, "fixture", None, None)
            .await
            .is_err()
    );
    assert!(
        bridge
            .dispose_in_generation(session_id, generation)
            .await
            .is_err()
    );
    assert_eq!(reached.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn disposal_rejects_missing_malformed_and_oversized_bodies() {
    let session_id = Uuid::new_v4();
    for (status, body) in [
        (StatusCode::NO_CONTENT, String::new()),
        (StatusCode::OK, "{".into()),
        (StatusCode::OK, " ".repeat(8 * 1024 * 1024 + 1)),
    ] {
        let app = Router::new().route(
            &format!("/v1/sessions/{session_id}/dispose"),
            post(move || {
                let body = body.clone();
                async move { (status, body).into_response() }
            }),
        );
        let (bridge, server) = serve(app).await;
        assert!(
            bridge
                .dispose_in_generation(session_id, Uuid::new_v4())
                .await
                .is_err()
        );
        server.abort();
    }
}

#[tokio::test]
async fn legacy_start_and_close_keep_wire_compatibility() {
    let session_id = Uuid::new_v4();
    let app = Router::new()
        .route(
            "/v1/sessions",
            post(move |request: Request| async move {
                assert_eq!(
                    authenticated_body(request).await,
                    json!({
                        "session_id": session_id, "platform": "fixture"
                    })
                );
                axum::Json(json!({"session_id": session_id}))
            }),
        )
        .route(
            &format!("/v1/sessions/{session_id}"),
            axum::routing::delete(|| async { StatusCode::NOT_FOUND }),
        );
    let (bridge, server) = serve(app).await;
    bridge
        .start(session_id, "fixture", None, None)
        .await
        .unwrap();
    bridge.close(session_id).await.unwrap();
    server.abort();
}
