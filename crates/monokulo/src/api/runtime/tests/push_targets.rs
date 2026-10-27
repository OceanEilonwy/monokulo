//! An app's push targets as the app meets them: who may have them, what
//! registering one takes and refuses, registering it again, the limit,
//! and that one app's targets are no other app's, and go when it does.

use axum::http::{header, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::super::recording::{connect_app, fixture, Fixture, Recorder, PUBLIC_URI};
use crate::apps::SaleKind;
use crate::db::{AppId, PushTargetId};
use crate::http::test_support::{api_request, assert_problem, body_json, store_with_keys};

const TARGETS: &str = "/api/v1/push-targets";

async fn engine() -> engine_test_support::TestEngineHandle {
    engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await
}

async fn call(
    fixture: &Fixture,
    method: &str,
    uri: &str,
    key: &str,
    body: Option<Value>,
) -> axum::response::Response {
    fixture
        .router
        .clone()
        .oneshot(api_request(method, uri, Some(key), body))
        .await
        .unwrap()
}

/// A UnifiedPush subscription named `client_id`, at `endpoint`, with RFC
/// 8291's example receiver's keys.
fn subscription(client_id: &str, endpoint: &str) -> Value {
    json!({
        "client_id": client_id,
        "kind": "encrypted",
        "endpoint": endpoint,
        "keys": {
            "p256dh": "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
            "auth": "BTBZMqHH6r4Tts7J_aSIgg"
        }
    })
}

/// Registers `body` with `key`, expecting it made: its body.
async fn register(fixture: &Fixture, key: &str, body: Value) -> Value {
    let made = call(fixture, "POST", TARGETS, key, Some(body)).await;
    assert_eq!(made.status(), StatusCode::CREATED);
    body_json(made).await
}

/// The app's id, from its own resource.
async fn app_id(fixture: &Fixture, ak: &str) -> AppId {
    let app = body_json(call(fixture, "GET", "/api/v1/app", ak, None).await).await;
    AppId::new(app["app_id"].as_str().unwrap())
}

/// Only an app's own key has push targets: the store's secret and public
/// keys are refused on every push-target route, and the entry offers them
/// neither the link nor the form, which it offers an app.
#[tokio::test]
async fn only_an_apps_key_has_push_targets() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let made = register(
        &fixture,
        &fixture.ak,
        subscription("unifiedpush", "https://ntfy.sh/upAbc?up=1"),
    )
    .await;
    let one = format!("{TARGETS}/{}", made["push_target_id"].as_str().unwrap());
    for key in [&fixture.sk, &fixture.pk] {
        for (method, uri, body) in [
            ("GET", TARGETS, None),
            (
                "POST",
                TARGETS,
                Some(subscription("mine", "https://ntfy.sh/upMine")),
            ),
            ("GET", one.as_str(), None),
            ("DELETE", one.as_str(), None),
        ] {
            let answer = call(&fixture, method, uri, key, body).await;
            assert_eq!(answer.status(), StatusCode::FORBIDDEN, "{method} {uri}");
            assert_problem(answer, "forbidden").await;
        }
        let entry = body_json(call(&fixture, "GET", "/api/v1", key, None).await).await;
        assert!(entry["_links"].get("mk:push-targets").is_none(), "{entry}");
        assert!(
            entry["_templates"].get("register-push-target").is_none(),
            "{entry}"
        );
    }
    // Still there: nothing above touched it.
    assert_eq!(
        call(&fixture, "GET", &one, &fixture.ak, None)
            .await
            .status(),
        StatusCode::OK
    );
    let entry = body_json(call(&fixture, "GET", "/api/v1", &fixture.ak, None).await).await;
    assert_eq!(entry["_links"]["mk:push-targets"]["href"], TARGETS);
    let form = &entry["_templates"]["register-push-target"];
    assert_eq!(
        (&form["method"], &form["target"]),
        (&json!("POST"), &json!(TARGETS))
    );
}

