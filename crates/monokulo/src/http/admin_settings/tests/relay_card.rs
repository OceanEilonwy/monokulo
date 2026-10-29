//! The Server tab's Push relay card: the push relay inside monokulo, its
//! settings the `[relay.*]` tables of monokulo's options file, through the
//! relay's own registry (`relay::Handle::registry`). Each test runs a real
//! relay handle over a real options file on disk, in-process; the one that
//! reaches the relay's listener says so (`over_http`).

use std::collections::HashMap;

use live_settings::{Env, OptionsFile};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::settings::RELAY_TABLE;

const HANDLE_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const APNS_KEY: &str = include_str!("../../../../../push-relay/tests/fixtures/apns-test.p8");

/// The FCM account the relay's own tests use, as text.
fn fcm_account() -> String {
    serde_json::json!({
        "type": "service_account",
        "project_id": "monokular-test",
        "private_key_id": "0123456789abcdef0123456789abcdef01234567",
        "private_key": include_str!("../../../../../push-relay/tests/fixtures/fcm-test-key.pem"),
        "client_email": "relay@monokular-test.iam.gserviceaccount.com",
        "client_id": "100000000000000000000",
        "token_uri": "https://oauth2.googleapis.com/token",
    })
    .to_string()
}

/// The relay's environment: its handle key alone, `--relay-…` options `cli`.
fn with_key(cli: &[(&str, &str)]) -> Env {
    Env::fixed([("MONOKULO_RELAY_HANDLE_KEY", HANDLE_KEY)]).with_nested_cli(
        RELAY_TABLE,
        cli.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<HashMap<_, _>>(),
    )
}

/// The APNs key's ids, which the options file holds beside the key.
const APNS_IDS: &str = "\n[relay.apns]\nteam_id = \"ABCDE12345\"\nkey_id = \"FGHIJ67890\"\n";

/// Every secret of the relay set: the handle key, the APNs key (its ids
/// in the file: [`APNS_IDS`]), the FCM account.
fn with_every_secret() -> Env {
    with_key(&[])
        .with_var("MONOKULO_RELAY_APNS_KEY", APNS_KEY)
        .with_var("MONOKULO_RELAY_FCM_CREDENTIALS", &fcm_account())
}

/// A port nothing listens on.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// monokulo over the options file `text` (written to a file of its own),
/// with its push relay started as monokulo starts it, its secrets and
/// `--relay-…` options from `env`. The engine is out of reach: the Server
/// tab's relay card needs none.
async fn relay_on_file(text: &str, env: Env) -> (TempDir, std::path::PathBuf, AppState) {
    let dir = TempDir::new("relay-card");
    let path = dir.0.join("monokulo.toml");
    std::fs::write(&path, text).unwrap();
    let file = OptionsFile::at(&path);
    let mut state = test_app_state_with_client(
        EngineClient::for_tests(format!(
            "http://{}",
            service_runtime::unreachable::address()
        )),
        crate::relay::own_options(file.clone().leaving(crate::settings::ENGINE_TABLE)),
    )
    .await;
    let enabled = state.settings.relay.load().enabled;
    let prepared = crate::relay::prepare_in(&file, env, enabled).unwrap();
    state.relay = crate::relay::start(
        prepared,
        &dir.0.join("monokulo.db"),
        CancellationToken::new(),
        state.settings.relay.clone(),
    )
    .await
    .unwrap();
    (dir, path, state)
}

/// The relay's options as the file has them, listening on `port`, on or
/// off.
fn relay_file(port: u16, enabled: bool) -> String {
    format!(
        "[relay]\nenabled = {enabled}\n\n[relay.server]\nlisten = \"127.0.0.1:{port}\"\npublic_uri = \"https://relay.example.org\"\n"
    )
}

async fn server_tab(router: &Router, cookie: &str, opened: bool) -> String {
    let uri = if opened {
        "/admin/settings?tab=server&relay=edit"
    } else {
        "/admin/settings?tab=server"
    };
    body_text(get(router, uri, Some(cookie)).await).await
}

