//! Read-only saved-analysis evidence selection, shared by insights and reports.
//! This has no inference, dispatch, cleanup, or measurement-write capability.
use std::sync::Arc;

use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ChannelTargetView, MAX_ANALYSIS_PROJECTION_ATTEMPTS, ObservationAnalysisRepository,
    ObservationAnalysisRevision, ObservationAnalysisSource, ObservationCaptureRepository,
    TenantScope, observation_analysis_source_json,
};

#[derive(Clone)]
pub struct ObservationEvidenceResolver {
    analyses: Arc<dyn ObservationAnalysisRepository>,
    captures: Arc<dyn ObservationCaptureRepository>,
}

impl ObservationEvidenceResolver {
    pub fn new(
        analyses: Arc<dyn ObservationAnalysisRepository>,
        captures: Arc<dyn ObservationCaptureRepository>,
    ) -> Self {
        Self { analyses, captures }
    }

    /// Callers supply scoped persisted views. Reports truncate attempts to their
    /// as-of boundary first; a newer pending attempt never revives an older one.
    /// Consumers project the grounded result and provenance, not candidate JSON.
    pub async fn resolve(
        &self,
        scope: &TenantScope,
        views: &[ChannelTargetView],
        as_of: DateTime<Utc>,
    ) -> Result<Vec<ObservationAnalysisRevision>, AppError> {
        if scope.project_id.is_none() {
            return Err(AppError::forbidden("project scope required"));
        }
        let attempts = views
            .iter()
            .filter_map(|view| {
                view.attempts
                    .last()
                    .filter(|attempt| {
                        attempt.claimed_at <= as_of
                            && attempt.received_at.is_some_and(|at| at <= as_of)
                    })
                    .map(|attempt| (view.target.target_id, attempt.attempt_id))
            })
            .collect::<Vec<_>>();
        let mut selected = Vec::new();
        for batch in attempts.chunks(MAX_ANALYSIS_PROJECTION_ATTEMPTS) {
            let revisions = self
                .analyses
                .latest_grounded_for_attempts(scope, batch, as_of)
                .await?;
            for revision in revisions {
                let Some(view) = views.iter().find(|view| {
                    view.target.target_id == revision.request.target_id
                        && view.attempts.last().is_some_and(|attempt| {
                            attempt.attempt_id == revision.request.attempt_id
                        })
                }) else {
                    continue;
                };
                let capture = match revision.request.source {
                    ObservationAnalysisSource::Capture { capture_id } => {
                        let Some(capture) = self.captures.get(scope, capture_id).await? else {
                            continue;
                        };
                        if capture.receipt.stored_at > as_of
                            || capture.receipt.capture_id != capture.input.capture_id
                            || capture.receipt.schema_version != 1
                        {
                            continue;
                        }
                        Some(capture)
                    }
                    ObservationAnalysisSource::AttemptEvidence { .. } => None,
                };
                if observation_analysis_source_json(
                    scope,
                    &revision.request,
                    view,
                    capture.as_ref(),
                )
                .is_ok()
                {
                    selected.push(revision);
                }
            }
        }
        Ok(selected)
    }
}
