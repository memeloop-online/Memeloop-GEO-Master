use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, str::FromStr};
use utoipa::ToSchema;
use uuid::Uuid;

macro_rules! scoped_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, ToSchema)]
        #[schema(value_type = String)]
        pub struct $name(pub Uuid);

        impl $name {
            pub const fn new(value: Uuid) -> Self {
                Self(value)
            }

            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }

        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value.parse().map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

scoped_id!(OperatorId);
scoped_id!(TenantId);
scoped_id!(ProjectId);

/// The operator and tenant boundary used for every tenant-owned operation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
pub struct TenantScopeId {
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
}

impl TenantScopeId {
    pub const fn new(operator_id: OperatorId, tenant_id: TenantId) -> Self {
        Self {
            operator_id,
            tenant_id,
        }
    }

    pub fn storage_key(&self) -> String {
        format!("{}:{}", self.operator_id, self.tenant_id)
    }
}

/// A tenant scope optionally narrowed to one project.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
pub struct TenantScope {
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
}

impl TenantScope {
    pub const fn new(
        operator_id: OperatorId,
        tenant_id: TenantId,
        project_id: Option<ProjectId>,
    ) -> Self {
        Self {
            operator_id,
            tenant_id,
            project_id,
        }
    }

    pub const fn tenant_id(&self) -> TenantScopeId {
        TenantScopeId::new(self.operator_id, self.tenant_id)
    }

    /// Returns whether an event or resource belongs to the requested scope.
    /// A scope without a project sees all projects in the same tenant.
    pub fn contains(&self, candidate: &Self) -> bool {
        self.operator_id == candidate.operator_id
            && self.tenant_id == candidate.tenant_id
            && self
                .project_id
                .is_none_or(|project_id| candidate.project_id == Some(project_id))
    }

    pub fn storage_key(&self) -> String {
        match self.project_id {
            Some(project_id) => format!("{}:{}:{}", self.operator_id, self.tenant_id, project_id),
            None => self.tenant_id().storage_key(),
        }
    }
}
