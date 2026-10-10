//! Keep subscriber installation and all diagnostic assertions in one test in a
//! separate process. Parallel provider unit tests also hit these callsites
//! without a subscriber; they must not race this capture's initial registration.
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use geo_provider::diagnostics::{ModelPhase, ModelPhaseTimer};
use geo_provider::{
    CompletionRequest, HttpTransport, Message, ProviderClient, ProviderError, ProviderSurface,
    RequestControl, ResolvedToken, SearchMode, SecretRef, TokenCenter,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::Instrument;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Capture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn phase_outcomes_and_http_stalls_preserve_correlation_without_private_payloads() {
    let output = Capture::default();
    let writer = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
    fixed_fields_cover_success_error_and_dropped_future_without_payloads(&output).await;
    diagnostics_distinguish_stalled_headers_and_body_without_request_payloads(&output).await;
}

async fn fixed_fields_cover_success_error_and_dropped_future_without_payloads(output: &Capture) {
    let span = tracing::info_span!("synthetic_call", call_id = "opaque-test-correlation");
    async {
        // These values never enter the diagnostics API, even on an error.
        let private_payload = "private-source-canary private-token-canary https://private.invalid";
        ModelPhaseTimer::start(ModelPhase::Decode).finish(true);
        let result: Result<(), &str> = Err(private_payload);
        ModelPhaseTimer::start(ModelPhase::Credentials).finish(result.is_ok());
        let pending = async {
            let _phase = ModelPhaseTimer::start(ModelPhase::FullBody);
            std::future::pending::<()>().await;
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(1), pending)
                .await
                .is_err()
        );
    }
    .instrument(span)
    .await;
    let text = output.text();
    for field in [
        "Decode",
        "Credentials",
        "FullBody",
        "started",
        "ok",
        "error",
        "unfinished",
        "elapsed_ms",
        "opaque-test-correlation",
    ] {
        assert!(text.contains(field), "missing {field}");
    }
    for private in [
        "private-source-canary",
        "private-token-canary",
        "private.invalid",
    ] {
        assert!(!text.contains(private));
    }
}

async fn read_headers(stream: &mut TcpStream) {
    let mut bytes = Vec::new();
    let mut buffer = [0; 1024];
    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0, "request ended before headers");
        bytes.extend_from_slice(&buffer[..count]);
        assert!(bytes.len() < 16 * 1024);
    }
}

async fn diagnostics_distinguish_stalled_headers_and_body_without_request_payloads(
    output: &Capture,
) {
    for send_headers in [false, true] {
        output.0.lock().unwrap().clear();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_headers(&mut stream).await;
            if send_headers {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nprivate-body-canary")
                    .await
                    .unwrap();
            }
            std::future::pending::<()>().await;
        });
        // Exercise the public client instead of exposing TransportRequest's
        // private credential field solely for this integration test.
        let client = ProviderClient::new(
            &base_url,
            SecretRef::new("test-ref").unwrap(),
            Arc::new(HttpTransport::new().unwrap()),
            Arc::new(TestTokenCenter),
        )
        .unwrap();
        let result = client
            .complete(
                CompletionRequest {
                    model: "test-model".into(),
                    messages: vec![Message {
                        role: "user".into(),
                        content: Some("private-source-canary".into()),
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
                RequestControl::new(Duration::from_millis(100)).unwrap(),
            )
            .await;
        assert_eq!(result.unwrap_err(), ProviderError::Timeout);
        server.abort();
        let text = output.text();
        assert!(text.contains("HttpHeaders"));
        assert_eq!(text.contains("FullBody"), send_headers);
        assert!(text.contains("unfinished"));
        assert!(!text.contains(&base_url));
        assert!(!text.contains("test-only-bearer-value"));
        assert!(!text.contains("private-body-canary"));
        assert!(!text.contains("private-source-canary"));
        assert!(!text.contains("test-model"));
    }
}

struct TestTokenCenter;

#[async_trait]
impl TokenCenter for TestTokenCenter {
    async fn resolve(&self, _secret_ref: &SecretRef) -> Result<ResolvedToken, ProviderError> {
        ResolvedToken::new("test-only-bearer-value".into())
    }
}
