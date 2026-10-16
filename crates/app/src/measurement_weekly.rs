//! Independent weekly snapshots over saved measurements, without sampling.
//! Each tick advances at most one frozen week per project; the next tick can
//! catch up after a restart. An incomplete save/completion stays retryable.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_api::{AppState, MeasurementPeriodRequest, save_project_measurement_report_at_cutoff};
use geo_domain::{AppError, TenantScope};
use geo_persistence::{MeasurementWeeklySchedule, PgReportRepository};
use tracing::warn;
use uuid::Uuid;

const PAGE_SIZE: usize = 100;
type DiscoveryResults = Vec<(Uuid, Result<(), AppError>)>;
type DueCursor = Option<(DateTime<Utc>, Uuid)>;

pub async fn scan(state: &AppState, repository: &PgReportRepository, now: DateTime<Utc>) {
    scan_with(&WeeklyReports { state, repository }, now).await;
}

// This narrow adapter lets the scanner's failure/replay and pagination behavior
// be exercised without a database, provider, model, or optimization cycle.
#[async_trait]
trait WeeklyReportStore: Sync {
    async fn discover(
        &self,
        now: DateTime<Utc>,
        after: Option<Uuid>,
    ) -> Result<DiscoveryResults, AppError>;

    async fn due(
        &self,
        now: DateTime<Utc>,
        after: DueCursor,
    ) -> Result<Vec<MeasurementWeeklySchedule>, AppError>;

    async fn save(
        &self,
        candidate: &MeasurementWeeklySchedule,
        generated_at: DateTime<Utc>,
    ) -> Result<Uuid, AppError>;

    async fn complete(
        &self,
        scope: &TenantScope,
        schedule_id: Uuid,
        report_id: Uuid,
        completed_at: DateTime<Utc>,
    ) -> Result<(), AppError>;
}

struct WeeklyReports<'a> {
    state: &'a AppState,
    repository: &'a PgReportRepository,
}

#[async_trait]
impl WeeklyReportStore for WeeklyReports<'_> {
    async fn discover(
        &self,
        now: DateTime<Utc>,
        after: Option<Uuid>,
    ) -> Result<DiscoveryResults, AppError> {
        self.repository
            .discover_weekly_projects_after(now, after)
            .await
    }

    async fn due(
        &self,
        now: DateTime<Utc>,
        after: DueCursor,
    ) -> Result<Vec<MeasurementWeeklySchedule>, AppError> {
        self.repository
            .due_measurement_weeks_after(now, after)
            .await
    }

    async fn save(
        &self,
        candidate: &MeasurementWeeklySchedule,
        generated_at: DateTime<Utc>,
    ) -> Result<Uuid, AppError> {
        let report = save_project_measurement_report_at_cutoff(
            self.state,
            &candidate.scope,
            report_request(candidate),
            candidate.evidence_as_of,
            generated_at,
        )
        .await?;
        Ok(report.report_id)
    }

    async fn complete(
        &self,
        scope: &TenantScope,
        schedule_id: Uuid,
        report_id: Uuid,
        completed_at: DateTime<Utc>,
    ) -> Result<(), AppError> {
        self.repository
            .complete_measurement_week(scope, schedule_id, report_id, completed_at)
            .await
    }
}

fn report_request(candidate: &MeasurementWeeklySchedule) -> MeasurementPeriodRequest {
    MeasurementPeriodRequest {
        start_at: candidate.window.start_at,
        end_at: candidate.window.end_at,
        report_timezone: candidate.window.report_timezone.clone(),
        correction_of: None,
        question_set: None,
    }
}

