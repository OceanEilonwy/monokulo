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

/// `POST` with `key` and a raw `body` of `content_type`, for what
/// [`api_request`] can't send: a body that isn't JSON, or none named.
async fn post_raw(
    fixture: &Fixture,
    key: &str,
    content_type: Option<&str>,
    body: String,
) -> axum::response::Response {
    let mut request = axum::http::Request::builder()
        .method("POST")
        .uri(TARGETS)
        .header(header::AUTHORIZATION, format!("Bearer {key}"));
    if let Some(content_type) = content_type {
        request = request.header(header::CONTENT_TYPE, content_type);
    }
    fixture
        .router
        .clone()
        .oneshot(request.body(axum::body::Body::from(body)).unwrap())
        .await
        .unwrap()
}

/// How many push targets there are, of every app.
fn rows(fixture: &Fixture) -> i64 {
    fixture
        .state
        .db
        .lock()
        .conn_for_test()
        .query_row("SELECT COUNT(*) FROM push_targets", [], |row| row.get(0))
        .unwrap()
}

/// A registration that isn't the form's JSON is refused as the document
/// says (every answer here is checked against it) and registers nothing:
/// no JSON `Content-Type` is `415`, a body past the API's 32 KiB is `413`
/// even when it would otherwise be a fine registration, and JSON cut
/// short, a string or `null`, of the wrong types, or without a field the
/// form needs is `400`.
#[tokio::test]
async fn a_registration_that_isnt_the_forms_json_is_refused() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let ak = fixture.ak.as_str();
    let fine = subscription("unifiedpush", "https://ntfy.sh/upAbc").to_string();
    for content_type in [
        None,
        Some("text/plain"),
        Some("application/x-www-form-urlencoded"),
    ] {
        let answer = post_raw(&fixture, ak, content_type, fine.clone()).await;
        assert_eq!(
            answer.status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "{content_type:?}"
        );
        assert_problem(answer, "unsupported-media-type").await;
    }

    // Thousands of one event, which would be kept once: refused unread.
    let mut huge = subscription("unifiedpush", "https://ntfy.sh/upAbc");
    huge["events"] = json!(vec!["sale.paid"; 4000]);
    let huge = huge.to_string();
    assert!(huge.len() > crate::api::runtime::MAX_BODY_BYTES);
    let answer = post_raw(&fixture, ak, Some("application/json"), huge).await;
    assert_eq!(answer.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_problem(answer, "too-large").await;

    let mut no_client_id = subscription("unifiedpush", "https://ntfy.sh/upAbc");
    no_client_id.as_object_mut().unwrap().remove("client_id");
    let mut numbered = subscription("unifiedpush", "https://ntfy.sh/upAbc");
    numbered["client_id"] = json!(7);
    let mut keys_as_text = subscription("unifiedpush", "https://ntfy.sh/upAbc");
    keys_as_text["keys"] = json!("BCVx.BTBZ");
    for (body, says) in [
        (fine[..fine.len() / 2].to_owned(), "EOF"),
        ("\"unifiedpush\"".to_owned(), "invalid type"),
        ("null".to_owned(), "invalid type"),
        (no_client_id.to_string(), "missing field `client_id`"),
        (numbered.to_string(), "invalid type"),
        (keys_as_text.to_string(), "invalid type"),
    ] {
        let answer = post_raw(&fixture, ak, Some("application/json"), body.clone()).await;
        assert_eq!(answer.status(), StatusCode::BAD_REQUEST, "{body}");
        let problem = assert_problem(answer, "invalid-request").await;
        let detail = problem["detail"].as_str().unwrap();
        assert!(detail.contains(says), "{body}: {detail}");
    }
    assert_eq!(rows(&fixture), 0);
}

