//! Bounded HTTP transport; endpoint and credentials are supplied by the caller.

use crate::diagnostics::{ModelPhase, ModelPhaseTimer};
use async_trait::async_trait;
use reqwest::{Client, redirect::Policy};
use std::error::Error;
use std::time::Duration;
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
        let builder = Client::builder().redirect(Policy::none());
        // RequestControl bounds the whole operation, including stalled uploads.
        // reqwest's implicit TCP deadline can otherwise expire earlier.
        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        let builder = builder.tcp_user_timeout(None);
        let client = builder
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
                let phase = ModelPhaseTimer::start(ModelPhase::PublicEndpoint);
                let result = crate::public_endpoint_client(&url).await;
                phase.finish(result.is_ok());
                public_client = result?;
                &public_client
            } else {
                &self.client
            };
            let request = client
                .post(url)
                .bearer_auth(request.bearer_token())
                .json(&request.body)
                .build()
                .map_err(|error| transport_failure(&error, ModelPhase::HttpHeaders))?;
            tracing::info!(
                request_payload_bytes = request
                    .body()
                    .and_then(|body| body.as_bytes())
                    .map(|bytes| bytes.len()),
                outer_timeout_ms = control.timeout().as_millis() as u64,
                "model HTTP request prepared"
            );
            let mut retries = 0;
            loop {
                if control.is_cancelled() {
                    return Err(ProviderError::Cancelled);
                }
                let headers_phase = ModelPhaseTimer::start(ModelPhase::HttpHeaders);
                let outbound = request.try_clone().ok_or_else(|| {
                    ProviderError::Transport("HTTP request could not be replayed".into())
                })?;
                let response = client
                    .execute(outbound)
                    .await
                    .map_err(|error| transport_failure(&error, ModelPhase::HttpHeaders));
                headers_phase.finish(response.is_ok());
                let mut response = response?;
                let status = response.status().as_u16();
                tracing::info!(http_status = status, "model response headers received");
                let body_phase = ModelPhaseTimer::start(ModelPhase::FullBody);
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
                    .map_err(|error| transport_failure(&error, ModelPhase::FullBody))?
                {
                    if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
                        return Err(response_too_large());
                    }
                    bytes.extend_from_slice(&chunk);
                }
                body_phase.finish(true);
                // Replay only complete gateway error responses. The outer control
                // bounds all attempts and delays, preserving this business turn.
                if matches!(status, 502 | 503) && retries < 2 {
                    let delay_ms = 250 << retries;
                    retries += 1;
                    tracing::info!(
                        http_status = status,
                        retry = retries,
                        delay_ms,
                        "retrying model gateway response"
                    );
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    continue;
                }
                let decode_phase = ModelPhaseTimer::start(ModelPhase::Decode);
                let body = String::from_utf8(bytes).map_err(|_| {
                    ProviderError::InvalidResponse("provider response is not UTF-8".into())
                })?;
                decode_phase.finish(true);
                return Ok(TransportResponse { status, body });
            }
        };
        tokio::select! {
            biased;
            () = wait_for_cancellation(control.clone()) => Err(ProviderError::Cancelled),
            result = timeout(control.timeout(), send) => result.unwrap_or(Err(ProviderError::Timeout)),
        }
    }
}

fn transport_failure(error: &reqwest::Error, phase: ModelPhase) -> ProviderError {
    // reqwest error text and its source chain can contain URLs and credentials.
    // Only classification flags, I/O kinds and OS error numbers are safe here.
    let io = io_failure(error);
    tracing::warn!(
        phase = ?phase,
        connect = error.is_connect(),
        timeout = error.is_timeout(),
        request = error.is_request(),
        body = error.is_body(),
        decode = error.is_decode(),
        io_kind = ?io.map(|(kind, _)| kind),
        raw_os_error = io.and_then(|(_, code)| code),
        "model HTTP transport failed"
    );
    if error.is_timeout() {
        return ProviderError::Timeout;
    }
    let message = match phase {
        ModelPhase::FullBody => "HTTP response failed",
        _ => "HTTP request failed",
    };
    ProviderError::Transport(message.into())
}

