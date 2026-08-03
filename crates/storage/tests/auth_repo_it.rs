//! Integration test for the auth repository against a real Postgres.
//!
//! Self-skips when no database is reachable (e.g. Docker not running), so it is safe in any
//! environment and runs for real in CI / once `docker compose up` is done. Point it at a
//! database with `TRUST_TEST_DATABASE_URL` (defaults to the dev compose Postgres).

use trust_storage::{auth_repo, Db};
use uuid::Uuid;

fn database_url() -> String {
    std::env::var("TRUST_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://trust:trust@localhost:5432/trust".to_string())
}

#[tokio::test]
async fn account_device_session_lifecycle() {
    let db = Db::connect_lazy(&database_url(), 4).expect("build pool");
    if db.ping().await.is_err() {
        eprintln!("skipping auth_repo_it: no database reachable at {}", database_url());
        return;
    }
    db.migrate().await.expect("migrate");
    let pool = &db.pool;

    // Unique username per run so repeated runs don't collide.
    let username = format!("it-user-{}", Uuid::now_v7());

    let account = auth_repo::create_account(pool, Uuid::now_v7(), &username, "verifier-str")
        .await
        .expect("create account");
    assert_eq!(account.username, username);

    // Duplicate username must violate the unique constraint.
    let dup = auth_repo::create_account(pool, Uuid::now_v7(), &username, "verifier-str").await;
    assert!(dup.is_err(), "duplicate username should be rejected");

    let found = auth_repo::find_account_by_username(pool, &username)
        .await
        .expect("query")
        .expect("account exists");
    assert_eq!(found.id, account.id);

    // Two devices for the same account (multi-device).
    let device1 = auth_repo::create_device(pool, Uuid::now_v7(), account.id, "phone", b"idkey-1")
        .await
        .expect("device 1");
    let _device2 = auth_repo::create_device(pool, Uuid::now_v7(), account.id, "laptop", b"idkey-2")
        .await
        .expect("device 2");

    // Session round-trip via hashed tokens.
    let access_hash = b"access-hash-0000000000000000000000".to_vec();
    let refresh_hash = b"refresh-hash-000000000000000000000".to_vec();
    let expires = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    let session = auth_repo::create_session(
        pool,
        Uuid::now_v7(),
        device1.id,
        &access_hash,
        &refresh_hash,
        expires,
    )
    .await
    .expect("create session");

    let live = auth_repo::find_live_session_by_access_hash(pool, &access_hash)
        .await
        .expect("query")
        .expect("session live");
    assert_eq!(live.id, session.id);

    // Rotate then confirm the old access hash no longer resolves.
    let new_access = b"access-hash-1111111111111111111111".to_vec();
    let new_refresh = b"refresh-hash-111111111111111111111".to_vec();
    auth_repo::rotate_session(pool, session.id, &new_access, &new_refresh, expires)
        .await
        .expect("rotate");
    assert!(auth_repo::find_live_session_by_access_hash(pool, &access_hash)
        .await
        .unwrap()
        .is_none());
    assert!(auth_repo::find_live_session_by_access_hash(pool, &new_access)
        .await
        .unwrap()
        .is_some());

    // Revoke → no longer live.
    auth_repo::revoke_session(pool, session.id).await.expect("revoke");
    assert!(auth_repo::find_live_session_by_access_hash(pool, &new_access)
        .await
        .unwrap()
        .is_none());
}