/// Registrations racing each other still hold to the limit: of 12
/// different subscriptions sent at once, 8 are registered and 4 are
/// `409`; the same `client_id` sent 6 times at once is one target, which
/// every answer names.
#[tokio::test]
async fn registrations_at_once_keep_to_the_limit_and_to_one_target_per_client_id() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let ak = fixture.ak.as_str();
    let answers = futures_util::future::join_all((0..12).map(|n| {
        call(
            &fixture,
            "POST",
            TARGETS,
            ak,
            Some(subscription(&format!("till-{n}"), "https://ntfy.sh/upTill")),
        )
    }))
    .await;
    let statuses: Vec<StatusCode> = answers.iter().map(|a| a.status()).collect();
    let made = statuses
        .iter()
        .filter(|s| **s == StatusCode::CREATED)
        .count();
    let refused = statuses
        .iter()
        .filter(|s| **s == StatusCode::CONFLICT)
        .count();
    assert_eq!((made, refused), (8, 4), "{statuses:?}");
    assert_eq!(rows(&fixture), 8);

    let sibling = connect_app(&fixture.router, &fixture.store_id, &fixture.cookie).await;
    let sibling = sibling["app_key"].as_str().unwrap();
    let answers = futures_util::future::join_all((0..6).map(|n| {
        call(
            &fixture,
            "POST",
            TARGETS,
            sibling,
            Some(subscription("same", &format!("https://ntfy.sh/up{n}"))),
        )
    }))
    .await;
    let mut ids = Vec::new();
    for answer in answers {
        assert_eq!(answer.status(), StatusCode::CREATED);
        ids.push(body_json(answer).await["push_target_id"].clone());
    }
    ids.dedup();
    assert_eq!(ids.len(), 1, "{ids:?}");
    let list = body_json(call(&fixture, "GET", TARGETS, sibling, None).await).await;
    assert_eq!(list["push_targets"].as_array().unwrap().len(), 1);
    assert_eq!(list["push_targets"][0]["push_target_id"], ids[0]);
}

/// Once an app is disconnected its key is `401` on every push-target
/// route: its targets went with it, and it can't register new ones.
#[tokio::test]
async fn a_disconnected_apps_key_is_unauthorized_on_every_push_target_route() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let ak = fixture.ak.as_str();
    let made = register(
        &fixture,
        ak,
        subscription("unifiedpush", "https://ntfy.sh/up"),
    )
    .await;
    let one = format!("{TARGETS}/{}", made["push_target_id"].as_str().unwrap());
    let gone = call(&fixture, "DELETE", "/api/v1/app", ak, None).await;
    assert_eq!(gone.status(), StatusCode::NO_CONTENT);
    for (method, uri, body) in [
        ("GET", TARGETS, None),
        (
            "POST",
            TARGETS,
            Some(subscription("unifiedpush", "https://ntfy.sh/up")),
        ),
        ("GET", one.as_str(), None),
        ("DELETE", one.as_str(), None),
    ] {
        let answer = call(&fixture, method, uri, ak, body).await;
        assert_eq!(answer.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
        assert_problem(answer, "unauthorized").await;
    }
    assert_eq!(rows(&fixture), 0);
}

/// An app disconnected between its key's check and the insert (the race
/// `Registration::AppGone` is for) registers nothing, rather than a
/// target under no app.
#[tokio::test]
async fn a_target_for_an_app_that_is_gone_is_not_registered() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let client_id = crate::api::runtime::scalars::ClientId::parse("unifiedpush").unwrap();
    let endpoint = ::push::Endpoint::parse("https://ntfy.sh/up").unwrap();
    let registered = fixture.state.db.lock().register_push_target(
        &crate::db::NewPushTarget {
            app_id: &AppId::new("app_disconnected"),
            client_id: &client_id,
            kind: &crate::db::TargetKind::LiveActivity {
                include_payment_uri: false,
            },
            endpoint: &endpoint,
            events: &SaleKind::ALL,
        },
        1,
    );
    assert_eq!(registered.unwrap(), crate::db::Registration::AppGone);
    assert_eq!(rows(&fixture), 0);
}

/// The 2048-character bound is on the endpoint as it is stored: one of
/// exactly 2048 is registered and read back; one of 2049 is refused, and
/// so is a shorter one whose non-ASCII path is stored percent-escaped,
/// three times longer. (Before the push crate checked the stored URL,
/// that one was registered and then failed to read back, and every read
/// of the app's targets answered `500` from then on.) A path that stays
/// in bounds once escaped is registered, read back escaped.
#[tokio::test]
async fn an_endpoint_is_bounded_as_it_is_stored() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let ak = fixture.ak.as_str();
    let base = "https://ntfy.sh/";
    let exact = format!("{base}{}", "a".repeat(2048 - base.len()));
    let made = register(&fixture, ak, subscription("exact", &exact)).await;
    assert_eq!(made["endpoint"], exact.as_str());

    let over = format!("{exact}a");
    let escaped_over = format!("{base}{}", "é".repeat(1000));
    assert!(escaped_over.len() <= 2048);
    for endpoint in [over, escaped_over] {
        let answer = call(
            &fixture,
            "POST",
            TARGETS,
            ak,
            Some(subscription("long", &endpoint)),
        )
        .await;
        assert_eq!(
            answer.status(),
            StatusCode::BAD_REQUEST,
            "{}",
            endpoint.len()
        );
        let problem = assert_problem(answer, "invalid-request").await;
        let detail = problem["detail"].as_str().unwrap();
        assert!(detail.contains("longer than 2048"), "{detail}");
    }

    let escaped = register(
        &fixture,
        ak,
        subscription("escaped", &format!("{base}{}", "é".repeat(300))),
    )
    .await;
    let stored = escaped["endpoint"].as_str().unwrap();
    assert!(
        stored.len() <= 2048 && stored.ends_with("%C3%A9"),
        "{stored}"
    );
    let list = call(&fixture, "GET", TARGETS, ak, None).await;
    assert_eq!(list.status(), StatusCode::OK);
    let list = body_json(list).await;
    assert_eq!(list["push_targets"].as_array().unwrap().len(), 2);
}