/// A target registered is answered `201` with its `Location`, read on its
/// own and in the list beside this Monokulo's VAPID key, never with its
/// keys, and unregistered with the form it carries; after that it is
/// `404`, read or unregistered again.
#[tokio::test]
async fn a_push_target_is_registered_read_and_unregistered() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let ak = fixture.ak.as_str();
    let made = call(
        &fixture,
        "POST",
        TARGETS,
        ak,
        Some(subscription("unifiedpush", "https://ntfy.sh/upAbc?up=1")),
    )
    .await;
    assert_eq!(made.status(), StatusCode::CREATED);
    let location = made.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    let made = body_json(made).await;
    let id = made["push_target_id"].as_str().unwrap();
    assert!(
        id.len() == 35 && id.starts_with("pt_") && id[3..].chars().all(|c| c.is_ascii_hexdigit()),
        "{id}"
    );
    assert_eq!(location, format!("{TARGETS}/{id}"));
    assert_eq!(made["_links"]["self"]["href"], location.as_str());
    assert_eq!(made["kind"], "encrypted");
    assert_eq!(made["endpoint"], "https://ntfy.sh/upAbc?up=1");
    assert!(made.get("keys").is_none() && made.get("include_payment_uri").is_none());
    assert!(
        !made.to_string().contains("BTBZMqHH6r4Tts7J_aSIgg"),
        "{made}"
    );
    assert!(made.get("last_ok_at").is_none());

    let read = body_json(call(&fixture, "GET", &location, ak, None).await).await;
    assert_eq!(read, made);
    let list = body_json(call(&fixture, "GET", TARGETS, ak, None).await).await;
    assert_eq!(
        list["vapid_public_key"],
        fixture.state.push.vapid_public_key().to_string()
    );
    assert_eq!(list["push_targets"], json!([made.clone()]));
    assert_eq!(list["_links"]["mk:app"]["href"], "/api/v1/app");
    assert_eq!(
        list["_templates"]["register-push-target"]["target"],
        TARGETS
    );

    let unregister = &read["_templates"]["unregister-push-target"];
    assert_eq!(unregister["method"], "DELETE");
    assert_eq!(unregister["target"], location.as_str());
    let gone = call(&fixture, "DELETE", &location, ak, None).await;
    assert_eq!(gone.status(), StatusCode::NO_CONTENT);
    for method in ["GET", "DELETE"] {
        let answer = call(&fixture, method, &location, ak, None).await;
        assert_eq!(answer.status(), StatusCode::NOT_FOUND, "{method}");
        assert_problem(answer, "not-found").await;
    }
    let list = body_json(call(&fixture, "GET", TARGETS, ak, None).await).await;
    assert_eq!(list["push_targets"], json!([]));
}

/// Registering again with a `client_id` the app used before gives that
/// target the new subscription: one row, the same id and `created_at`, the
/// new endpoint, kind and events, and the old subscription's failures and
/// last success forgotten.
#[tokio::test]
async fn registering_the_same_client_id_again_replaces_the_subscription() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let ak = fixture.ak.as_str();
    let first = register(
        &fixture,
        ak,
        subscription("unifiedpush", "https://ntfy.sh/upOld"),
    )
    .await;
    let id = PushTargetId::new(first["push_target_id"].as_str().unwrap());
    fixture
        .state
        .db
        .lock()
        .conn_for_test()
        .execute(
            "UPDATE push_targets SET failures = 3, last_ok_at_utc = 1000, created_at_utc = 1 WHERE id = ?1",
            [id.as_str()],
        )
        .unwrap();
    let again = register(
        &fixture,
        ak,
        json!({
            "client_id": "unifiedpush",
            "kind": "live-activity",
            "endpoint": "https://relay.example/v1/push/ph_new",
            "events": ["sale.paid"],
            "include_payment_uri": true,
        }),
    )
    .await;
    assert_eq!(again["push_target_id"], id.as_str());
    assert_eq!(again["created_at"], 1);
    assert_eq!(again["endpoint"], "https://relay.example/v1/push/ph_new");
    assert_eq!(again["kind"], "live-activity");
    assert_eq!(again["include_payment_uri"], true);
    assert_eq!(again["events"], json!(["sale.paid"]));
    assert!(again.get("last_ok_at").is_none(), "{again}");
    let app = app_id(&fixture, ak).await;
    let targets = fixture.state.db.lock().list_push_targets(&app).unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!((targets[0].failures, targets[0].last_ok_at), (0, None));
    assert_eq!(
        targets[0].kind,
        crate::db::TargetKind::LiveActivity {
            include_payment_uri: true
        }
    );
}

