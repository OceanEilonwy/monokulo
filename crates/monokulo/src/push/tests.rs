use super::*;
use crate::db::{Database, Db};
use crate::http::TEST_ENCRYPTION_KEY;

fn database() -> Database {
    Database::inline(Db::open_in_memory().unwrap().into_shared())
}

/// The first start makes the key and stores it, its private half
/// encrypted; every start after reads that same key back.
#[tokio::test]
async fn the_vapid_key_is_made_once_and_survives_a_restart() {
    let db = database();
    let first = Push::load_or_make(&db, &TEST_ENCRYPTION_KEY).await.unwrap();
    let stored = db.lock().vapid_key().unwrap().expect("a stored key");
    assert_eq!(stored.public_key, first.vapid_public_key().to_string());
    let private = hex::encode(*first.vapid.to_bytes());
    assert!(
        !stored.private_key_encrypted.contains(&private),
        "the private key is stored in the clear"
    );
    let again = Push::load_or_make(&db, &TEST_ENCRYPTION_KEY).await.unwrap();
    assert_eq!(again.vapid_public_key(), first.vapid_public_key());
    assert_eq!(*again.vapid.to_bytes(), *first.vapid.to_bytes());
    assert_eq!(db.lock().vapid_key().unwrap(), Some(stored));
}

/// Under another encryption key the stored key can't be read, and that is
/// an error saying so, never a new key that every subscription made with
/// the old one would refuse; the stored key is left as it was.
#[tokio::test]
async fn a_key_that_cannot_be_read_back_is_an_error_not_a_new_key() {
    let db = database();
    Push::load_or_make(&db, &TEST_ENCRYPTION_KEY).await.unwrap();
    let stored = db.lock().vapid_key().unwrap();
    let error = Push::load_or_make(&db, &AtRestKey::new([9; 32]))
        .await
        .err()
        .expect("a wrong key is refused");
    assert!(matches!(error, LoadError::Decrypt(_)), "{error:?}");
    assert!(
        error.to_string().contains("MONOKULO_ENCRYPTION_KEY"),
        "{error}"
    );
    assert_eq!(db.lock().vapid_key().unwrap(), stored);
}

/// A stored public key that isn't the private key's (a row edited by
/// hand) is refused: apps would be handed a key their pushes don't verify
/// against.
#[tokio::test]
async fn a_stored_public_key_must_be_the_private_keys() {
    let db = database();
    let other = ::push::VapidKey::generate();
    db.lock()
        .store_vapid_key(&StoredVapidKey {
            public_key: ::push::VapidKey::generate().public().to_string(),
            private_key_encrypted: encrypt_private(&TEST_ENCRYPTION_KEY, &other),
            created_at: 1,
        })
        .unwrap();
    let error = Push::load_or_make(&db, &TEST_ENCRYPTION_KEY)
        .await
        .err()
        .expect("a mismatched row is refused");
    assert!(matches!(error, LoadError::Mismatched), "{error:?}");
}

/// The private key is bound to its row: a value encrypted for another
/// row (a webhook's secret, a setting) doesn't decrypt as it.
#[tokio::test]
async fn the_private_key_is_bound_to_its_row() {
    let db = database();
    let key = ::push::VapidKey::generate();
    let hex = hex::encode(*key.to_bytes());
    db.lock()
        .store_vapid_key(&StoredVapidKey {
            public_key: key.public().to_string(),
            private_key_encrypted: crate::crypto::encrypt(
                &TEST_ENCRYPTION_KEY,
                Binding::Setting("vapid"),
                &hex,
            ),
            created_at: 1,
        })
        .unwrap();
    let error = Push::load_or_make(&db, &TEST_ENCRYPTION_KEY)
        .await
        .err()
        .expect("a value from another binding is refused");
    assert!(matches!(error, LoadError::Decrypt(_)), "{error:?}");
}

/// A second key stored after the first never replaces it.
#[test]
fn the_first_stored_key_is_kept() {
    let db = Db::open_in_memory().unwrap();
    let row = |public_key: &str| StoredVapidKey {
        public_key: public_key.to_owned(),
        private_key_encrypted: "00".to_owned(),
        created_at: 1,
    };
    assert_eq!(db.store_vapid_key(&row("first")).unwrap(), row("first"));
    assert_eq!(db.store_vapid_key(&row("second")).unwrap(), row("first"));
}

