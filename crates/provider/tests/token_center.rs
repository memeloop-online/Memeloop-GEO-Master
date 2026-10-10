use std::sync::Arc;
use std::time::Duration;

use geo_provider::{
    CompletionRequest, HttpTokenCenter, Message, ProviderClient, ProviderError, ProviderSurface,
    RequestControl, SearchMode, SecretRef, TokenCenter, TokenCenterKeyMapping, Transport,
    TransportRequest, TransportResponse,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const KEY_ID: &str = "a7d32917-989c-424d-b24a-0526603de89d";
const DOWNSTREAM: &str = "test-only-downstream-credential";
const SERVICE: &str = "test-only-management-credential";

fn binding(tenant: &str) -> TokenCenterKeyMapping {
    TokenCenterKeyMapping::new(
        SecretRef::new("trusted-tenant-reference").unwrap(),
        tenant,
        "trusted-principal",
        KEY_ID,
    )
    .unwrap()
}

fn metadata(tenant: &str, copy_available: bool) -> String {
    json!([{
        "key_id": KEY_ID,
        "tenant_external_id": tenant,
        "principal_external_id": "trusted-principal",
        "status": "active",
        "credential_generation": 2,
        "credential_copy_available": copy_available
    }])
    .to_string()
}

fn copied() -> String {
    json!({"key_id": KEY_ID, "credential_generation": 2, "key": DOWNSTREAM}).to_string()
}

async fn serve(responses: Vec<(u16, String)>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 2048];
            while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                assert!(bytes.len() < 16 * 1024);
            }
            requests.push(String::from_utf8(bytes).unwrap());
            let reason = if status == 200 { "OK" } else { "Not Found" };
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
        requests
    });
    (endpoint, server)
}

struct InferenceTransport;