/// What host an endpoint may name: an onion address is refused however it
/// is spelled; plain `http` only reaches this machine (a push service on
/// loopback, which sending refuses unless the admin allows private
/// addresses), never another host, a private one included.
#[tokio::test]
async fn an_endpoint_is_https_never_an_onion_and_http_only_on_loopback() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let ak = fixture.ak.as_str();
    for endpoint in [
        "https://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.ONION/up",
        "https://up.2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion./up",
        "https://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid%2Eonion/up",
        "http://10.0.0.7/up",
        "http://192.168.1.2:8080/up",
        "http://ntfy.sh/up",
        "ws://127.0.0.1/up",
        "file:///etc/passwd",
    ] {
        let answer = call(
            &fixture,
            "POST",
            TARGETS,
            ak,
            Some(subscription("refused", endpoint)),
        )
        .await;
        assert_eq!(answer.status(), StatusCode::BAD_REQUEST, "{endpoint}");
        let problem = assert_problem(answer, "invalid-request").await;
        assert!(
            problem["detail"].as_str().unwrap().contains("`endpoint`"),
            "{endpoint}: {problem}"
        );
    }
    assert_eq!(rows(&fixture), 0);
    for (n, endpoint) in [
        "http://127.0.0.1:8444/up",
        "http://[::1]/up",
        "http://localhost/up",
    ]
    .into_iter()
    .enumerate()
    {
        let made = register(&fixture, ak, subscription(&format!("local-{n}"), endpoint)).await;
        assert_eq!(made["endpoint"], endpoint);
    }
}

/// Keys are checked as keys: a 65-byte point that isn't on the curve, or
/// a compressed one, is refused; padding, which some libraries add, is
/// accepted and stored without it, as Web Push writes keys.
#[tokio::test]
async fn subscription_keys_are_checked_as_keys() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let ak = fixture.ak.as_str();
    let good = subscription("x", "https://x.example")["keys"].clone();
    let off_curve = format!("BA{}", "A".repeat(85));
    let compressed = "AiVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcx";
    for (keys, says) in [
        (
            json!({"p256dh": off_curve, "auth": good["auth"]}),
            "`keys.p256dh`",
        ),
        (
            json!({"p256dh": compressed, "auth": good["auth"]}),
            "`keys.p256dh`",
        ),
        (
            json!({"p256dh": good["p256dh"], "auth": "BTBZMqHH6r4Tts7J_aSIgg+/"}),
            "`keys.auth`",
        ),
        (
            json!({"p256dh": good["p256dh"], "auth": "BTBZMqHH6r4Tts7J_aSIggAA"}),
            "`keys.auth`",
        ),
        (json!({"p256dh": good["p256dh"]}), "missing field `auth`"),
    ] {
        let mut body = subscription("unifiedpush", "https://ntfy.sh/up");
        body["keys"] = keys.clone();
        let answer = call(&fixture, "POST", TARGETS, ak, Some(body)).await;
        assert_eq!(answer.status(), StatusCode::BAD_REQUEST, "{keys}");
        let problem = assert_problem(answer, "invalid-request").await;
        let detail = problem["detail"].as_str().unwrap();
        assert!(detail.contains(says), "{keys}: {detail}");
    }
    assert_eq!(rows(&fixture), 0);

    let mut padded = subscription("unifiedpush", "https://ntfy.sh/up");
    padded["keys"]["auth"] = json!("BTBZMqHH6r4Tts7J_aSIgg==");
    register(&fixture, ak, padded).await;
    let auth: String = fixture
        .state
        .db
        .lock()
        .conn_for_test()
        .query_row("SELECT auth FROM push_targets", [], |row| row.get(0))
        .unwrap();
    assert_eq!(auth, "BTBZMqHH6r4Tts7J_aSIgg");
}

