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
    MigrationMetadata {
        version: 5,
        description: "atomic project start",
    },
    MigrationMetadata {
        version: 6,
        description: "knowledge import",
    },
    MigrationMetadata {
        version: 7,
        description: "agent state",
    },
    MigrationMetadata {
        version: 8,
        description: "document manifest dependencies",
    },
    MigrationMetadata {
        version: 9,
        description: "report snapshots",
    },
    MigrationMetadata {
        version: 10,
        description: "channel accounts",
    },
    MigrationMetadata {
        version: 11,
        description: "channel jobs",
    },
    MigrationMetadata {
        version: 12,
        description: "successor cycles",
    },
    MigrationMetadata {
        version: 13,
        description: "channel preflight reservations",
    },
    MigrationMetadata {
        version: 14,
        description: "document content",
    },
    MigrationMetadata {
        version: 15,
        description: "tenant model routes",
    },
    MigrationMetadata {
        version: 16,
        description: "content recovery",
    },
    MigrationMetadata {
        version: 17,
        description: "distribution execution",
    },
    MigrationMetadata {
        version: 18,
        description: "distribution jobs",
    },
    MigrationMetadata {
        version: 19,
        description: "autonomous cycle recovery",
    },
    MigrationMetadata {
        version: 20,
        description: "connector capabilities",
    },
    MigrationMetadata {
        version: 21,
        description: "saved connector verifications",
    },
    MigrationMetadata {
        version: 22,
        description: "agent queued scan",
    },
    MigrationMetadata {
        version: 23,
        description: "publication lookup",
    },
    MigrationMetadata {
        version: 24,
        description: "publication execution bindings",
    },
    MigrationMetadata {
        version: 25,
        description: "publication lookup report indexes",
    },
    MigrationMetadata {
        version: 26,
        description: "pdf page parse",
    },
    MigrationMetadata {
        version: 27,
        description: "content reuse",
    },
    MigrationMetadata {
        version: 28,
        description: "question sets",
    },
    MigrationMetadata {
        version: 29,
        description: "office parse",
    },
    MigrationMetadata {
        version: 30,
        description: "standalone measurements",
    },
    MigrationMetadata {
        version: 31,
        description: "knowledge text revisions",
    },
    MigrationMetadata {
        version: 32,
        description: "operator appearance",
    },
    MigrationMetadata {
        version: 33,
        description: "authored text invariants",
    },
    MigrationMetadata {
        version: 34,
        description: "content media bindings",
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
        for (metadata, embedded) in metadata.iter().zip(embedded) {
            assert_eq!(metadata.version, embedded.version);
            // Metadata may add operational caveats after the SQL filename.
            assert!(
                metadata
                    .description
                    .starts_with(embedded.description.as_ref())
            );
        }
    }
}