#[async_trait::async_trait]
impl Transport for InferenceTransport {
    async fn send(
        &self,
        request: TransportRequest,
        _: RequestControl,
    ) -> Result<TransportResponse, ProviderError> {
        assert_eq!(request.bearer_token(), DOWNSTREAM);
        assert_ne!(request.bearer_token(), SERVICE);
        Ok(TransportResponse {
            status: 200,
            body: json!({
                "id": "request-1",
                "model": "routed-model",
                "choices": [{"message": {"content": "answer"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            })
            .to_string(),
        })
    }
}

fn request() -> CompletionRequest {
    CompletionRequest {
        model: "routed-model".into(),
        messages: vec![Message {
            role: "user".into(),
            content: Some("hello".into()),
            tool_calls: vec![],
            tool_call_id: None,
        }],
        tools: vec![],
        max_output_tokens: None,
        temperature: None,
        surface: ProviderSurface::OfficialApi,
        search_mode: SearchMode::Disabled,
        include_citations: false,
    }
}

#[tokio::test]
async fn checks_explicit_tenant_metadata_then_copies_active_key_for_inference() {
    let (endpoint, server) = serve(vec![(200, metadata("tenant-a", true)), (200, copied())]).await;
    let center = Arc::new(HttpTokenCenter::new(endpoint, SERVICE, [binding("tenant-a")]).unwrap());
    let client = ProviderClient::new(
        "https://example.invalid/v1/",
        SecretRef::new("trusted-tenant-reference").unwrap(),
        Arc::new(InferenceTransport),
        center,
    )
    .unwrap();
    let answer = client
        .complete(
            request(),
            RequestControl::new(Duration::from_secs(2)).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(answer.text, "answer");
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /internal/v1/keys?"));
    assert!(requests[0].contains("tenant_external_id=tenant-a"));
    assert!(requests[0].contains("principal_external_id=trusted-principal"));
    assert!(requests[0].contains(&format!("key_id={KEY_ID}")));
    assert!(requests[1].starts_with(&format!("POST /internal/v1/keys/{KEY_ID}/copy ")));
    for request in requests {
        assert!(
            request
                .to_ascii_lowercase()
                .contains(&format!("authorization: bearer {SERVICE}\r\n"))
        );
        assert!(!request.contains(DOWNSTREAM));
    }
}

#[tokio::test]
async fn refuses_unmapped_reference_without_http_request() {
    let center =
        HttpTokenCenter::new("http://127.0.0.1:1/", SERVICE, [binding("tenant-a")]).unwrap();
    let error = center
        .resolve(&SecretRef::new("different-reference").unwrap())
        .await
        .unwrap_err();
    assert!(matches!(error, ProviderError::TokenUnavailable(_)));
    assert!(!error.to_string().contains(SERVICE));
}

#[tokio::test]
async fn rejects_wrong_tenant_or_unavailable_plaintext_without_copying() {
    for body in [metadata("tenant-b", true), metadata("tenant-a", false)] {
        let (endpoint, server) = serve(vec![(200, body)]).await;
        let center = HttpTokenCenter::new(endpoint, SERVICE, [binding("tenant-a")]).unwrap();
        let error = center
            .resolve(&SecretRef::new("trusted-tenant-reference").unwrap())
            .await
            .unwrap_err();
        assert!(matches!(error, ProviderError::TokenUnavailable(_)));
        assert_eq!(server.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn missing_copy_key_and_upstream_errors_are_redacted() {
    for copy in [
        (
            200,
            json!({"key_id": KEY_ID, "credential_generation": 2}).to_string(),
        ),
        (404, json!({"error": {"message": SERVICE}}).to_string()),
    ] {
        let (endpoint, server) = serve(vec![(200, metadata("tenant-a", true)), copy]).await;
        let center = HttpTokenCenter::new(endpoint, SERVICE, [binding("tenant-a")]).unwrap();
        let error = center
            .resolve(&SecretRef::new("trusted-tenant-reference").unwrap())
            .await
            .unwrap_err();
        let diagnostic = format!("{error:?} {error} {center:?} {:?}", binding("tenant-a"));
        assert!(!diagnostic.contains(SERVICE));
        assert!(!diagnostic.contains(DOWNSTREAM));
        assert!(!diagnostic.contains("tenant-a"));
        assert_eq!(server.await.unwrap().len(), 2);
    }
}

#[tokio::test]
async fn rotation_between_metadata_and_copy_fails_closed() {
    let rotated = json!({
        "key_id": KEY_ID,
        "credential_generation": 3,
        "key": DOWNSTREAM
    })
    .to_string();
    let (endpoint, server) = serve(vec![(200, metadata("tenant-a", true)), (200, rotated)]).await;
    let center = HttpTokenCenter::new(endpoint, SERVICE, [binding("tenant-a")]).unwrap();
    let error = center
        .resolve(&SecretRef::new("trusted-tenant-reference").unwrap())
        .await
        .unwrap_err();
    assert!(matches!(error, ProviderError::TokenUnavailable(_)));
    assert!(!error.to_string().contains(DOWNSTREAM));
    assert_eq!(server.await.unwrap().len(), 2);
}

#[tokio::test]
async fn provisioned_generation_mismatch_does_not_copy_credential() {
    let (endpoint, server) = serve(vec![(200, metadata("tenant-a", true))]).await;
    let mapping = binding("tenant-a").with_generation(1).unwrap();
    let center = HttpTokenCenter::new(endpoint, SERVICE, [mapping]).unwrap();
    assert!(matches!(
        center
            .resolve(&SecretRef::new("trusted-tenant-reference").unwrap())
            .await,
        Err(ProviderError::TokenUnavailable(_))
    ));
    assert_eq!(server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn revoked_metadata_does_not_copy_credential() {
    let revoked = json!([{
        "key_id": KEY_ID,
        "tenant_external_id": "tenant-a",
        "principal_external_id": "trusted-principal",
        "status": "revoked",
        "credential_generation": 2,
        "credential_copy_available": false
    }])
    .to_string();
    let (endpoint, server) = serve(vec![(200, revoked)]).await;
    let center = HttpTokenCenter::new(endpoint, SERVICE, [binding("tenant-a")]).unwrap();
    assert!(matches!(
        center
            .resolve(&SecretRef::new("trusted-tenant-reference").unwrap())
            .await,
        Err(ProviderError::TokenUnavailable(_))
    ));
    assert_eq!(server.await.unwrap().len(), 1);
}