/// The Push relay card's part of a page.
fn card(html: &str) -> &str {
    let start = html
        .find(r#"id="card-relay""#)
        .unwrap_or_else(|| panic!("no Push relay card: {html}"));
    let end = html[start..]
        .find(r#"id="card-server-engine""#)
        .map_or(html.len(), |end| start + end);
    &html[start..end]
}

/// A registration of an app the relay doesn't know, at `port`: a relay
/// answers 400 with its problem.
async fn relay_answers(port: u16) -> bool {
    let answer = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/register"))
        .body(
            serde_json::json!({
                "platform": "apns", "token": "00", "app": "nobody",
                "vapid_public_key": ::push::VapidKey::generate().public().to_base64url(),
            })
            .to_string(),
        )
        .send()
        .await;
    matches!(answer, Ok(answer) if answer.status() == StatusCode::BAD_REQUEST)
}

/// Closed, the card shows the switch, the relay's state and what it can't
/// start without; Edit settings, a link, opens every relay setting in its
/// sub-section; saving them writes the `[relay.*]` tables of monokulo's
/// own options file, the apps a table each, and never a secret.
#[tokio::test]
async fn every_relay_setting_is_on_the_opened_card_and_saves_into_the_relay_tables() {
    let (_dir, path, state) = relay_on_file(
        &format!("[signup]\nmode = \"public\"\n{APNS_IDS}"),
        with_every_secret(),
    )
    .await;
    let router = build_router(state);
    let cookie = admin_session_cookie(&router).await;

    let closed = server_tab(&router, &cookie, false).await;
    let relay = card(&closed);
    assert!(relay.contains(r#"name="relay.enabled""#), "{relay}");
    assert!(relay.contains("relay-state-words\">Off<"), "{relay}");
    assert!(
        relay.contains(r#"href="/admin/settings?tab=server&amp;relay=edit#card-relay""#),
        "{relay}"
    );
    assert!(
        relay.contains("FCM ready") && relay.contains("APNs ready"),
        "{relay}"
    );
    assert!(
        relay.contains("relay.server.public_uri is not set"),
        "{relay}"
    );
    assert!(
        !relay.contains(r#"name="relay:"#),
        "closed, no relay setting: {relay}"
    );
    // After monokulo's server card (the engine's, out of reach here,
    // follows it when there is one: the views' tests).
    let at = |id: &str| closed.find(&format!(r#"id="{id}""#)).unwrap();
    assert!(at("card-server-monokulo") < at("card-relay"));

    let opened = server_tab(&router, &cookie, true).await;
    let relay = card(&opened);
    for heading in [
        "Server",
        "Handles and apps",
        "Limits, a minute each",
        "Apple (APNs)",
        "Google (FCM)",
    ] {
        assert!(
            relay.contains(&format!("<h4>{heading}</h4>")),
            "{heading}: {relay}"
        );
    }
    assert!(
        relay.contains(r#"href="/admin/settings?tab=server#card-relay""#),
        "Close: {relay}"
    );
    for setting in push_relay::settings::embedded_settings() {
        let key = setting.key();
        let named = relay.contains(&format!(r#"name="relay:{key}""#));
        let shown = !setting.sources().toml || !setting.editable() || key == "handles.apps";
        assert_eq!(named, !shown, "relay:{key}: {relay}");
    }
    // The secrets: set, never shown, never sent.
    for secret in [
        "MONOKULO_RELAY_HANDLE_KEY",
        "MONOKULO_RELAY_APNS_KEY",
        "MONOKULO_RELAY_FCM_CREDENTIALS",
    ] {
        assert!(
            relay.contains(&format!(
                "This is set with {secret} when the process starts"
            )),
            "{secret}: {relay}"
        );
    }
    assert!(!opened.contains(&HANDLE_KEY[..8]));
    assert!(!opened.contains("BEGIN PRIVATE KEY"));

    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let save = post_settings(
        &router,
        &cookie,
        &[
            ("tab", "server"),
            ("relay:server.listen", &listen),
            ("relay:server.public_uri", "https://relay.example.org"),
            ("relay:server.trusted_proxies", "10.0.0.5"),
            ("relay:server.upstream_timeout_ms", "3000"),
            ("relay:handles.ttl_days", "60"),
            ("relay:handles.unused_ttl_days", "5"),
            ("relay:limits.per_sender_per_minute", "500"),
            ("relay:limits.per_handle_per_minute", "50"),
            ("relay:limits.registrations_per_minute", "20"),
            ("relay:apns.endpoint", "https://api.sandbox.push.apple.com"),
            ("relay:apns.team_id", "ABCDE12345"),
            ("relay:apns.key_id", "FGHIJ67890"),
            ("relay:fcm.endpoint", "https://fcm.googleapis.com"),
            ("relay_apps", ""),
            ("relay_app_0_name", "monokular"),
            ("relay_app_0_apns_topic", "com.example.monokular"),
            ("relay_app_0_fcm_project_id", "monokular-test"),
        ],
    )
    .await;
    assert_eq!(save.status(), StatusCode::SEE_OTHER);
    let page = follow(&router, &cookie, save).await;
    assert!(is_saved(&page), "{}", toast_text(&page));
    // Saved, closed again, and the chips say what was saved.
    let relay = card(&page);
    assert!(!relay.contains(r#"name="relay:"#), "{relay}");
    assert!(relay.contains("1 app: monokular"), "{relay}");
    let saved = std::fs::read_to_string(&path).unwrap();
    for line in [
        "[relay.server]",
        &format!("listen = \"{listen}\""),
        "public_uri = \"https://relay.example.org\"",
        "trusted_proxies = \"10.0.0.5\"",
        "upstream_timeout_ms = 3000",
        "[relay.handles]",
        "ttl_days = 60",
        "unused_ttl_days = 5",
        "[relay.limits]",
        "per_sender_per_minute = 500",
        "[relay.apns]",
        "endpoint = \"https://api.sandbox.push.apple.com/\"",
        "team_id = \"ABCDE12345\"",
        "[relay.handles.apps.monokular]",
        "apns_topic = \"com.example.monokular\"",
        "fcm_project_id = \"monokular-test\"",
    ] {
        assert!(saved.contains(line), "{line} in {saved}");
    }
    assert!(!saved.contains(&HANDLE_KEY[..8]), "{saved}");
    assert!(!saved.contains("PRIVATE KEY"), "{saved}");
    // Off, nothing listens where it was told to.
    assert!(!relay_answers(port).await);
}

/// The card's settings sent back as shown (the apps as their rows) change
/// nothing, so nothing is written and the relay isn't restarted.
#[tokio::test]
async fn the_relay_card_saved_as_shown_writes_nothing_and_restarts_nothing() {
    let port = free_port();
    let text = format!(
        "{}{APNS_IDS}\n[relay.handles.apps.monokular]\nfcm_project_id = \"monokular-test\"\n",
        relay_file(port, true)
    );
    let (_dir, path, state) = relay_on_file(&text, with_every_secret()).await;
    let since = state.relay.view().await.unwrap().state;
    let mut form = vec![
        ("tab".to_string(), "server".to_string()),
        ("relay.enabled".to_string(), "true".to_string()),
        ("relay_apps".to_string(), String::new()),
        ("relay_app_0_name".to_string(), "monokular".to_string()),
        ("relay_app_0_apns_topic".to_string(), String::new()),
        (
            "relay_app_0_fcm_project_id".to_string(),
            "monokular-test".to_string(),
        ),
    ];
    for view in state.relay.registry().unwrap().describe() {
        if view.locked.is_none() && view.key != "handles.apps" {
            form.push((format!("relay:{}", view.key), view.value));
        }
    }
    let relay = state.relay.clone();
    let router = build_router(state);
    let cookie = admin_session_cookie(&router).await;
    let fields: Vec<(&str, &str)> = form.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let save = post_settings(&router, &cookie, &fields).await;
    let page = follow(&router, &cookie, save).await;
    assert!(
        toast_text(&page).contains("Nothing to save"),
        "{}",
        toast_text(&page)
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    assert_eq!(relay.view().await.unwrap().state, since, "not restarted");
    relay.stop().await;
}

/// A `--relay-…` option and the environment's secrets lock what they give,
/// saying where; a form sending a locked one is refused, nothing saved.
#[tokio::test]
async fn a_relay_option_on_the_command_line_shows_locked_with_why_and_is_never_saved() {
    let port = free_port().to_string();
    let listen = format!("127.0.0.1:{port}");
    let (_dir, path, state) = relay_on_file("", with_key(&[("server.listen", &listen)])).await;
    let router = build_router(state);
    let cookie = admin_session_cookie(&router).await;

    let relay = server_tab(&router, &cookie, true).await;
    let relay = card(&relay);
    assert!(!relay.contains(r#"name="relay:server.listen""#), "{relay}");
    assert!(relay.contains("--relay-server-listen"), "{relay}");
    assert!(relay.contains(&listen), "{relay}");
    assert!(
        relay.contains("This is set with MONOKULO_RELAY_HANDLE_KEY when the process starts"),
        "{relay}"
    );
    assert!(
        relay.contains("Set MONOKULO_RELAY_APNS_KEY in the environment and restart monokulo. Until then iOS registrations are refused."),
        "{relay}"
    );

    let save = post_settings(
        &router,
        &cookie,
        &[("tab", "server"), ("relay:server.listen", "127.0.0.1:1")],
    )
    .await;
    let page = follow(&router, &cookie, save).await;
    assert!(!is_saved(&page), "{}", toast_text(&page));
    assert!(card(&page).contains("--relay-server-listen"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
}

/// An operator's mistakes on the card are each refused with what the
/// setting needs, by the relay's own checks (the ones that stop the next
/// start), nothing saved, and the card comes back opened with what was
/// typed.
#[tokio::test]
async fn a_mistake_on_the_relay_card_is_refused_with_the_settings_own_words() {
    let text = "[signup]\nmode = \"public\"\n";
    let (_dir, path, state) = relay_on_file(text, with_key(&[])).await;
    let router = build_router(state);
    let cookie = admin_session_cookie(&router).await;

    let mistakes: &[(&[(&str, &str)], &str)] = &[
        (
            &[("relay:server.upstream_timeout_ms", "50")],
            "relay.server.upstream_timeout_ms",
        ),
        (
            &[("relay:handles.unused_ttl_days", "100")],
            "more than handles.ttl_days (90)",
        ),
        (
            &[("relay:server.public_uri", "http://relay.example.org")],
            "https",
        ),
        (&[("relay:apns.team_id", "abc")], "ABCDE12345"),
        (
            &[("relay:apns.team_id", "ABCDE12345")],
            "is set, but MONOKULO_RELAY_APNS_KEY isn't",
        ),
        (
            &[
                ("relay_apps", ""),
                ("relay_app_0_name", "monokular"),
                ("relay_app_0_fcm_project_id", "monokular-test"),
            ],
            "monokular has an fcm_project_id, but MONOKULO_RELAY_FCM_CREDENTIALS isn't set",
        ),
        (
            &[
                ("relay_apps", ""),
                ("relay_app_0_name", "Not An App"),
                ("relay_app_0_fcm_project_id", "monokular-test"),
            ],
            "Some apps need fixing",
        ),
        (
            &[("relay_apps", ""), ("relay_app_0_name", "monokular")],
            "needs an iOS bundle ID or a Firebase project ID, or both.",
        ),
    ];
    for (sent, said) in mistakes {
        let mut fields = vec![("tab", "server")];
        fields.extend_from_slice(sent);
        let save = post_settings(&router, &cookie, &fields).await;
        assert_eq!(save.status(), StatusCode::SEE_OTHER, "{sent:?}");
        let page = follow(&router, &cookie, save).await;
        assert!(!is_saved(&page), "{sent:?}: {}", toast_text(&page));
        let relay = unescaped(card(&page));
        assert!(relay.contains(said), "{sent:?}, {said}: {relay}");
        // Opened, with what was typed.
        assert!(
            relay.contains(r#"name="relay:server.listen""#),
            "{sent:?}: {relay}"
        );
        for (name, value) in sent.iter().filter(|(_, value)| !value.is_empty()) {
            assert!(
                relay.contains(&format!(r#"name="{name}" value="{value}""#))
                    || relay.contains(&format!(r#"value="{value}""#)),
                "{name}={value} shown again: {relay}"
            );
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "{sent:?}");
    }
}

/// Turning the relay on while it couldn't start (no public address, no
/// handle key) is refused on its switch, saying what is missing, before
/// anything is written.
#[tokio::test]
async fn turning_the_relay_on_without_what_it_needs_is_refused_on_its_switch() {
    let text = "[signup]\nmode = \"public\"\n";
    let (_dir, path, state) = relay_on_file(text, Env::fixed::<&str, &str>([])).await;
    let router = build_router(state);
    let cookie = admin_session_cookie(&router).await;
    let save = post_settings(
        &router,
        &cookie,
        &[("tab", "server"), ("relay.enabled", "true")],
    )
    .await;
    let page = follow(&router, &cookie, save).await;
    assert!(!is_saved(&page), "{}", toast_text(&page));
    let relay = unescaped(card(&page));
    assert!(
        relay.contains(
            "relay.enabled: is on, but the relay can't start: relay.server.public_uri is not set"
        ),
        "{relay}"
    );
    assert!(
        relay.contains("MONOKULO_RELAY_HANDLE_KEY is not set"),
        "{relay}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);

    // With a public address in the same save, the key is still missing.
    let save = post_settings(
        &router,
        &cookie,
        &[
            ("tab", "server"),
            ("relay.enabled", "true"),
            ("relay:server.public_uri", "https://relay.example.org"),
        ],
    )
    .await;
    let page = follow(&router, &cookie, save).await;
    let relay = unescaped(card(&page));
    assert!(
        relay.contains(
            "relay.enabled: is on, but the relay can't start: MONOKULO_RELAY_HANDLE_KEY is not set"
        ),
        "{relay}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
}

/// A save of `server.listen` restarts the relay's listener there: the old
/// address no longer answers, the new one does, the toast and the card say
/// so. Turned off, it stops; on again, it starts.
#[tokio::test]
async fn saving_the_relays_settings_restarts_its_listener_over_http() {
    let (old, new) = (free_port(), free_port());
    let (_dir, path, state) = relay_on_file(&relay_file(old, true), with_key(&[])).await;
    let relay = state.relay.clone();
    let router = build_router(state);
    let cookie = admin_session_cookie(&router).await;
    assert!(relay_answers(old).await);
    let page = server_tab(&router, &cookie, false).await;
    assert!(card(&page).contains("On, since"), "{}", card(&page));
    assert!(card(&page).contains(&format!("Listening on 127.0.0.1:{old}")));

    let listen = format!("127.0.0.1:{new}");
    let save = post_settings(
        &router,
        &cookie,
        &[("tab", "server"), ("relay:server.listen", &listen)],
    )
    .await;
    let page = follow(&router, &cookie, save).await;
    assert!(is_saved(&page), "{}", toast_text(&page));
    assert!(
        toast_text(&page).contains("The push relay restarted its listener."),
        "{}",
        toast_text(&page)
    );
    assert!(
        card(&page).contains(&format!("Listening on {listen}")),
        "{}",
        card(&page)
    );
    assert!(!relay_answers(old).await);
    assert!(relay_answers(new).await);
    assert!(std::fs::read_to_string(&path).unwrap().contains(&listen));

    let save = post_settings(
        &router,
        &cookie,
        &[("tab", "server"), ("relay.enabled", "false")],
    )
    .await;
    let page = follow(&router, &cookie, save).await;
    assert!(
        toast_text(&page).contains("The push relay stopped its listener."),
        "{}",
        toast_text(&page)
    );
    assert!(
        card(&page).contains("relay-state-words\">Off<"),
        "{}",
        card(&page)
    );
    assert!(!relay_answers(new).await);

    let save = post_settings(
        &router,
        &cookie,
        &[("tab", "server"), ("relay.enabled", "true")],
    )
    .await;
    let page = follow(&router, &cookie, save).await;
    assert!(
        toast_text(&page).contains("The push relay started its listener."),
        "{}",
        toast_text(&page)
    );
    assert!(relay_answers(new).await);
    relay.stop().await;
}

/// A save whose restart can't listen (the port is taken) is saved, says the
/// relay didn't start, and the card shows it isn't running and why;
/// monokulo carries on, and the next save that can, starts it.
#[tokio::test]
async fn a_restart_that_fails_is_shown_on_the_card_and_monokulo_carries_on() {
    let (first, taken) = (free_port(), free_port());
    let (_dir, path, state) = relay_on_file(&relay_file(first, true), with_key(&[])).await;
    let relay = state.relay.clone();
    let router = build_router(state);
    let cookie = admin_session_cookie(&router).await;
    let occupied = std::net::TcpListener::bind(("127.0.0.1", taken)).unwrap();
    let listen = format!("127.0.0.1:{taken}");
    let save = post_settings(
        &router,
        &cookie,
        &[("tab", "server"), ("relay:server.listen", &listen)],
    )
    .await;
    let page = follow(&router, &cookie, save).await;
    let toast = toast_text(&page);
    assert!(page.contains(r#"class="toast toast-warning""#), "{toast}");
    assert!(
        toast.contains(&format!(
            "The push relay didn't start: the push relay failed to listen on {listen}:"
        )),
        "{toast}"
    );
    let shown = card(&page);
    assert!(
        shown.contains("relay-state-words\">Not running<"),
        "{shown}"
    );
    assert!(shown.contains("The push relay isn't running."), "{shown}");
    assert!(!shown.contains("Listening on"), "{shown}");
    assert!(
        std::fs::read_to_string(&path).unwrap().contains(&listen),
        "saved, not rolled back"
    );
    // monokulo still serves its pages.
    assert!(card(&server_tab(&router, &cookie, false).await).contains("Not running"));

    drop(occupied);
    let save = post_settings(
        &router,
        &cookie,
        &[
            ("tab", "server"),
            ("relay:server.listen", &format!("127.0.0.1:{first}")),
        ],
    )
    .await;
    let page = follow(&router, &cookie, save).await;
    assert!(
        toast_text(&page).contains("The push relay started its listener."),
        "{}",
        toast_text(&page)
    );
    assert!(relay_answers(first).await);
    relay.stop().await;
}

/// A change to the relay's tables by hand is applied by Reload options
/// file, as a save would: the listener moves, the toast names the key.
#[tokio::test]
async fn reloading_the_options_file_applies_the_relays_tables() {
    let (old, new) = (free_port(), free_port());
    let (_dir, path, state) = relay_on_file(&relay_file(old, true), with_key(&[])).await;
    let relay = state.relay.clone();
    let router = build_router(state);
    let cookie = admin_session_cookie(&router).await;
    std::fs::write(&path, relay_file(new, true)).unwrap();
    let reload = router
        .clone()
        .oneshot(authed_form_request(
            "POST",
            "/admin/settings/reload",
            &cookie,
            &[("tab", "server"), ("owner", "monokulo")],
        ))
        .await
        .unwrap();
    let page = follow(&router, &cookie, reload).await;
    let toast = toast_text(&page);
    assert!(toast.contains("relay.server.listen"), "{toast}");
    assert!(
        toast.contains("The push relay restarted its listener."),
        "{toast}"
    );
    assert!(relay_answers(new).await);
    assert!(!relay_answers(old).await);
    relay.stop().await;
}

/// Without JavaScript an app row's Add and Remove post the tab and come
/// back with the rows changed and everything typed kept, the card opened,
/// nothing saved.
#[tokio::test]
async fn the_app_rows_buttons_work_without_javascript_and_save_nothing() {
    let text = "[signup]\nmode = \"public\"\n";
    let (_dir, path, state) = relay_on_file(text, with_key(&[])).await;
    let router = build_router(state);
    let cookie = admin_session_cookie(&router).await;

    let add = post_settings(
        &router,
        &cookie,
        &[
            ("tab", "server"),
            ("relay:handles.ttl_days", "30"),
            ("relay_apps", ""),
            ("relay_app_0_name", "monokular"),
            ("relay_action", "add-app"),
        ],
    )
    .await;
    assert_eq!(add.status(), StatusCode::SEE_OTHER);
    let location = add.headers()["location"].to_str().unwrap().to_string();
    assert!(
        location.contains("relay=edit") && location.ends_with("#card-relay"),
        "{location}"
    );
    let page = follow(&router, &cookie, add).await;
    let relay = card(&page);
    assert!(
        relay.contains(r#"name="relay_app_0_name" type="text" value="monokular""#),
        "{relay}"
    );
    assert!(relay.contains(r#"name="relay_app_1_name""#), "{relay}");
    assert!(
        relay.contains(r#"id="setting-relay:handles.ttl_days" value="30""#),
        "{relay}"
    );
    assert!(!is_saved(&page));

    let remove = post_settings(
        &router,
        &cookie,
        &[
            ("tab", "server"),
            ("relay_apps", ""),
            ("relay_app_0_name", "monokular"),
            ("relay_app_1_name", "second"),
            ("relay_action", "remove-app:0"),
        ],
    )
    .await;
    let page = follow(&router, &cookie, remove).await;
    let relay = card(&page);
    assert!(
        relay.contains(r#"name="relay_app_0_name" type="text" value="second""#),
        "{relay}"
    );
    assert!(!relay.contains(r#"value="monokular""#), "{relay}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
}