async fn scan_with(store: &impl WeeklyReportStore, now: DateTime<Utc>) {
    let mut after = None;
    loop {
        let candidates = match store.discover(now, after).await {
            Ok(candidates) => candidates,
            Err(error) => {
                warn!(code = ?error.code, "weekly measurement project scan failed");
                break;
            }
        };
        let count = candidates.len();
        for (project_id, result) in candidates {
            // Materialization failure must not trap all later projects behind
            // the same first page. Retry it from the beginning on the next tick.
            after = Some(project_id);
            if let Err(error) = result {
                warn!(code = ?error.code, "weekly measurement schedule discovery failed");
            }
        }
        if count < PAGE_SIZE {
            break;
        }
        tokio::task::yield_now().await;
    }

    let mut after = None;
    loop {
        let candidates = match store.due(now, after).await {
            Ok(candidates) => candidates,
            Err(error) => {
                warn!(code = ?error.code, "weekly measurement due scan failed");
                break;
            }
        };
        let count = candidates.len();
        for candidate in candidates {
            after = Some((candidate.due_at, candidate.id));
            // The tick fixes eligibility only. Generation/completion record
            // the actual operation time, even when an old week is recovered.
            let report_id = match store.save(&candidate, Utc::now()).await {
                Ok(report_id) => report_id,
                Err(error) => {
                    warn!(code = ?error.code, "weekly measurement report save failed");
                    continue;
                }
            };
            if let Err(error) = store
                .complete(&candidate.scope, candidate.id, report_id, Utc::now())
                .await
            {
                // If a process dies or completion fails after saving, the
                // next tick reuses the immutable initial-window report.
                warn!(code = ?error.code, "weekly measurement report completion failed");
            }
        }
        if count < PAGE_SIZE {
            break;
        }
        tokio::task::yield_now().await;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Mutex;

    use geo_domain::{MeasurementPeriodWindow, ReportSchedule};

    use super::*;

    #[derive(Default)]
    struct Calls {
        discovery_cursors: Vec<Option<Uuid>>,
        due_cursors: Vec<DueCursor>,
        saves: Vec<(Uuid, DateTime<Utc>)>,
        reports: BTreeMap<Uuid, Uuid>,
        completed: BTreeSet<Uuid>,
        completion_attempts: Vec<(TenantScope, Uuid, Uuid)>,
        failed_save: bool,
        failed_completion: bool,
    }

    struct FakeStore {
        candidates: Vec<MeasurementWeeklySchedule>,
        calls: Mutex<Calls>,
    }

    #[async_trait]
    impl WeeklyReportStore for FakeStore {
        async fn discover(
            &self,
            _now: DateTime<Utc>,
            after: Option<Uuid>,
        ) -> Result<DiscoveryResults, AppError> {
            self.calls.lock().unwrap().discovery_cursors.push(after);
            Ok(self
                .candidates
                .iter()
                .filter(|candidate| after.is_none_or(|id| candidate.id > id))
                .take(PAGE_SIZE)
                .map(|candidate| {
                    let result = if candidate.id == Uuid::from_u128(1) {
                        Err(AppError::conflict("synthetic schedule failure"))
                    } else {
                        Ok(())
                    };
                    (candidate.id, result)
                })
                .collect())
        }

        async fn due(
            &self,
            now: DateTime<Utc>,
            after: DueCursor,
        ) -> Result<Vec<MeasurementWeeklySchedule>, AppError> {
            let mut calls = self.calls.lock().unwrap();
            calls.due_cursors.push(after);
            Ok(self
                .candidates
                .iter()
                .filter(|candidate| candidate.due_at <= now)
                .filter(|candidate| {
                    after.is_none_or(|cursor| (candidate.due_at, candidate.id) > cursor)
                })
                .filter(|candidate| !calls.completed.contains(&candidate.id))
                .take(PAGE_SIZE)
                .cloned()
                .collect())
        }

        async fn save(
            &self,
            candidate: &MeasurementWeeklySchedule,
            generated_at: DateTime<Utc>,
        ) -> Result<Uuid, AppError> {
            let mut calls = self.calls.lock().unwrap();
            calls.saves.push((candidate.id, generated_at));
            if candidate.id == Uuid::from_u128(1) && !calls.failed_save {
                calls.failed_save = true;
                return Err(AppError::conflict("synthetic save failure"));
            }
            Ok(*calls
                .reports
                .entry(candidate.id)
                .or_insert_with(Uuid::new_v4))
        }

        async fn complete(
            &self,
            scope: &TenantScope,
            schedule_id: Uuid,
            report_id: Uuid,
            _completed_at: DateTime<Utc>,
        ) -> Result<(), AppError> {
            let mut calls = self.calls.lock().unwrap();
            calls
                .completion_attempts
                .push((scope.clone(), schedule_id, report_id));
            if schedule_id == Uuid::from_u128(2) && !calls.failed_completion {
                calls.failed_completion = true;
                return Err(AppError::conflict("synthetic completion failure"));
            }
            calls.completed.insert(schedule_id);
            Ok(())
        }
    }

    fn candidate(id: u128) -> MeasurementWeeklySchedule {
        let end_at = DateTime::parse_from_rfc3339("2026-09-07T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        MeasurementWeeklySchedule {
            id: Uuid::from_u128(id),
            scope: TenantScope::new(
                Uuid::from_u128(1000).into(),
                Uuid::from_u128(2000).into(),
                Some(Uuid::from_u128(id + 3000).into()),
            ),
            window: MeasurementPeriodWindow {
                start_at: end_at - chrono::Duration::days(7),
                end_at,
                report_timezone: "UTC".to_owned(),
            },
            evidence_as_of: end_at + chrono::Duration::hours(8),
            report_at: end_at + chrono::Duration::hours(9),
            due_at: end_at + chrono::Duration::hours(9),
            report_schedule: ReportSchedule {
                cutoff_local_time: "08:00".to_owned(),
                ..ReportSchedule::default()
            },
            report_id: None,
        }
    }

    #[tokio::test]
    async fn failure_does_not_starve_later_projects_or_due_pages() {
        let store = FakeStore {
            candidates: (1..=102).map(candidate).collect(),
            calls: Mutex::default(),
        };
        let tick = Utc::now();
        scan_with(&store, tick).await;
        let calls = store.calls.lock().unwrap();
        assert_eq!(calls.discovery_cursors, [None, Some(Uuid::from_u128(100))]);
        assert_eq!(
            calls.due_cursors,
            [None, Some((candidate(100).due_at, Uuid::from_u128(100)))]
        );
        assert_eq!(calls.saves.len(), 102);
        assert_eq!(calls.completed.len(), 100);
        assert!(!calls.completed.contains(&Uuid::from_u128(1)));
        assert!(!calls.completed.contains(&Uuid::from_u128(2)));
        assert!(calls.completed.contains(&Uuid::from_u128(102)));
        assert!(
            calls
                .saves
                .iter()
                .all(|(_, generated_at)| *generated_at >= tick)
        );
        assert!(calls.completion_attempts.iter().all(|(scope, id, _)| {
            scope
                == &store
                    .candidates
                    .iter()
                    .find(|row| row.id == *id)
                    .unwrap()
                    .scope
        }));
    }

    #[tokio::test]
    async fn next_tick_retries_failed_save_and_reuses_saved_report_after_completion_failure() {
        let store = FakeStore {
            candidates: (1..=3).map(candidate).collect(),
            calls: Mutex::default(),
        };
        scan_with(&store, Utc::now()).await;
        let saved = store.calls.lock().unwrap().reports[&Uuid::from_u128(2)];
        scan_with(&store, Utc::now()).await;
        let calls = store.calls.lock().unwrap();
        assert_eq!(calls.completed.len(), 3);
        assert_eq!(calls.reports.len(), 3);
        assert_eq!(calls.reports[&Uuid::from_u128(2)], saved);
        assert_eq!(
            calls.saves.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            [1, 2, 3, 1, 2].map(Uuid::from_u128)
        );
        assert_eq!(
            calls
                .completion_attempts
                .iter()
                .filter(|(_, id, _)| *id == Uuid::from_u128(2))
                .map(|(_, _, id)| *id)
                .collect::<Vec<_>>(),
            [saved, saved]
        );
    }

    #[test]
    fn request_preserves_the_frozen_week_and_never_creates_a_correction() {
        let candidate = candidate(1);
        let request = report_request(&candidate);
        assert_eq!(request.start_at, candidate.window.start_at);
        assert_eq!(request.end_at, candidate.window.end_at);
        assert_eq!(request.report_timezone, candidate.window.report_timezone);
        assert_eq!(request.correction_of, None);
        assert_eq!(request.question_set, None);
    }
}
