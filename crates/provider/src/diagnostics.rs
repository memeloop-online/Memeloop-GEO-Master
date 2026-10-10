//! Payload-free phase timing. An unfinished phase means an early return or
//! dropped future, not proof of a timeout; consult the caller's outcome.
use std::time::Instant;

/// Closed vocabulary prevents accidentally logging request-derived labels.
#[derive(Clone, Copy, Debug)]
pub enum ModelPhase {
    Settings,
    InheritedDispatch,
    CustomDispatch,
    DirectDispatch,
    Route,
    Credentials,
    Transport,
    PublicEndpoint,
    HttpHeaders,
    FullBody,
    Decode,
    Grounding,
}

pub struct ModelPhaseTimer {
    phase: ModelPhase,
    started: Instant,
    outcome: &'static str,
    span: tracing::Span,
}

impl ModelPhaseTimer {
    pub fn start(phase: ModelPhase) -> Self {
        let timer = Self {
            phase,
            started: Instant::now(),
            outcome: "unfinished",
            span: tracing::Span::current(),
        };
        tracing::info!(phase = ?phase, outcome = "started", "model phase");
        timer
    }

    pub fn finish(mut self, success: bool) {
        self.outcome = if success { "ok" } else { "error" };
    }
}

impl Drop for ModelPhaseTimer {
    fn drop(&mut self) {
        let _entered = self.span.enter();
        tracing::info!(
            phase = ?self.phase,
            outcome = self.outcome,
            elapsed_ms = self.started.elapsed().as_millis() as u64,
            "model phase"
        );
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};
    use tracing::Instrument;

    #[derive(Clone, Default)]
    pub(crate) struct Capture(pub(crate) Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fixed_fields_cover_success_error_and_dropped_future_without_payloads() {
        let output = Capture::default();
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _subscriber = tracing::subscriber::set_default(subscriber);
        let span = tracing::info_span!("synthetic_call", call_id = "opaque-test-correlation");
        async {
            // These values never enter the diagnostics API, even on an error.
            let private_payload =
                "private-source-canary private-token-canary https://private.invalid";
            ModelPhaseTimer::start(ModelPhase::Decode).finish(true);
            let result: Result<(), &str> = Err(private_payload);
            ModelPhaseTimer::start(ModelPhase::Credentials).finish(result.is_ok());
            let pending = async {
                let _phase = ModelPhaseTimer::start(ModelPhase::FullBody);
                std::future::pending::<()>().await;
            };
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(1), pending)
                    .await
                    .is_err()
            );
        }
        .instrument(span)
        .await;
        let text = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
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
}