fn io_failure(error: &(dyn Error + 'static)) -> Option<(std::io::ErrorKind, Option<i32>)> {
    let mut source = Some(error);
    // Error implementations may expose unusually deep or cyclic source chains.
    for _ in 0..16 {
        let current = source?;
        if let Some(io) = current.downcast_ref::<std::io::Error>() {
            return Some((io.kind(), io.raw_os_error()));
        }
        source = current.source();
    }
    None
}

fn response_too_large() -> ProviderError {
    ProviderError::InvalidResponse("provider response exceeds the size limit".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::oneshot;

    use std::sync::Arc;

    use crate::{
        CompletionRequest, Message, ProviderClient, ProviderSurface, ResolvedToken, SearchMode,
        SecretRef, TokenCenter,
    };

    #[derive(Debug)]
    struct ErrorSource(Option<Box<dyn Error + Send + Sync>>);

    impl std::fmt::Display for ErrorSource {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("https://example.invalid/?token=synthetic-private-value")
        }
    }

    impl Error for ErrorSource {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            self.0.as_deref().map(|source| source as &dyn Error)
        }
    }

    #[test]
    fn io_failure_extracts_only_safe_fields_from_sensitive_source_chain() {
        let source = ErrorSource(Some(Box::new(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "Authorization: Bearer synthetic-private-value",
        ))));
        let diagnostic = io_failure(&source);
        assert_eq!(diagnostic, Some((std::io::ErrorKind::TimedOut, None)));
        let rendered = format!("{diagnostic:?}");
        assert!(!rendered.contains("synthetic-private-value"));
        assert!(!rendered.contains("example.invalid"));

        let os_error = std::io::Error::from_raw_os_error(123);
        assert_eq!(io_failure(&os_error), Some((os_error.kind(), Some(123))));
    }

    #[test]
    fn io_failure_handles_unknown_deep_and_cyclic_sources() {
        assert_eq!(io_failure(&ErrorSource(None)), None);
        let mut source: Box<dyn Error + Send + Sync> =
            Box::new(std::io::Error::from_raw_os_error(123));
        for _ in 0..16 {
            source = Box::new(ErrorSource(Some(source)));
        }
        assert_eq!(io_failure(source.as_ref()), None);

        #[derive(Debug)]
        struct CyclicError;
        impl std::fmt::Display for CyclicError {
            fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                panic!("diagnostics must not format source errors");
            }
        }
        impl Error for CyclicError {
            fn source(&self) -> Option<&(dyn Error + 'static)> {
                Some(self)
            }
        }
        assert_eq!(io_failure(&CyclicError), None);
    }

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

    async fn read_request(stream: &mut TcpStream) -> String {
        let mut request = read_headers(stream).await.into_bytes();
        let header_end = request
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .unwrap()
            + 4;
        let headers = std::str::from_utf8(&request[..header_end]).unwrap();
        let body_len: usize = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().unwrap())
            })
            .unwrap();
        let mut buffer = [0; 1024];
        while request.len() < header_end + body_len {
            let count = stream.read(&mut buffer).await.unwrap();
            assert!(count > 0, "request ended before body");
            request.extend_from_slice(&buffer[..count]);
        }
        String::from_utf8(request).unwrap()
    }

    async fn write_response(stream: &mut TcpStream, status: u16, body: &str) {
        stream
            .write_all(
                format!(
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    }

    async fn serve_responses(listener: TcpListener, statuses: &[u16], body: &str) -> Vec<String> {
        let mut requests = Vec::new();
        for status in statuses {
            let (mut stream, _) = timeout(Duration::from_secs(2), listener.accept())
                .await
                .unwrap()
                .unwrap();
            requests.push(read_request(&mut stream).await);
            write_response(&mut stream, *status, body).await;
        }
        assert!(
            timeout(Duration::from_millis(40), listener.accept())
                .await
                .is_err()
        );
        requests
    }

    #[tokio::test]
    async fn retries_complete_gateway_errors_with_identical_request_and_tool_results() {
        let (listener, url) = listen().await;
        let server =
            tokio::spawn(async move { serve_responses(listener, &[502, 503, 200], "{}").await });
        let mut request = request(url);
        request.body = json!({
            "model": "test-model",
            "messages": [
                {"role": "assistant", "tool_calls": [{"id": "test-call", "type": "function", "function": {"name": "test_read", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": "test-call", "content": "existing tool result"}
            ]
        });
        let expected_body = serde_json::to_string(&request.body).unwrap();
        let started = tokio::time::Instant::now();
        let response = HttpTransport::new()
            .unwrap()
            .send(
                request,
                RequestControl::new(Duration::from_secs(3)).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        assert!(started.elapsed() >= Duration::from_millis(750));
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests.windows(2).all(|pair| pair[0] == pair[1]));
        let (headers, body) = requests[0].split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
        assert!(headers.contains("authorization: Bearer test-only-bearer-value\r\n"));
        assert_eq!(body, expected_body);
    }

    #[tokio::test]
    async fn stops_after_three_gateway_responses_and_returns_last_error() {
        let (listener, url) = listen().await;
        let server = tokio::spawn(async move {
            serve_responses(listener, &[503, 502, 503], "gateway unavailable").await
        });
        let response = HttpTransport::new()
            .unwrap()
            .send(
                request(url),
                RequestControl::new(Duration::from_secs(3)).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status, 503);
        assert_eq!(response.body, "gateway unavailable");
        assert_eq!(server.await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn does_not_retry_other_statuses_or_invalid_success_json() {
        for status in [200, 302, 400, 403, 408, 429, 500, 501, 504] {
            let (listener, url) = listen().await;
            let server =
                tokio::spawn(async move { serve_responses(listener, &[status], "not JSON").await });
            let response = HttpTransport::new()
                .unwrap()
                .send(
                    request(url),
                    RequestControl::new(Duration::from_secs(2)).unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status, status);
            assert_eq!(response.body, "not JSON");
            assert_eq!(server.await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn does_not_retry_incomplete_or_oversized_gateway_bodies() {
        for oversized in [false, true] {
            let (listener, url) = listen().await;
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_request(&mut stream).await;
                let length = if oversized {
                    MAX_RESPONSE_BYTES + 1
                } else {
                    100
                };
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 502 Test\r\nContent-Length: {length}\r\nConnection: close\r\n\r\npartial"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                drop(stream);
                assert!(
                    timeout(Duration::from_millis(400), listener.accept())
                        .await
                        .is_err()
                );
            });
            let error = HttpTransport::new()
                .unwrap()
                .send(
                    request(url),
                    RequestControl::new(Duration::from_secs(2)).unwrap(),
                )
                .await
                .unwrap_err();
            assert_eq!(
                error,
                if oversized {
                    response_too_large()
                } else {
                    ProviderError::Transport("HTTP response failed".into())
                }
            );
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_gateway_retry_delay() {
        let (listener, url) = listen().await;
        let (responded_tx, responded_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request(&mut stream).await;
            write_response(&mut stream, 502, "gateway unavailable").await;
            drop(stream);
            responded_tx.send(()).unwrap();
            assert!(
                timeout(Duration::from_millis(400), listener.accept())
                    .await
                    .is_err()
            );
        });
        let control = RequestControl::new(Duration::from_secs(2)).unwrap();
        let cancellation = control.clone();
        let task = tokio::spawn(async move {
            HttpTransport::new()
                .unwrap()
                .send(request(url), control)
                .await
        });
        responded_rx.await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!task.is_finished());
        cancellation.cancel();
        let error = timeout(Duration::from_millis(150), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error, ProviderError::Cancelled);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn gateway_attempts_and_delays_share_original_timeout() {
        let (listener, url) = listen().await;
        let server = tokio::spawn(async move {
            for status in [502, 503] {
                let (mut stream, _) = timeout(Duration::from_secs(2), listener.accept())
                    .await
                    .unwrap()
                    .unwrap();
                read_request(&mut stream).await;
                tokio::time::sleep(Duration::from_millis(100)).await;
                write_response(&mut stream, status, "gateway unavailable").await;
            }
            assert!(
                timeout(Duration::from_millis(550), listener.accept())
                    .await
                    .is_err()
            );
        });
        let error = timeout(
            Duration::from_millis(900),
            HttpTransport::new().unwrap().send(
                request(url),
                RequestControl::new(Duration::from_millis(600)).unwrap(),
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(error, ProviderError::Timeout);
        server.await.unwrap();
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
    async fn classifies_reqwest_headers_and_body_timeouts() {
        for send_headers in [false, true] {
            let (listener, url) = listen().await;
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_headers(&mut stream).await;
                if send_headers {
                    stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")
                        .await
                        .unwrap();
                }
                std::future::pending::<()>().await;
            });
            let transport = HttpTransport {
                client: Client::builder()
                    .timeout(Duration::from_millis(100))
                    .build()
                    .unwrap(),
                public_only: false,
            };
            let error = transport
                .send(
                    request(url),
                    RequestControl::new(Duration::from_secs(2)).unwrap(),
                )
                .await
                .unwrap_err();
            assert_eq!(error, ProviderError::Timeout);
            server.abort();
        }
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

    #[tokio::test]
    async fn transport_failure_does_not_expose_request_url() {
        let (listener, url) = listen().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_headers(&mut stream).await;
            // Closing before the response is an actual request transport error.
            drop(stream);
            assert!(
                timeout(Duration::from_millis(400), listener.accept())
                    .await
                    .is_err()
            );
        });
        let private_url = format!("{url}?credential=synthetic-private-value");
        let error = HttpTransport::new()
            .unwrap()
            .send(
                request(private_url),
                RequestControl::new(Duration::from_secs(5)).unwrap(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error,
            ProviderError::Transport("HTTP request failed".into())
        );
        server.await.unwrap();
    }

    struct TestTokenCenter;

    #[async_trait]
    impl TokenCenter for TestTokenCenter {
        async fn resolve(&self, _secret_ref: &SecretRef) -> Result<ResolvedToken, ProviderError> {
            ResolvedToken::new("test-only-bearer-value".into())
        }
    }
}