/// An app may have 8 push targets: the 9th is `409`
/// `too-many-push-targets`, while registering one of its 8 again still
/// works, and unregistering one makes room.
#[tokio::test]
async fn an_app_may_have_eight_push_targets() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let ak = fixture.ak.as_str();
    let mut ids = Vec::new();
    for n in 0..crate::db::MAX_TARGETS_PER_APP {
        let made = register(
            &fixture,
            ak,
            subscription(&format!("till-{n}"), "https://ntfy.sh/upTill"),
        )
        .await;
        ids.push(made["push_target_id"].as_str().unwrap().to_owned());
    }
    assert_eq!(ids.len(), 8);
    let over = call(
        &fixture,
        "POST",
        TARGETS,
        ak,
        Some(subscription("till-8", "https://ntfy.sh/upTill")),
    )
    .await;
    assert_eq!(over.status(), StatusCode::CONFLICT);
    let problem = assert_problem(over, "too-many-push-targets").await;
    assert!(
        problem["detail"].as_str().unwrap().contains('8'),
        "{problem}"
    );
    let again = register(
        &fixture,
        ak,
        subscription("till-3", "https://ntfy.sh/upTillNew"),
    )
    .await;
    assert_eq!(again["push_target_id"], ids[3].as_str());
    let gone = call(
        &fixture,
        "DELETE",
        &format!("{TARGETS}/{}", ids[0]),
        ak,
        None,
    )
    .await;
    assert_eq!(gone.status(), StatusCode::NO_CONTENT);
    register(
        &fixture,
        ak,
        subscription("till-8", "https://ntfy.sh/upTill"),
    )
    .await;
}

/// What the form refuses is `400` `invalid-request`, saying why, and
/// registers nothing: keys for a Live Activity, none for an encrypted
/// target, `include_payment_uri` for one, an endpoint Monokulo doesn't push
/// to (plain `http`, an onion address, userinfo, too long, not a URL),
/// keys that aren't keys, an event that isn't one, an empty list of
/// events, and a `client_id` its rules refuse.
#[tokio::test]
async fn a_subscription_the_form_refuses_is_an_invalid_request_with_a_reason() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let ak = fixture.ak.as_str();
    let keys = subscription("x", "https://x.example")["keys"].clone();
    let with = |changes: Value| {
        let mut body = subscription("unifiedpush", "https://ntfy.sh/upAbc");
        for (name, value) in changes.as_object().unwrap() {
            if value.is_null() {
                body.as_object_mut().unwrap().remove(name);
            } else {
                body[name] = value.clone();
            }
        }
        body
    };
    let long = format!("https://ntfy.sh/{}", "a".repeat(2048));
    let onion = "https://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/up";
    for (body, says) in [
        (
            json!({"client_id": "la", "kind": "live-activity", "endpoint": "https://relay.example/v1/push/ph_1", "keys": keys}),
            "takes no `keys`",
        ),
        (
            with(json!({"keys": null})),
            "needs its subscription's `keys`",
        ),
        (
            with(json!({"include_payment_uri": false})),
            "`include_payment_uri` is for a `live-activity` target",
        ),
        (
            with(json!({"endpoint": "http://ntfy.sh/upAbc"})),
            "`endpoint`",
        ),
        (with(json!({"endpoint": onion})), "`endpoint`"),
        (
            with(json!({"endpoint": "https://me:pw@ntfy.sh/up"})),
            "`endpoint`",
        ),
        (with(json!({"endpoint": long})), "`endpoint`"),
        (with(json!({"endpoint": "ntfy.sh/upAbc"})), "`endpoint`"),
        (
            with(json!({"keys": {"p256dh": "BCVx", "auth": "BTBZMqHH6r4Tts7J_aSIgg"}})),
            "`keys.p256dh`",
        ),
        (
            with(json!({"keys": {"p256dh": keys["p256dh"], "auth": "c2hvcnQ"}})),
            "`keys.auth`",
        ),
        (
            with(json!({"events": ["sale.refunded"]})),
            "isn't a sale event",
        ),
        (with(json!({"events": []})), "`events` names no sale event"),
        (
            with(json!({"client_id": "two words"})),
            "ClientId must match",
        ),
        (with(json!({"kind": "apns"})), "unknown variant"),
    ] {
        let answer = call(&fixture, "POST", TARGETS, ak, Some(body.clone())).await;
        assert_eq!(answer.status(), StatusCode::BAD_REQUEST, "{body}");
        let problem = assert_problem(answer, "invalid-request").await;
        let detail = problem["detail"].as_str().unwrap();
        assert!(detail.contains(says), "{body}: {detail}");
    }
    let list = body_json(call(&fixture, "GET", TARGETS, ak, None).await).await;
    assert_eq!(list["push_targets"], json!([]));
}

/// Left out, `events` is every sale event; given, it is kept once each,
/// in the order a sale goes through them.
#[tokio::test]
async fn events_are_every_sale_event_unless_the_app_names_some() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let ak = fixture.ak.as_str();
    let every = register(&fixture, ak, subscription("all", "https://ntfy.sh/upAll")).await;
    let names: Vec<&str> = SaleKind::ALL.iter().map(|kind| kind.as_str()).collect();
    assert_eq!(every["events"], json!(names));
    assert_eq!(names.len(), 7);
    let mut some = subscription("some", "https://ntfy.sh/upSome");
    some["events"] = json!(["sale.paid", "sale.waiting", "sale.paid"]);
    let some = register(&fixture, ak, some).await;
    assert_eq!(some["events"], json!(["sale.waiting", "sale.paid"]));
}