/// A `client_id` is 1 to 64 visible ASCII characters: 64 is registered,
/// 65, none, a space, a control character or a non-ASCII one is refused.
#[tokio::test]
async fn a_client_id_is_one_to_64_visible_ascii_characters() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let ak = fixture.ak.as_str();
    let longest = "~".repeat(64);
    let made = register(&fixture, ak, subscription(&longest, "https://ntfy.sh/up")).await;
    assert_eq!(made["client_id"], longest.as_str());
    for client_id in [
        "x".repeat(65),
        String::new(),
        "a b".into(),
        "a\tb".into(),
        "tíll".into(),
    ] {
        let answer = call(
            &fixture,
            "POST",
            TARGETS,
            ak,
            Some(subscription(&client_id, "https://ntfy.sh/up")),
        )
        .await;
        assert_eq!(answer.status(), StatusCode::BAD_REQUEST, "{client_id:?}");
        assert_problem(answer, "invalid-request").await;
    }
    assert_eq!(rows(&fixture), 1);
}

/// A Live Activity's target registered again as an encrypted one keeps no
/// `include_payment_uri` (stored false, shown not at all), and carries
/// the keys it was given: nothing of the old kind is left behind.
#[tokio::test]
async fn a_live_activity_registered_again_as_encrypted_keeps_nothing_of_it() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let ak = fixture.ak.as_str();
    let first = register(
        &fixture,
        ak,
        json!({
            "client_id": "island",
            "kind": "live-activity",
            "endpoint": "https://relay.example/v1/push/ph_1",
            "include_payment_uri": true,
        }),
    )
    .await;
    assert_eq!(first["include_payment_uri"], true);
    let again = register(&fixture, ak, subscription("island", "https://ntfy.sh/up")).await;
    assert_eq!(again["push_target_id"], first["push_target_id"]);
    assert_eq!(again["kind"], "encrypted");
    assert!(again.get("include_payment_uri").is_none(), "{again}");
    let app = app_id(&fixture, ak).await;
    let targets = fixture.state.db.lock().list_push_targets(&app).unwrap();
    let crate::db::TargetKind::Encrypted { auth, .. } = &targets[0].kind else {
        panic!("{:?}", targets[0].kind);
    };
    assert_eq!(auth.to_base64url(), "BTBZMqHH6r4Tts7J_aSIgg");
    let stored: i64 = fixture
        .state
        .db
        .lock()
        .conn_for_test()
        .query_row("SELECT include_payment_uri FROM push_targets", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(stored, 0);
}

/// The list is oldest first, and registering a target again leaves it
/// where it was: the app sees one order whichever subscription it renews.
#[tokio::test]
async fn the_list_keeps_registration_order_through_renewals() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let ak = fixture.ak.as_str();
    for name in ["a", "b", "c"] {
        register(&fixture, ak, subscription(name, "https://ntfy.sh/up")).await;
    }
    register(&fixture, ak, subscription("a", "https://ntfy.sh/upRenewed")).await;
    let list = body_json(call(&fixture, "GET", TARGETS, ak, None).await).await;
    let names: Vec<&str> = list["push_targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["client_id"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["a", "b", "c"]);
    assert_eq!(
        list["push_targets"][0]["endpoint"],
        "https://ntfy.sh/upRenewed"
    );
}

/// A path that names no target of the app's is `404`, whatever it holds:
/// a word, the id in capitals, the id with more after it, an escaped
/// character.
#[tokio::test]
async fn a_path_that_names_no_target_is_not_found() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let ak = fixture.ak.as_str();
    let made = register(
        &fixture,
        ak,
        subscription("unifiedpush", "https://ntfy.sh/up"),
    )
    .await;
    let id = made["push_target_id"].as_str().unwrap();
    for named in [
        "garbage".to_owned(),
        id.to_uppercase(),
        format!("{id}0"),
        format!("{id}%20"),
        "pt_%27%20OR%20%271%27%3D%271".to_owned(),
    ] {
        for method in ["GET", "DELETE"] {
            let answer = call(&fixture, method, &format!("{TARGETS}/{named}"), ak, None).await;
            assert_eq!(answer.status(), StatusCode::NOT_FOUND, "{method} {named}");
            assert_problem(answer, "not-found").await;
        }
    }
    assert_eq!(rows(&fixture), 1);
}

