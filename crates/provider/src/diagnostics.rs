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
