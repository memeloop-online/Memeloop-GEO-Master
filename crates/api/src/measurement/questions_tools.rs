//! Narrow question-set tool projection. Evaluation text never crosses this
//! model-facing seam, including after a write: receipts are metadata only.

use async_trait::async_trait;
use geo_domain::{AppError, CreateQuestionSet, QuestionPurpose, TenantScope};
use geo_worker::{
    QuestionDiscoverRequest, QuestionDiscoveryItem, QuestionDiscoveryPage, QuestionReference,
    QuestionReviseRequest, QuestionWriteReceipt,
};
use uuid::Uuid;

use crate::{AppState, questions};

#[async_trait]
pub(crate) trait QuestionToolService: Send + Sync {
    async fn discover(
        &self,
        scope: &TenantScope,
        request: QuestionDiscoverRequest,
    ) -> Result<QuestionDiscoveryPage, AppError>;
    async fn create(
        &self,
        scope: &TenantScope,
        request: CreateQuestionSet,
    ) -> Result<QuestionWriteReceipt, AppError>;
    async fn revise(
        &self,
        scope: &TenantScope,
        request: QuestionReviseRequest,
    ) -> Result<QuestionWriteReceipt, AppError>;
}

fn receipt(version: geo_domain::QuestionSetVersion) -> QuestionWriteReceipt {
    QuestionWriteReceipt {
        question_set_id: version.question_set_id,
        question_set_version_id: version.id,
        revision: version.revision,
        optimization_count: version.optimization_count,
        evaluation_count: version.evaluation_count,
    }
}

#[async_trait]
impl QuestionToolService for AppState {
    async fn discover(
        &self,
        scope: &TenantScope,
        request: QuestionDiscoverRequest,
    ) -> Result<QuestionDiscoveryPage, AppError> {
        request.validate().map_err(AppError::invalid_request)?;
        let limit = request.limit.unwrap_or(25);
        let mut result = QuestionDiscoveryPage {
            sets: Vec::new(),
            versions: Vec::new(),
            questions: Vec::new(),
            next_cursor: None,
        };
        if let (Some(set_id), Some(version_id)) =
            (request.question_set_id, request.question_set_version_id)
        {
            let version = questions::get_version(self, scope, set_id, version_id).await?;
            let after = request
                .cursor
                .as_deref()
                .map(|cursor| {
                    cursor
                        .parse::<usize>()
                        .map_err(|_| AppError::invalid_request("invalid question cursor"))
                })
                .transpose()?
                .unwrap_or(0);
            if after > version.questions.len() {
                return Err(AppError::invalid_request(
                    "question cursor exceeds version length",
                ));
            }
            let end = after
                .saturating_add(limit as usize)
                .min(version.questions.len());
            result.questions = version.questions[after..end]
                .iter()
                .map(|revision| QuestionDiscoveryItem {
                    reference: QuestionReference {
                        question_set_id: set_id,
                        question_set_version_id: version_id,
                        question_id: revision.question_id,
                        question_revision_id: revision.id,
                    },
                    purpose: revision.purpose,
                    optimization_text: (revision.purpose == QuestionPurpose::Optimization)
                        .then(|| revision.text.clone()),
                })
                .collect();
            result.next_cursor = (end < version.questions.len()).then(|| end.to_string());
        } else if let Some(set_id) = request.question_set_id {
            let after_revision = request
                .cursor
                .as_deref()
                .map(|cursor| {
                    cursor
                        .parse::<u32>()
                        .map_err(|_| AppError::invalid_request("invalid version cursor"))
                })
                .transpose()?;
            let page = questions::list_versions(self, scope, set_id, after_revision, limit).await?;
            result.versions = page.items;
            result.next_cursor = page.next_cursor.map(|cursor| cursor.to_string());
        } else {
            let after = request
                .cursor
                .as_deref()
                .map(|cursor| {
                    Uuid::parse_str(cursor)
                        .map_err(|_| AppError::invalid_request("invalid question-set cursor"))
                })
                .transpose()?;
            let page = questions::list_sets(self, scope, after, limit).await?;
            result.sets = page.items;
            result.next_cursor = page.next_cursor.map(|cursor| cursor.to_string());
        }
        result
            .validate_for(&request)
            .map_err(AppError::invalid_request)?;
        Ok(result)
    }

    async fn create(
        &self,
        scope: &TenantScope,
        request: CreateQuestionSet,
    ) -> Result<QuestionWriteReceipt, AppError> {
        geo_worker::host::validate_question_create(&request).map_err(AppError::invalid_request)?;
        let result = receipt(questions::create_set(self, scope, request).await?);
        result.validate().map_err(AppError::invalid_request)?;
        Ok(result)
    }

    async fn revise(
        &self,
        scope: &TenantScope,
        request: QuestionReviseRequest,
    ) -> Result<QuestionWriteReceipt, AppError> {
        request.validate().map_err(AppError::invalid_request)?;
        let result = receipt(
            questions::revise_set(self, scope, request.question_set_id, request.command).await?,
        );
        result.validate().map_err(AppError::invalid_request)?;
        Ok(result)
    }
}
