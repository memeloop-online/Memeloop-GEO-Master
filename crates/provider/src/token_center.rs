//! Explicit, tenant-bound Token Center credential resolution.
//!
//! The application constructs mappings from authenticated Rust-side scope.
//! Neither JavaScript nor completion requests can choose a Token Center key.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, Response, redirect::Policy};
use serde::Deserialize;
use url::Url;

use crate::{ProviderError, ResolvedToken, SecretRef, TokenCenter};

const MAX_CREDENTIAL_RESPONSE_BYTES: usize = 64 * 1024;

/// A pre-provisioned binding supplied by trusted application configuration.
#[derive(Clone)]
pub struct TokenCenterKeyMapping {
    secret_ref: SecretRef,
    tenant_external_id: String,
    principal_external_id: String,
    key_id: String,
    expected_generation: Option<i64>,
}

impl fmt::Debug for TokenCenterKeyMapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TokenCenterKeyMapping(***)")
    }
}

impl TokenCenterKeyMapping {
    pub fn new(
        secret_ref: SecretRef,
        tenant_external_id: impl Into<String>,
        principal_external_id: impl Into<String>,
        key_id: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        let tenant_external_id = tenant_external_id.into();
        let principal_external_id = principal_external_id.into();
        let key_id = key_id.into();
        if !valid_identity(&tenant_external_id)
            || !valid_identity(&principal_external_id)
            || !valid_uuid(&key_id)
        {
            return Err(ProviderError::InvalidRequest(
                "invalid Token Center credential mapping".into(),
            ));
        }
        Ok(Self {
            secret_ref,
            tenant_external_id,
            principal_external_id,
            key_id,
            expected_generation: None,
        })
    }

    /// Pins a deployment-approved generation. Rotation fails closed until
    /// trusted provisioning updates the durable mapping.
    pub fn with_generation(mut self, generation: i64) -> Result<Self, ProviderError> {
        if generation < 1 {
            return Err(ProviderError::InvalidRequest(
                "invalid Token Center credential generation".into(),
            ));
        }
        self.expected_generation = Some(generation);
        Ok(self)
    }
}

fn valid_identity(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 200 && !value.chars().any(char::is_control)
}

fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

/// HTTP adapter for an injected internal endpoint and `keys:read`/`keys:write`
/// service token. The service token is never used as a downstream inference key.
pub struct HttpTokenCenter {
    base_url: Url,
    service_token: String,
    mappings: BTreeMap<String, TokenCenterKeyMapping>,
    client: Client,
}

impl fmt::Debug for HttpTokenCenter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HttpTokenCenter(***)")
    }
}

impl HttpTokenCenter {
    pub fn new(
        base_url: impl AsRef<str>,
        service_token: impl Into<String>,
        mappings: impl IntoIterator<Item = TokenCenterKeyMapping>,
    ) -> Result<Self, ProviderError> {
        let mut base_url = Url::parse(base_url.as_ref())
            .map_err(|_| ProviderError::InvalidRequest("invalid Token Center endpoint".into()))?;
        if !matches!(base_url.scheme(), "http" | "https")
            || base_url.host_str().is_none()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(ProviderError::InvalidRequest(
                "invalid Token Center endpoint".into(),
            ));
        }
        let service_token = service_token.into();
        if service_token.trim().is_empty()
            || service_token.contains('\r')
            || service_token.contains('\n')
        {
            return Err(ProviderError::InvalidRequest(
                "invalid Token Center service token".into(),
            ));
        }
        if !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path());
            base_url.set_path(&path);
        }
        let mut bindings = BTreeMap::new();
        for mapping in mappings {
            if bindings
                .insert(mapping.secret_ref.as_str().to_owned(), mapping)
                .is_some()
            {
                return Err(ProviderError::InvalidRequest(
                    "duplicate Token Center secret reference".into(),
                ));
            }
        }
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| {
                ProviderError::Transport("Token Center client initialization failed".into())
            })?;
        Ok(Self {
            base_url,
            service_token,
            mappings: bindings,
            client,
        })
    }

    fn key_url(&self, key_id: &str, copy: bool) -> Result<Url, ProviderError> {
        let suffix = if copy {
            format!("internal/v1/keys/{key_id}/copy")
        } else {
            "internal/v1/keys".to_owned()
        };
        self.base_url
            .join(&suffix)
            .map_err(|_| ProviderError::Transport("Token Center endpoint unavailable".into()))
    }
}

#[derive(Deserialize)]
struct KeyMetadata {
    key_id: String,
    tenant_external_id: String,
    principal_external_id: String,
    status: String,
    credential_generation: i64,
    credential_copy_available: bool,
}

#[derive(Deserialize)]
struct CopiedCredential {
    key_id: String,
    credential_generation: i64,
    key: String,
}

async fn bounded_body(mut response: Response) -> Result<Vec<u8>, ProviderError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_CREDENTIAL_RESPONSE_BYTES as u64)
    {
        return Err(unavailable());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if chunk.len() > MAX_CREDENTIAL_RESPONSE_BYTES - body.len() {
            return Err(unavailable());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn unavailable() -> ProviderError {
    ProviderError::TokenUnavailable("Token Center credential unavailable".into())
}

#[async_trait]
impl TokenCenter for HttpTokenCenter {
    async fn resolve(&self, secret_ref: &SecretRef) -> Result<ResolvedToken, ProviderError> {
        let mapping = self
            .mappings
            .get(secret_ref.as_str())
            .ok_or_else(unavailable)?;
        let metadata_response = self
            .client
            .get(self.key_url(&mapping.key_id, false)?)
            .bearer_auth(&self.service_token)
            .query(&[
                ("tenant_external_id", mapping.tenant_external_id.as_str()),
                (
                    "principal_external_id",
                    mapping.principal_external_id.as_str(),
                ),
                ("key_id", mapping.key_id.as_str()),
                ("limit", "1"),
            ])
            .send()
            .await
            .map_err(|_| unavailable())?;
        if !metadata_response.status().is_success() {
            return Err(unavailable());
        }
        let metadata: Vec<KeyMetadata> =
            serde_json::from_slice(&bounded_body(metadata_response).await?)
                .map_err(|_| unavailable())?;
        let [metadata] = metadata.as_slice() else {
            return Err(unavailable());
        };
        if metadata.key_id != mapping.key_id
            || metadata.tenant_external_id != mapping.tenant_external_id
            || metadata.principal_external_id != mapping.principal_external_id
            || metadata.status != "active"
            || metadata.credential_generation < 1
            || mapping
                .expected_generation
                .is_some_and(|generation| metadata.credential_generation != generation)
            || !metadata.credential_copy_available
        {
            return Err(unavailable());
        }

        // No issuance and no cache: a rotation/revocation takes effect on the
        // next resolution. The copy endpoint also fences status, generation,
        // service scope and tenant atomically in Token Center.
        let copied_response = self
            .client
            .post(self.key_url(&mapping.key_id, true)?)
            .bearer_auth(&self.service_token)
            .send()
            .await
            .map_err(|_| unavailable())?;
        if !copied_response.status().is_success() {
            return Err(unavailable());
        }
        let copied: CopiedCredential =
            serde_json::from_slice(&bounded_body(copied_response).await?)
                .map_err(|_| unavailable())?;
        if copied.key_id != mapping.key_id
            || copied.credential_generation != metadata.credential_generation
        {
            return Err(unavailable());
        }
        ResolvedToken::new(copied.key).map_err(|_| unavailable())
    }
}