/// One app's push targets are its own: another app of the same store, or
/// of another store, can't read, list or unregister them (`404`, as for
/// one that doesn't exist), and each target is under its app's store.
#[tokio::test]
async fn another_apps_push_target_is_not_found() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let mine = register(
        &fixture,
        &fixture.ak,
        subscription("unifiedpush", "https://ntfy.sh/upMine"),
    )
    .await;
    let one = format!("{TARGETS}/{}", mine["push_target_id"].as_str().unwrap());
    // A second till on the same store, and an app of another store.
    let sibling = connect_app(&fixture.router, &fixture.store_id, &fixture.cookie).await;
    let session = fixture.cookie.strip_prefix("session=").unwrap();
    let other = store_with_keys(
        &fixture.router,
        &fixture.state,
        session,
        "cafe.example",
        "XMR",
    )
    .await;
    let stranger = connect_app(&fixture.router, &other.id, &fixture.cookie).await;
    for app in [&sibling, &stranger] {
        let ak = app["app_key"].as_str().unwrap();
        for method in ["GET", "DELETE"] {
            let answer = call(&fixture, method, &one, ak, None).await;
            assert_eq!(answer.status(), StatusCode::NOT_FOUND, "{method}");
            assert_problem(answer, "not-found").await;
        }
        let list = body_json(call(&fixture, "GET", TARGETS, ak, None).await).await;
        assert_eq!(list["push_targets"], json!([]));
        // The same client_id is each app's own.
        let theirs = register(
            &fixture,
            ak,
            subscription("unifiedpush", "https://ntfy.sh/upTheirs"),
        )
        .await;
        assert_ne!(theirs["push_target_id"], mine["push_target_id"]);
    }
    let read = body_json(call(&fixture, "GET", &one, &fixture.ak, None).await).await;
    assert_eq!(read["endpoint"], "https://ntfy.sh/upMine");
    // Each target is under its own app's store.
    let stores: Vec<(String, String)> = {
        let db = fixture.state.db.lock();
        let mut statement = db
            .conn_for_test()
            .prepare(
                "SELECT t.store_id, a.store_id FROM push_targets t JOIN app_connections a ON a.id = t.app_id",
            )
            .unwrap();
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        rows.collect::<rusqlite::Result<_>>().unwrap()
    };
    assert_eq!(stores.len(), 3);
    assert!(
        stores.iter().all(|(target, app)| target == app),
        "{stores:?}"
    );
    assert!(stores.iter().any(|(store, _)| *store == other.id));
}

/// Disconnecting an app deletes its push targets, whether it disconnects
/// itself (`DELETE /api/v1/app`) or the merchant does from the dashboard;
/// another app's stay.
#[tokio::test]
async fn disconnecting_an_app_deletes_its_push_targets() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let second = connect_app(&fixture.router, &fixture.store_id, &fixture.cookie).await;
    let third = connect_app(&fixture.router, &fixture.store_id, &fixture.cookie).await;
    let keys = [
        fixture.ak.clone(),
        second["app_key"].as_str().unwrap().to_owned(),
        third["app_key"].as_str().unwrap().to_owned(),
    ];
    let mut apps = Vec::new();
    for key in &keys {
        register(
            &fixture,
            key,
            subscription("unifiedpush", "https://ntfy.sh/up"),
        )
        .await;
        apps.push(app_id(&fixture, key).await);
    }
    let held = |app: &AppId| {
        fixture
            .state
            .db
            .lock()
            .list_push_targets(app)
            .unwrap()
            .len()
    };
    assert!(apps.iter().all(|app| held(app) == 1));

    let gone = call(&fixture, "DELETE", "/api/v1/app", &keys[0], None).await;
    assert_eq!(gone.status(), StatusCode::NO_CONTENT);
    assert_eq!(held(&apps[0]), 0);

    let disconnected = fixture
        .router
        .clone()
        .oneshot(crate::http::test_support::form_post(
            &format!(
                "/stores/{}/settings/apps/{}/disconnect",
                fixture.store_id, apps[1]
            ),
            Some(&fixture.cookie),
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(disconnected.status(), StatusCode::SEE_OTHER);
    assert_eq!(held(&apps[1]), 0);
    assert_eq!(held(&apps[2]), 1);
    let rows: i64 = fixture
        .state
        .db
        .lock()
        .conn_for_test()
        .query_row("SELECT COUNT(*) FROM push_targets", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 1);
}
