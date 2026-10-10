//! Bounded HTTP transport; endpoint and credentials are supplied by the caller.

use async_trait::async_trait;
use reqwest::{Client, redirect::Policy};
use tokio::time::timeout;
use url::Url;

use crate::{
    MAX_RESPONSE_BYTES, ProviderError, RequestControl, Transport, TransportRequest,
    TransportResponse, wait_for_cancellation,
};

#[derive(Clone)]
pub struct HttpTransport {
    client: Client,
    public_only: bool,
}

impl HttpTransport {
    pub fn new() -> Result<Self, ProviderError> {
        let client = Client::builder()
            .redirect(Policy::none())
            .build()
            .map_err(|_| ProviderError::Transport("HTTP client initialization failed".into()))?;
        Ok(Self {
            client,
            public_only: false,
        })
    }

    /// Tenant-configured destinations, distinct from operator-owned gateways.
    pub fn public_only() -> Result<Self, ProviderError> {
        Ok(Self {
            public_only: true,
            ..Self::new()?
        })
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn send(
        &self,
        request: TransportRequest,
        control: RequestControl,
    ) -> Result<TransportResponse, ProviderError> {
        if control.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let url = Url::parse(&request.url)
            .map_err(|_| ProviderError::InvalidRequest("invalid provider endpoint".into()))?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(ProviderError::InvalidRequest(
                "invalid provider endpoint".into(),
            ));
        }
        let send = async {
            let public_client;
            let client = if self.public_only {
                public_client = crate::public_endpoint_client(&url).await?;
                &public_client
            } else {
                &self.client
            };
            let mut response = client
                .post(url)
                .bearer_auth(request.bearer_token())
                .json(&request.body)
                .send()
                .await
                .map_err(|_| ProviderError::Transport("HTTP request failed".into()))?;
            let status = response.status().as_u16();
            if response
                .content_length()
                .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
            {
                return Err(response_too_large());
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| ProviderError::Transport("HTTP response failed".into()))?
            {
                if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
                    return Err(response_too_large());
                }
                bytes.extend_from_slice(&chunk);
            }
            let body = String::from_utf8(bytes).map_err(|_| {
                ProviderError::InvalidResponse("provider response is not UTF-8".into())
            })?;
            Ok(TransportResponse { status, body })
        };
        tokio::select! {
            biased;
            () = wait_for_cancellation(control.clone()) => Err(ProviderError::Cancelled),
            result = timeout(control.timeout(), send) => result.unwrap_or(Err(ProviderError::Timeout)),
        }
    }
}

fn response_too_large() -> ProviderError {
    ProviderError::InvalidResponse("provider response exceeds the size limit".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::oneshot;

    use std::sync::Arc;

    use crate::{
        CompletionRequest, Message, ProviderClient, ProviderSurface, ResolvedToken, SearchMode,
        SecretRef, TokenCenter,
    };

    fn request(url: String) -> TransportRequest {
        TransportRequest {
            url,
            body: json!({"model": "test-model", "messages": []}),
            token: ResolvedToken::new("test-only-bearer-value".into()).unwrap(),
        }
    }

    async fn read_headers(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0; 1024];
        while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream.read(&mut buffer).await.unwrap();
            assert!(count > 0, "request ended before headers");
            bytes.extend_from_slice(&buffer[..count]);
            assert!(bytes.len() < 16 * 1024);
        }
        String::from_utf8(bytes).unwrap()
    }

    async fn listen() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        (listener, url)
    }

    #[tokio::test]
    async fn sends_json_bearer_and_returns_status_without_leaking_error_body() {
        let (listener, url) = listen().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let headers = read_headers(&mut stream).await;
            let body = r#"{"error":{"message":"test-only-bearer-value"}}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 429 Too Many Requests\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            headers
        });
        let client = ProviderClient::new(
            url.trim_end_matches("chat/completions"),
            SecretRef::new("test-ref").unwrap(),
            Arc::new(HttpTransport::new().unwrap()),
            Arc::new(TestTokenCenter),
        )
        .unwrap();
        let error = client
            .complete(
                CompletionRequest {
                    model: "test-model".into(),
                    messages: vec![Message {
                        role: "user".into(),
                        content: Some("test".into()),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                    }],
                    tools: Vec::new(),
                    max_output_tokens: None,
                    temperature: None,
                    surface: ProviderSurface::OfficialApi,
                    search_mode: SearchMode::Disabled,
                    include_citations: false,
                },
                RequestControl::new(Duration::from_secs(2)).unwrap(),
            )
            .await
            .unwrap_err();
        let headers = server.await.unwrap().to_ascii_lowercase();
        assert!(headers.contains("authorization: bearer test-only-bearer-value\r\n"));
        assert!(headers.contains("content-type: application/json\r\n"));
        assert_eq!(
            error.to_string(),
            "provider returned HTTP status 429: provider returned an error"
        );
    }

    #[tokio::test]
    async fn refuses_redirects_with_bearer_credentials() {
        let (listener, url) = listen().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_headers(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/elsewhere\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });
        let response = HttpTransport::new()
            .unwrap()
            .send(
                request(url),
                RequestControl::new(Duration::from_secs(2)).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status, 302);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn limits_streamed_body_without_content_length() {
        let (listener, url) = listen().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_headers(&mut stream).await;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            let chunk = [b'x'; 8192];
            for _ in 0..=(MAX_RESPONSE_BYTES / chunk.len()) {
                if stream.write_all(b"2000\r\n").await.is_err()
                    || stream.write_all(&chunk).await.is_err()
                    || stream.write_all(b"\r\n").await.is_err()
                {
                    break;
                }
            }
            let _ = stream.write_all(b"0\r\n\r\n").await;
        });
        let error = HttpTransport::new()
            .unwrap()
            .send(
                request(url),
                RequestControl::new(Duration::from_secs(10)).unwrap(),
            )
            .await
            .unwrap_err();
        assert_eq!(error, response_too_large());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn cancels_stalled_response() {
        let (listener, url) = listen().await;
        let (accepted_tx, accepted_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_headers(&mut stream).await;
            accepted_tx.send(()).unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        let control = RequestControl::new(Duration::from_secs(2)).unwrap();
        let cancellation = control.clone();
        let task = tokio::spawn(async move {
            HttpTransport::new()
                .unwrap()
                .send(request(url), control)
                .await
        });
        accepted_rx.await.unwrap();
        cancellation.cancel();
        assert_eq!(task.await.unwrap().unwrap_err(), ProviderError::Cancelled);
        server.abort();
    }

    #[tokio::test]
    async fn times_out_stalled_response() {
        let (listener, url) = listen().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_headers(&mut stream).await;
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        let error = HttpTransport::new()
            .unwrap()
            .send(
                request(url),
                RequestControl::new(Duration::from_millis(100)).unwrap(),
            )
            .await
            .unwrap_err();
        assert_eq!(error, ProviderError::Timeout);
        server.abort();
    }

    #[tokio::test]
    async fn tenant_transport_rejects_loopback_before_sending_credentials() {
        let (listener, url) = listen().await;
        let error = HttpTransport::public_only()
            .unwrap()
            .send(
                request(url),
                RequestControl::new(Duration::from_secs(2)).unwrap(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ProviderError::InvalidRequest(_)));
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }

    struct TestTokenCenter;

    #[async_trait]
    impl TokenCenter for TestTokenCenter {
        async fn resolve(&self, _secret_ref: &SecretRef) -> Result<ResolvedToken, ProviderError> {
            ResolvedToken::new("test-only-bearer-value".into())
        }
    }
}
