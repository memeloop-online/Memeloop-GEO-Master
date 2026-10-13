use geo_domain::{AuthRepository, OperatorId, Role};
use geo_persistence::{
    Database, DatabaseConfig, PgAuthRepository,
    bootstrap::{BootstrapError, BootstrapIdentity, BootstrapOutcome, bootstrap_identity},
};
use uuid::Uuid;

#[derive(Clone)]
struct Fixture {
    operator_id: Uuid,
    tenant_id: Uuid,
    user_id: Uuid,
    operator_slug: String,
    tenant_slug: String,
    host: String,
    email: String,
    password: String,
}

impl Fixture {
    fn new() -> Self {
        let operator_id = Uuid::new_v4();
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        Self {
            operator_id,
            tenant_id,
            user_id,
            operator_slug: format!("op-{}", operator_id.simple()),
            tenant_slug: format!("tenant-{}", tenant_id.simple()),
            host: format!("{}.example.invalid", operator_id.simple()),
            email: format!("{}@example.invalid", user_id.simple()),
            password: format!("{}{}", Uuid::new_v4(), Uuid::new_v4()),
        }
    }

    fn input(&self, role: &str) -> BootstrapIdentity {
        BootstrapIdentity::new(
            &self.operator_id.to_string(),
            &self.operator_slug,
            "Bootstrap operator",
            &self.host,
            &self.tenant_id.to_string(),
            &self.tenant_slug,
            "Bootstrap tenant",
            &self.user_id.to_string(),
            &self.email,
            "Bootstrap administrator",
            self.password.clone(),
            role,
        )
        .unwrap()
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn persistent_and_finite_sessions_round_trip_without_reviving_expired_sessions() {
    let config = DatabaseConfig::from_url(
        std::env::var("GEO_TEST_DATABASE_URL").expect("test database URL"),
    )
    .unwrap();
    let database = Database::connect_and_migrate(&config).await.unwrap();
    let fixture = Fixture::new();
    bootstrap_identity(database.pool(), &fixture.input("customer_admin"))
        .await
        .unwrap();
    let repo = PgAuthRepository::from_database(&database);
    let operator = fixture.operator_id.into();
    let user = fixture.user_id.into();
    let finite = repo
        .create_session(operator, user, chrono::Duration::hours(12))
        .await
        .unwrap();
    let expired = repo
        .create_session(operator, user, chrono::Duration::hours(-1))
        .await
        .unwrap();
    let persistent = repo
        .create_persistent_session(operator, user)
        .await
        .unwrap();
    let stored_hash: String =
        sqlx::query_scalar("SELECT token_hash FROM sessions WHERE session_id=$1")
            .bind(persistent.session.id.as_uuid())
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(stored_hash, geo_domain::hash_token(&persistent.token));
    assert_ne!(stored_hash, persistent.token);
    database.migrate().await.unwrap();
    // A new connection/repository reads both legacy finite timestamps and NULL.
    let reopened = Database::connect_and_migrate(&config).await.unwrap();
    let repo = PgAuthRepository::from_database(&reopened);
    let read_finite = repo
        .find_session(operator, &finite.token)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        read_finite.expires_at.unwrap().timestamp_micros(),
        finite.session.expires_at.unwrap().timestamp_micros(),
    );
    assert!(
        !repo
            .find_session(operator, &expired.token)
            .await
            .unwrap()
            .unwrap()
            .is_active_at(chrono::Utc::now())
    );
    let read_persistent = repo
        .find_session(operator, &persistent.token)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read_persistent.expires_at, None);
    assert!(read_persistent.is_active_at(chrono::Utc::now() + chrono::Duration::days(3650)));
    assert!(
        repo.find_session(Uuid::new_v4().into(), &persistent.token)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.create_persistent_session(Uuid::new_v4().into(), user)
            .await
            .is_err()
    );
    repo.revoke_session(operator, persistent.session.id)
        .await
        .unwrap();
    let revoked = repo
        .find_session(operator, &persistent.token)
        .await
        .unwrap()
        .unwrap();
    assert!(!revoked.is_active_at(chrono::Utc::now()));
    assert_eq!(revoked.expires_at, None);
    sqlx::query("UPDATE users SET active=false WHERE user_id=$1")
        .bind(fixture.user_id)
        .execute(reopened.pool())
        .await
        .unwrap();
    assert!(
        !repo
            .find_user(operator, user)
            .await
            .unwrap()
            .unwrap()
            .active
    );
    assert!(
        repo.create_persistent_session(operator, user)
            .await
            .is_err()
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn transaction_replay_and_conflicts_preserve_identity() {
    let config = DatabaseConfig::from_url(
        std::env::var("GEO_TEST_DATABASE_URL").expect("test database URL"),
    )
    .unwrap();
    let database = Database::connect_and_migrate(&config).await.unwrap();
    let pool = database.pool();
    let fixture = Fixture::new();
    let input = fixture.input("resource_admin");
    assert_eq!(
        bootstrap_identity(pool, &input).await.unwrap(),
        BootstrapOutcome::Created
    );
    let auth = PgAuthRepository::from_database(&database);
    assert_eq!(
        auth.operator_for_host(&fixture.host)
            .await
            .unwrap()
            .unwrap()
            .id,
        OperatorId::from(fixture.operator_id)
    );
    let login = auth
        .authenticate(
            OperatorId::from(fixture.operator_id),
            &fixture.email,
            &fixture.password,
        )
        .await
        .unwrap()
        .expect("login works with deployer-supplied secret");
    assert_eq!(login.memberships.len(), 1);
    assert_eq!(login.memberships[0].role, Role::ResourceAdmin);
    let initial_hash: String =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE user_id=$1")
            .bind(fixture.user_id)
            .fetch_one(pool)
            .await
            .unwrap();
    let initial_membership: Uuid =
        sqlx::query_scalar("SELECT membership_id FROM memberships WHERE user_id=$1")
            .bind(fixture.user_id)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(
        bootstrap_identity(pool, &input).await.unwrap(),
        BootstrapOutcome::Unchanged
    );
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE user_id=$1")
        .bind(fixture.user_id)
        .fetch_one(pool)
        .await
        .unwrap();
    let membership: Uuid =
        sqlx::query_scalar("SELECT membership_id FROM memberships WHERE user_id=$1")
            .bind(fixture.user_id)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(hash, initial_hash, "replay does not rotate credentials");
    assert_eq!(
        membership, initial_membership,
        "replay preserves membership"
    );

    assert_eq!(
        bootstrap_identity(pool, &fixture.input("customer_admin")).await,
        Err(BootstrapError::Conflict),
        "bootstrap cannot upgrade or switch the role"
    );
    let mut changed_password = fixture.clone();
    changed_password.password = format!("{}{}", Uuid::new_v4(), Uuid::new_v4());
    assert_eq!(
        bootstrap_identity(pool, &changed_password.input("resource_admin")).await,
        Err(BootstrapError::Conflict),
        "bootstrap cannot rotate credentials"
    );
    // Another identity cannot claim an existing Host. Its operator must not
    // survive the failed transaction.
    let mut contender = Fixture::new();
    contender.host = fixture.host.clone();
    assert_eq!(
        bootstrap_identity(pool, &contender.input("customer_admin")).await,
        Err(BootstrapError::Conflict)
    );
    let leaked: i64 = sqlx::query_scalar("SELECT count(*) FROM operators WHERE operator_id=$1")
        .bind(contender.operator_id)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(leaked, 0);

    sqlx::query("UPDATE memberships SET active=false WHERE user_id=$1")
        .bind(fixture.user_id)
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(
        bootstrap_identity(pool, &fixture.input("resource_admin")).await,
        Err(BootstrapError::Conflict),
        "bootstrap never reactivates membership"
    );
}
