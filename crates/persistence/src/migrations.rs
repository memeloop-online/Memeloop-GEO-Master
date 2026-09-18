use sqlx::migrate::Migrator;

/// The migration set is embedded into the binary at compile time. The source
/// SQL remains in the repository's top-level `migrations/` directory so it can
/// be reviewed and applied by the same versioned artifact as the service.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationMetadata {
    pub version: i64,
    pub description: &'static str,
}

const MIGRATION_METADATA: &[MigrationMetadata] = &[
    MigrationMetadata {
        version: 1,
        description: "initial schema",
    },
    MigrationMetadata {
        version: 2,
        description: "projects",
    },
    MigrationMetadata {
        version: 3,
        description: "auth sessions",
    },
    MigrationMetadata {
        version: 4,
        description: "tenant rls deferred until transaction-local scope wiring is complete",
    },
];

pub fn embedded_migrations() -> &'static Migrator {
    &MIGRATOR
}

pub fn migration_metadata() -> &'static [MigrationMetadata] {
    MIGRATION_METADATA
}

#[cfg(test)]
mod tests {
    use super::{embedded_migrations, migration_metadata};

    #[test]
    fn migration_metadata_matches_embedded_sql() {
        let metadata = migration_metadata();
        let embedded = embedded_migrations().iter().collect::<Vec<_>>();

        assert_eq!(metadata.len(), embedded.len());
        assert_eq!(metadata[0].version, embedded[0].version);
        assert_eq!(metadata[0].description, embedded[0].description);
    }
}