/// A kind reads and writes by the name `push_targets.kind` keeps.
#[test]
fn a_kind_is_its_name() {
    for kind in PushTargetKind::ALL {
        let json = serde_json::to_value(kind).unwrap();
        assert_eq!(json, kind.as_str());
        assert_eq!(
            serde_json::from_value::<PushTargetKind>(json).unwrap(),
            kind
        );
    }
    let schema = schemars::schema_for!(PushTargetKind).to_value();
    assert_eq!(
        schema["enum"],
        serde_json::json!(["encrypted", "live-activity"])
    );
}

/// A private key that decrypts but isn't one (not hex, the wrong length,
/// zero, or past the curve's order) stops the start with what is wrong,
/// rather than a key made afresh.
#[tokio::test]
async fn a_stored_private_key_that_isnt_a_scalar_is_unreadable() {
    let public = ::push::VapidKey::generate().public().to_string();
    for hex in [
        "not hex at all".to_owned(),
        "00".repeat(31),
        "00".repeat(33),
        "00".repeat(32),
        "ff".repeat(32),
    ] {
        let db = database();
        db.lock()
            .store_vapid_key(&StoredVapidKey {
                public_key: public.clone(),
                private_key_encrypted: crate::crypto::encrypt(
                    &TEST_ENCRYPTION_KEY,
                    Binding::VapidKey,
                    &hex,
                ),
                created_at: 1,
            })
            .unwrap();
        let error = Push::load_or_make(&db, &TEST_ENCRYPTION_KEY)
            .await
            .err()
            .unwrap_or_else(|| panic!("{hex:?} was taken for a key"));
        assert!(
            matches!(error, LoadError::Unreadable(_)),
            "{hex:?}: {error:?}"
        );
        assert_eq!(db.lock().vapid_key().unwrap().unwrap().public_key, public);
    }
}

/// Two first starts on one database file at once (its read pool sees no
/// key for both) end with the same key: whichever was stored first,
/// which the other reads back instead of its own.
#[tokio::test]
async fn first_starts_at_once_agree_on_one_key() {
    let path = std::env::temp_dir().join(format!("monokulo_vapid_{}.db", uuid::Uuid::new_v4()));
    let db = Database::open(path.to_str().unwrap(), 4).unwrap();
    let key = TEST_ENCRYPTION_KEY;
    let starts =
        futures_util::future::join_all((0..8).map(|_| Push::load_or_make(&db, &key))).await;
    let keys: Vec<String> = starts
        .into_iter()
        .map(|push| push.unwrap().vapid_public_key().to_string())
        .collect();
    let stored = db.read(|db| db.vapid_key()).await.unwrap().unwrap();
    assert!(keys.iter().all(|key| *key == stored.public_key), "{keys:?}");
    drop(db);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

/// The key an app is handed is the one pushes are signed with, across a
/// restart: a subscription made before it takes a push signed after it,
/// and refuses one signed by any other key, one for another push
/// service, and one past its expiry.
#[tokio::test]
async fn the_published_key_verifies_pushes_signed_after_a_restart() {
    let db = database();
    let before = Push::load_or_make(&db, &TEST_ENCRYPTION_KEY).await.unwrap();
    let handed: ::push::VapidPublicKey = before.vapid_public_key().to_string().parse().unwrap();
    let after = Push::load_or_make(&db, &TEST_ENCRYPTION_KEY).await.unwrap();
    let service = ::push::Origin::parse("https://ntfy.sh").unwrap();
    let now = 1_800_000_000;
    let signed = after.vapid.authorization(&service, None, now).unwrap();
    let claims = ::push::vapid::verify(&signed, &handed, &service, now).unwrap();
    assert_eq!(claims.aud, service);

    let stranger = ::push::VapidKey::generate()
        .authorization(&service, None, now)
        .unwrap();
    assert!(matches!(
        ::push::vapid::verify(&stranger, &handed, &service, now),
        Err(::push::vapid::VapidError::KeyMismatch)
    ));
    let elsewhere = ::push::Origin::parse("https://fcm.googleapis.com").unwrap();
    assert!(matches!(
        ::push::vapid::verify(&signed, &handed, &elsewhere, now),
        Err(::push::vapid::VapidError::Audience)
    ));
    let late = now + ::push::vapid::EXPIRY.as_secs() as i64;
    assert!(matches!(
        ::push::vapid::verify(&signed, &handed, &service, late),
        Err(::push::vapid::VapidError::Expired)
    ));
}
