use chrono::Utc;
use geo_domain::{
    ChannelImportRecord, ChannelImportRow, ChannelImportStatus, ChannelRepository, ChannelSecret,
    ChannelStatus, OperatorId, PoolAccount, PoolGroup,
};
use uuid::Uuid;

pub async fn verify(
    repository: &dyn ChannelRepository,
    operator: OperatorId,
    other_operator: OperatorId,
) -> String {
    let key = Uuid::new_v4().to_string();
    let group_id = Uuid::new_v4();
    let id = Uuid::new_v4();
    let now = Utc::now();
    let account = PoolAccount {
        account_id: id,
        platform: "zhihu".into(),
        group_id: Some(group_id),
        status: ChannelStatus::NeedsLogin,
        display_name: Some("Synthetic account".into()),
        platform_account_id: None,
        avatar_url: None,
        enabled: true,
        proxy_configured: false,
        proxy_server: None,
        created_at: now,
        updated_at: now,
    };
    // Repository tests use opaque envelope bytes. AEAD is verified separately
    // at the trusted service boundary that owns the encryption key.
    let record = ChannelImportRecord {
        secret_id: id,
        encrypted_input: ChannelSecret::new(vec![42; 64]),
        row: ChannelImportRow {
            row_key: "row-1".into(),
            ordinal: 0,
            account_id: Some(id),
            status: ChannelImportStatus::NeedsLogin,
            error_code: None,
            masked_username: Some("***".into()),
            display_alias: Some("Synthetic account".into()),
        },
    };
    let failed = repository
        .save_import_row(operator, &key, None, record.clone(), Some(account.clone()))
        .await
        .unwrap();
    assert_eq!(failed.row.status, ChannelImportStatus::Failed);
    assert_eq!(failed.row.error_code.as_deref(), Some("group_not_found"));
    assert!(repository.get_pool_account(operator, id).await.is_err());
    repository
        .save_pool_group(
            operator,
            PoolGroup {
                group_id,
                name: "Synthetic group".into(),
                created_at: now,
            },
        )
        .await
        .unwrap();
    let replay = repository
        .save_import_row(operator, &key, None, record.clone(), Some(account.clone()))
        .await
        .unwrap();
    assert_eq!(replay.row.status, ChannelImportStatus::Failed);
    let mismatch = repository
        .save_import_row(
            operator,
            &key,
            Some(ChannelSecret::new(vec![9])),
            record.clone(),
            Some(account.clone()),
        )
        .await
        .unwrap();
    assert_eq!(mismatch.row.status, ChannelImportStatus::Failed);
    let (a, b) = tokio::join!(
        repository.save_import_row(
            operator,
            &key,
            Some(failed.encrypted_input.clone()),
            record.clone(),
            Some(account.clone())
        ),
        repository.save_import_row(
            operator,
            &key,
            Some(failed.encrypted_input.clone()),
            record.clone(),
            Some(account.clone())
        ),
    );
    assert_eq!(a.unwrap().row.account_id, Some(id));
    assert_eq!(b.unwrap().row.account_id, Some(id));
    let rows = repository.list_import_rows(operator, &key).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].encrypted_input.encrypted_bytes(), &[42; 64]);
    let stored = repository.get_pool_account(operator, id).await.unwrap();
    assert!(stored.session.is_none());
    assert!(stored.account.platform_account_id.is_none());
    assert!(
        repository
            .list_import_rows(Uuid::new_v4().into(), &key)
            .await
            .unwrap()
            .is_empty()
    );
    let mut collision = record.clone();
    collision.secret_id = Uuid::new_v4();
    collision.encrypted_input = ChannelSecret::new(vec![43; 64]);
    let mut other_account = account.clone();
    other_account.account_id = collision.secret_id;
    collision.row.account_id = Some(other_account.account_id);
    let returned = repository
        .save_import_row(operator, &key, None, collision, Some(other_account.clone()))
        .await
        .unwrap();
    assert_eq!(returned.row.account_id, Some(id));
    assert!(
        repository
            .get_pool_account(operator, other_account.account_id)
            .await
            .is_err()
    );
    // New repository instances and row lookups reuse the same durable outcome;
    // deleting an account does not silently recreate it via an import replay.
    repository.delete_pool_account(operator, id).await.unwrap();
    let replay = repository
        .save_import_row(operator, &key, None, record, Some(account))
        .await
        .unwrap();
    assert_eq!(replay.row.account_id, Some(id));
    assert!(repository.get_pool_account(operator, id).await.is_err());

    // Racing first inserts with different candidates return the same winner
    // and leave no losing candidate account behind.
    let race_key = Uuid::new_v4().to_string();
    let mut first_record = replay.clone();
    let mut first_account = other_account.clone();
    first_account.group_id = None;
    first_record.secret_id = first_account.account_id;
    first_record.row.account_id = Some(first_account.account_id);
    let mut second_record = first_record.clone();
    let mut second_account = first_account.clone();
    second_account.account_id = Uuid::new_v4();
    second_record.secret_id = second_account.account_id;
    second_record.row.account_id = Some(second_account.account_id);
    second_record.encrypted_input = ChannelSecret::new(vec![44; 64]);
    let (a, b) = tokio::join!(
        repository.save_import_row(
            operator,
            &race_key,
            None,
            first_record.clone(),
            Some(first_account.clone())
        ),
        repository.save_import_row(
            operator,
            &race_key,
            None,
            second_record,
            Some(second_account.clone())
        ),
    );
    assert_eq!(a.unwrap().row.account_id, b.unwrap().row.account_id);
    assert_ne!(
        repository
            .get_pool_account(operator, first_account.account_id)
            .await
            .is_ok(),
        repository
            .get_pool_account(operator, second_account.account_id)
            .await
            .is_ok(),
    );
    assert_eq!(
        repository
            .list_import_rows(operator, &race_key)
            .await
            .unwrap()
            .len(),
        1
    );

    let foreign_group = Uuid::new_v4();
    repository
        .save_pool_group(
            other_operator,
            PoolGroup {
                group_id: foreign_group,
                name: "Other operator group".into(),
                created_at: now,
            },
        )
        .await
        .unwrap();
    let mut foreign_account = first_account.clone();
    foreign_account.account_id = Uuid::new_v4();
    foreign_account.group_id = Some(foreign_group);
    let mut foreign_record = first_record.clone();
    foreign_record.secret_id = foreign_account.account_id;
    foreign_record.row.account_id = Some(foreign_account.account_id);
    let foreign = repository
        .save_import_row(
            operator,
            &Uuid::new_v4().to_string(),
            None,
            foreign_record,
            Some(foreign_account.clone()),
        )
        .await
        .unwrap();
    assert_eq!(foreign.row.error_code.as_deref(), Some("group_not_found"));
    assert!(
        repository
            .get_pool_account(operator, foreign_account.account_id)
            .await
            .is_err()
    );

    // The cap covers concurrent appends, not only request-level validation.
    let limit_key = Uuid::new_v4().to_string();
    let mut failed_record = first_record;
    failed_record.row.account_id = None;
    failed_record.row.status = ChannelImportStatus::Failed;
    failed_record.row.error_code = Some("unsupported_platform".into());
    for ordinal in 0..99 {
        failed_record.row.row_key = format!("row-{ordinal}");
        failed_record.row.ordinal = ordinal;
        repository
            .save_import_row(operator, &limit_key, None, failed_record.clone(), None)
            .await
            .unwrap();
    }
    let mut left = failed_record.clone();
    left.row.row_key = "row-left".into();
    let mut right = failed_record;
    right.row.row_key = "row-right".into();
    let (a, b) = tokio::join!(
        repository.save_import_row(operator, &limit_key, None, left, None),
        repository.save_import_row(operator, &limit_key, None, right, None),
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(
        repository
            .list_import_rows(operator, &limit_key)
            .await
            .unwrap()
            .len(),
        100
    );
    key
}