/// A row that no longer reads as a target (an event name this version
/// doesn't know, as after a downgrade) is `500` `internal` to read alone,
/// never something else quietly, and is left out of the app's list (logged)
/// rather than failing it: the app's other targets are still listed, and
/// one missing from the list is the app's cue to register it again. The
/// app can put it right: registering its `client_id` again works, and so
/// does unregistering it.
#[tokio::test]
async fn an_unreadable_target_is_internal_alone_and_left_out_of_the_list() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let ak = fixture.ak.as_str();
    let made = register(
        &fixture,
        ak,
        subscription("unifiedpush", "https://ntfy.sh/up"),
    )
    .await;
    let fine = register(&fixture, ak, subscription("fcm", "https://ntfy.sh/fcm")).await;
    let spoil = |fixture: &Fixture| {
        fixture
            .state
            .db
            .lock()
            .conn_for_test()
            .execute(
                "UPDATE push_targets SET events = '[\"sale.refunded\"]' WHERE id = ?1",
                [made["push_target_id"].as_str().unwrap()],
            )
            .unwrap();
    };
    let listed = |answer: Value| -> Vec<Value> {
        answer["push_targets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|target| target["push_target_id"].clone())
            .collect()
    };
    let one = format!("{TARGETS}/{}", made["push_target_id"].as_str().unwrap());
    spoil(&fixture);
    let answer = call(&fixture, "GET", &one, ak, None).await;
    assert_eq!(answer.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_problem(answer, "internal").await;
    let list = call(&fixture, "GET", TARGETS, ak, None).await;
    assert_eq!(list.status(), StatusCode::OK);
    assert_eq!(
        listed(body_json(list).await),
        [fine["push_target_id"].clone()]
    );
    let again = register(
        &fixture,
        ak,
        subscription("unifiedpush", "https://ntfy.sh/up2"),
    )
    .await;
    assert_eq!(again["push_target_id"], made["push_target_id"]);
    assert_eq!(
        call(&fixture, "GET", &one, ak, None).await.status(),
        StatusCode::OK
    );
    let list = call(&fixture, "GET", TARGETS, ak, None).await;
    assert_eq!(listed(body_json(list).await).len(), 2);
    spoil(&fixture);
    let gone = call(&fixture, "DELETE", &one, ak, None).await;
    assert_eq!(gone.status(), StatusCode::NO_CONTENT);
    assert_eq!(rows(&fixture), 1);
}

/// The table itself refuses what the types do: a kind it doesn't know,
/// an encrypted target missing a key, a Live Activity's with one,
/// `include_payment_uri` on an encrypted target or not a boolean, a
/// second target under one app's `client_id`, a target of an app that
/// doesn't exist.
#[tokio::test]
async fn the_push_targets_table_refuses_what_its_types_do() {
    let engine = engine().await;
    let fixture = fixture(engine.router(), PUBLIC_URI, &Recorder::default()).await;
    let made = register(
        &fixture,
        &fixture.ak,
        subscription("unifiedpush", "https://ntfy.sh/up"),
    )
    .await;
    assert_eq!(made["kind"], "encrypted");
    let app = app_id(&fixture, &fixture.ak).await;
    let db = fixture.state.db.lock();
    let conn = db.conn_for_test();
    let insert = |id: &str,
                  app_id: &str,
                  client_id: &str,
                  kind: &str,
                  keys: (Option<&str>, Option<&str>),
                  include_payment_uri: i64| {
        conn.execute(
            "INSERT INTO push_targets
                 (id, store_id, app_id, client_id, kind, endpoint, p256dh, auth, events,
                  include_payment_uri, created_at_utc)
             VALUES (?1, ?2, ?3, ?4, ?5, 'https://ntfy.sh/up', ?6, ?7, '[]', ?8, 1)",
            rusqlite::params![
                id,
                fixture.store_id,
                app_id,
                client_id,
                kind,
                keys.0,
                keys.1,
                include_payment_uri
            ],
        )
    };
    let app = app.as_str();
    let both = (Some("p"), Some("a"));
    for (n, (app_id, client_id, kind, keys, include)) in [
        (app, "k1", "apns", both, 0),
        (app, "k2", "encrypted", (None, None), 0),
        (app, "k3", "encrypted", (Some("p"), None), 0),
        (app, "k4", "encrypted", (None, Some("a")), 0),
        (app, "k5", "live-activity", both, 0),
        (app, "k6", "live-activity", (Some("p"), None), 0),
        (app, "k7", "encrypted", both, 1),
        (app, "k8", "live-activity", (None, None), 2),
        (app, "unifiedpush", "encrypted", both, 0),
        ("app_nobody", "k9", "encrypted", both, 0),
    ]
    .into_iter()
    .enumerate()
    {
        let refused = insert(&format!("pt_{n}"), app_id, client_id, kind, keys, include);
        assert!(refused.is_err(), "{client_id} {kind} {keys:?} {include}");
    }
    insert("pt_fine", app, "fine", "live-activity", (None, None), 1).unwrap();
    assert_eq!(db.list_push_targets(&AppId::new(app)).unwrap().len(), 2);
}
