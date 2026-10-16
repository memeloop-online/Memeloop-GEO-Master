//! Explicit operator/tenant model grants provisioned by a trusted operator.
//! No inference request can choose a credential, subject, or gateway.

use crate::TenantScope;
use std::fmt;
use uuid::Uuid;

#[derive(Clone, PartialEq, Eq)]
pub struct ModelRouteGrant {
    pub route_id: Uuid,
    pub scope: TenantScope,
    pub model: String,
    pub is_default: bool,
    pub enabled: bool,
    pub tenant_external_id: String,
    pub principal_external_id: String,
    pub key_id: Uuid,
    /// The explicitly approved Token Center credential generation.
    pub credential_generation: i64,
}

impl fmt::Debug for ModelRouteGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ModelRouteGrant(***)")
    }
}

impl ModelRouteGrant {
    pub fn valid(&self) -> bool {
        !self.model.trim().is_empty()
            && self.model.len() <= 256
            && !self.model.contains("://")
            && self.key_id != Uuid::nil()
            && self.credential_generation > 0
            && valid_external_identity(&self.tenant_external_id)
            && valid_external_identity(&self.principal_external_id)
    }
}

fn valid_external_identity(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 200 && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_grant_rejects_endpoint_model_and_invalid_subject() {
        let mut grant = ModelRouteGrant {
            route_id: Uuid::new_v4(),
            scope: TenantScope::new(Uuid::new_v4().into(), Uuid::new_v4().into(), None),
            model: "approved-model".into(),
            is_default: true,
            enabled: true,
            tenant_external_id: "external-tenant".into(),
            principal_external_id: "external-principal".into(),
            key_id: Uuid::new_v4(),
            credential_generation: 1,
        };
        assert!(grant.valid());
        grant.model = "https://untrusted.invalid/v1".into();
        assert!(!grant.valid());
        grant.model = "approved-model".into();
        grant.principal_external_id = "\n".into();
        assert!(!grant.valid());
    }
}
