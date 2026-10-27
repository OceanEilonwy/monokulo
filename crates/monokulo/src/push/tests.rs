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
