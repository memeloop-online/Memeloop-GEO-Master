use chrono::{Duration, Utc};
use geo_domain::{
    AuthRepository, DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_USER_EMAIL, MemoryAuthRepository, Session,
    UserId, hash_token,
};
use uuid::Uuid;

#[test]
fn persistent_sessions_have_no_server_deadline_but_remain_revocable() {
    let credentials = Session::persistent(DEVELOPMENT_OPERATOR_ID, UserId::from(Uuid::new_v4()));
    let mut session = credentials.session;
    assert_eq!(session.expires_at, None);
    assert_eq!(session.token_hash(), hash_token(&credentials.token));
    for elapsed in [
        Duration::hours(13),
        Duration::days(401),
        Duration::days(3650),
    ] {
        assert!(session.is_active_at(session.created_at + elapsed));
    }
    session.revoked_at = Some(Utc::now());
    assert!(!session.is_active_at(Utc::now() + Duration::days(1)));
}

#[test]
fn finite_sessions_keep_their_original_expiration_boundary() {
    let session = Session::new(
        DEVELOPMENT_OPERATOR_ID,
        UserId::from(Uuid::new_v4()),
        Duration::hours(12),
    )
    .session;
    let expiry = session.expires_at.unwrap();
    assert!(session.is_active_at(expiry - Duration::milliseconds(1)));
    assert!(!session.is_active_at(expiry));
    assert!(!session.is_active_at(expiry + Duration::hours(1)));
}

#[tokio::test]
async fn memory_persistent_sessions_preserve_scope_and_revocation() {
    let repo = MemoryAuthRepository::development_with_password("test-password");
    let identity = repo
        .authenticate(
            DEVELOPMENT_OPERATOR_ID,
            DEVELOPMENT_USER_EMAIL,
            "test-password",
        )
        .await
        .unwrap()
        .unwrap();
    let credentials = repo
        .create_persistent_session(DEVELOPMENT_OPERATOR_ID, identity.user.id)
        .await
        .unwrap();
    let found = repo
        .find_session(DEVELOPMENT_OPERATOR_ID, &credentials.token)
        .await
        .unwrap()
        .unwrap();
    assert!(found.is_active_at(Utc::now() + Duration::days(3650)));
    assert!(
        repo.find_session(Uuid::new_v4().into(), &credentials.token)
            .await
            .unwrap()
            .is_none()
    );
    repo.revoke_session(DEVELOPMENT_OPERATOR_ID, found.id)
        .await
        .unwrap();
    let revoked = repo
        .find_session(DEVELOPMENT_OPERATOR_ID, &credentials.token)
        .await
        .unwrap()
        .unwrap();
    assert!(!revoked.is_active_at(Utc::now()));
}
