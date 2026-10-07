//! Project-scoped, revocable authorization to use verified attachment images.
//! Byte inspection and project/knowledge authorization happen at the caller;
//! these bindings do not by themselves prove that uploaded bytes are an image.

use crate::{AppError, MAX_UPLOAD_BYTES, OperatorId, ProjectId, TenantId, TenantScope};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};
use tokio::sync::{OwnedRwLockReadGuard, RwLock};
use utoipa::ToSchema;
use uuid::Uuid;

/// A bounded decode input: image decoders must separately enforce their own limits.
const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_IMAGE_PIXELS: u64 = 100_000_000;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MediaObjectKey {
    pub object_id: Uuid,
    pub object_version: i64,
    pub sha256: String,
}

impl MediaObjectKey {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.object_id.is_nil() || self.object_version < 1 {
            return Err(AppError::invalid_request("invalid media object identity"));
        }
        if self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(AppError::invalid_request(
                "media sha256 must be 64 lowercase hexadecimal characters",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifiedImage {
    pub key: MediaObjectKey,
    pub media_type: String,
    pub byte_len: u64,
    pub width: u32,
    pub height: u32,
}

impl VerifiedImage {
    pub fn validate(&self) -> Result<(), AppError> {
        self.key.validate()?;
        if !matches!(
            self.media_type.as_str(),
            "image/png" | "image/jpeg" | "image/webp"
        ) {
            return Err(AppError::invalid_request("unsupported image media type"));
        }
        if self.byte_len == 0 || self.byte_len > MAX_UPLOAD_BYTES {
            return Err(AppError::invalid_request(
                "image byte length is out of range",
            ));
        }
        if self.width == 0
            || self.height == 0
            || self.width > MAX_IMAGE_DIMENSION
            || self.height > MAX_IMAGE_DIMENSION
            || u64::from(self.width) * u64::from(self.height) > MAX_IMAGE_PIXELS
        {
            return Err(AppError::invalid_request(
                "image dimensions are out of range",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContentMediaBindingState {
    Active,
    Withdrawn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ContentMediaBinding {
    pub binding_id: Uuid,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub image: VerifiedImage,
    pub state: ContentMediaBindingState,
    pub created_at: DateTime<Utc>,
    pub withdrawn_at: Option<DateTime<Utc>>,
}

#[async_trait]
pub trait ContentMediaRepository: Send + Sync {
    async fn create_binding(
        &self,
        scope: &TenantScope,
        image: VerifiedImage,
    ) -> Result<ContentMediaBinding, AppError>;

    async fn get_binding(
        &self,
        scope: &TenantScope,
        binding_id: Uuid,
    ) -> Result<Option<ContentMediaBinding>, AppError>;

    /// Active bindings in increasing binding ID order; `after` is exclusive.
    async fn list_bindings(
        &self,
        scope: &TenantScope,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ContentMediaBinding>, AppError>;

    async fn withdraw_binding(
        &self,
        scope: &TenantScope,
        binding_id: Uuid,
    ) -> Result<ContentMediaBinding, AppError>;
}

type ScopedObjectKey = (OperatorId, TenantId, ProjectId, MediaObjectKey);

#[derive(Debug, Default)]
struct MemoryContentMediaState {
    bindings: BTreeMap<Uuid, ContentMediaBinding>,
    by_object: HashMap<ScopedObjectKey, Uuid>,
}

/// The entire authorized batch remains protected from withdrawal until this
/// guard is dropped. Do not take a second media read lock while holding it.
pub struct ContentMediaReadGuard {
    hold: OwnedRwLockReadGuard<MemoryContentMediaState>,
}

impl ContentMediaReadGuard {
    /// Resolve every referenced image under the held read lock. The returned
    /// bindings are snapshots, not a substitute for retaining this guard.
    pub fn validate(
        &self,
        scope: &TenantScope,
        keys: &[MediaObjectKey],
    ) -> Result<Vec<ContentMediaBinding>, AppError> {
        let project_id = require_project(scope)?;
        let mut authorized = Vec::with_capacity(keys.len());
        for key in keys {
            key.validate()?;
            let index_key = (scope.operator_id, scope.tenant_id, project_id, key.clone());
            let binding = self
                .hold
                .by_object
                .get(&index_key)
                .and_then(|id| self.hold.bindings.get(id))
                .filter(|binding| {
                    binding.state == ContentMediaBindingState::Active
                        && binding.image.key == *key
                        && in_scope(binding, scope, project_id)
                })
                .ok_or_else(|| AppError::conflict("image is not authorized for content use"))?;
            authorized.push(binding.clone());
        }
        Ok(authorized)
    }
}

#[derive(Debug, Clone, Default)]
pub struct MemoryContentMediaRepository {
    state: Arc<RwLock<MemoryContentMediaState>>,
}

impl MemoryContentMediaRepository {
    /// Obtain this before the content-state write lock, even if the candidate
    /// media references can only be discovered after cloning content state.
    pub async fn read_guard(&self) -> ContentMediaReadGuard {
        ContentMediaReadGuard {
            hold: self.state.clone().read_owned().await,
        }
    }

    /// Hold a single media read lock across the caller's content-state commit.
    /// The caller is responsible for acquiring project and knowledge guards
    /// first; this method does not attempt either lock.
    pub async fn hold_authorized_images(
        &self,
        scope: &TenantScope,
        keys: &[MediaObjectKey],
    ) -> Result<ContentMediaReadGuard, AppError> {
        let hold = self.read_guard().await;
        hold.validate(scope, keys)?;
        Ok(hold)
    }
}

fn require_project(scope: &TenantScope) -> Result<ProjectId, AppError> {
    scope
        .project_id
        .ok_or_else(|| AppError::invalid_request("a project scope is required for media"))
}

fn in_scope(binding: &ContentMediaBinding, scope: &TenantScope, project_id: ProjectId) -> bool {
    binding.operator_id == scope.operator_id
        && binding.tenant_id == scope.tenant_id
        && binding.project_id == project_id
}

#[async_trait]
impl ContentMediaRepository for MemoryContentMediaRepository {
    async fn create_binding(
        &self,
        scope: &TenantScope,
        image: VerifiedImage,
    ) -> Result<ContentMediaBinding, AppError> {
        let project_id = require_project(scope)?;
        image.validate()?;
        let mut state = self.state.write().await;
        let key = (
            scope.operator_id,
            scope.tenant_id,
            project_id,
            image.key.clone(),
        );
        if let Some(id) = state.by_object.get(&key) {
            let existing = state
                .bindings
                .get(id)
                .ok_or_else(|| AppError::conflict("media binding index is inconsistent"))?;
            if existing.state == ContentMediaBindingState::Withdrawn {
                return Err(AppError::conflict("image binding was withdrawn"));
            }
            if existing.image != image {
                return Err(AppError::conflict(
                    "image metadata changed for existing binding",
                ));
            }
            return Ok(existing.clone());
        }
        let binding = ContentMediaBinding {
            binding_id: Uuid::new_v4(),
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            image,
            state: ContentMediaBindingState::Active,
            created_at: Utc::now(),
            withdrawn_at: None,
        };
        state.by_object.insert(key, binding.binding_id);
        state.bindings.insert(binding.binding_id, binding.clone());
        Ok(binding)
    }

    async fn get_binding(
        &self,
        scope: &TenantScope,
        binding_id: Uuid,
    ) -> Result<Option<ContentMediaBinding>, AppError> {
        let project_id = require_project(scope)?;
        let state = self.state.read().await;
        Ok(state
            .bindings
            .get(&binding_id)
            .filter(|binding| in_scope(binding, scope, project_id))
            .cloned())
    }

    async fn list_bindings(
        &self,
        scope: &TenantScope,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ContentMediaBinding>, AppError> {
        let project_id = require_project(scope)?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let state = self.state.read().await;
        Ok(state
            .bindings
            .iter()
            .filter(|(id, binding)| {
                after.is_none_or(|after_id| **id > after_id)
                    && in_scope(binding, scope, project_id)
                    && binding.state == ContentMediaBindingState::Active
            })
            .take(limit)
            .map(|(_, binding)| binding.clone())
            .collect())
    }

    async fn withdraw_binding(
        &self,
        scope: &TenantScope,
        binding_id: Uuid,
    ) -> Result<ContentMediaBinding, AppError> {
        let project_id = require_project(scope)?;
        let mut state = self.state.write().await;
        let binding = state
            .bindings
            .get_mut(&binding_id)
            .filter(|binding| in_scope(binding, scope, project_id))
            .ok_or_else(|| AppError::not_found("image binding not found"))?;
        if binding.state == ContentMediaBindingState::Active {
            binding.state = ContentMediaBindingState::Withdrawn;
            binding.withdrawn_at = Some(Utc::now());
        }
        Ok(binding.clone())
    }
}
