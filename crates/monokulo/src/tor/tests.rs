use live_settings::Reloadable as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;
use crate::abuse::ClientIdentity;
use crate::http::{build_router, AppState};

/// One HTTP request over a stream handed to a [`TorListener`], as Arti
/// hands a visitor's stream over, and the raw response.
async fn request_over_tor(router: axum::Router, request: &str) -> String {
    let (connections, listener) = TorListener::new();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    tokio::spawn(serve(listener, router, stopped));
    let (mut visitor, server_side) = tokio::io::duplex(64 * 1024);
    assert!(connections.send(server_side, circuit_peer(3)).await);
    visitor.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    visitor.read_to_end(&mut response).await.unwrap();
    String::from_utf8_lossy(&response).into_owned()
}

#[tokio::test]
async fn a_stream_from_tor_is_served_as_a_visitor_on_the_onion_address() {
    let response = request_over_tor(
        build_router(AppState::for_tests()),
        "GET /login HTTP/1.1\r\nHost: x.onion\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    // Only the onion network says this (`network::Onion`).
    assert!(
        response
            .to_lowercase()
            .contains("referrer-policy: no-referrer\r\n"),
        "{response}"
    );
}

#[test]
fn each_rendezvous_circuit_is_its_own_visitor_as_with_an_external_tor() {
    assert_eq!(circuit_peer(1).identity(), ClientIdentity::Circuit(1));
    assert_eq!(
        circuit_peer(0xaabb_ccdd).identity(),
        ClientIdentity::Circuit(0xaabb_ccdd)
    );
    assert_ne!(circuit_peer(1).identity(), circuit_peer(2).identity());
}

/// The problem loading `options` reports, or `None` when it loads.
async fn problem_loading(options: &str) -> Option<String> {
    let state = AppState::for_tests();
    crate::settings::MonokuloSettings::load(
        state.db.clone(),
        state.engine.client.clone(),
        state.exchange_rate.clone(),
        state.abuse.clone(),
        None,
        crate::settings::test_secrets(),
        live_settings::OptionsFile::in_memory(options),
    )
    .await
    .err()
}

const ONION_URI: &str = "http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";

#[tokio::test]
async fn each_mode_refuses_what_it_does_not_use_saying_what_to_change() {
    // An external tor needs its port and its address.
    let problem = problem_loading("[tor]\nmode = \"external\"\n")
        .await
        .unwrap();
    assert!(problem.contains("set onion_uri"), "{problem}");
    assert!(problem.contains("set abuse.onion_listener"), "{problem}");
    let complete = format!(
        "onion_uri = \"{ONION_URI}\"\n[tor]\nmode = \"external\"\n[abuse]\nonion_listener = \"127.0.0.1:8082\"\n"
    );
    assert_eq!(problem_loading(&complete).await, None);

    // An onion listener left from before tor.mode existed: told to say so.
    let problem = problem_loading("[abuse]\nonion_listener = \"127.0.0.1:8082\"\n")
        .await
        .unwrap();
    assert!(problem.contains("set tor.mode = external"), "{problem}");

    // Tor inside monokulo makes its own address and needs no port.
    let problem = problem_loading(&format!(
        "onion_uri = \"{ONION_URI}\"\n[tor]\nmode = \"inside\"\n"
    ))
    .await
    .unwrap();
    assert!(problem.contains("leave onion_uri empty"), "{problem}");

    let problem = problem_loading("[tor]\nhidden_service_dir = \"/srv/hs\"\n")
        .await
        .unwrap();
    assert!(problem.contains("tor.mode = inside"), "{problem}");
}

#[tokio::test]
async fn an_onion_address_in_public_uri_is_moved_to_onion_uri() {
    let problem = problem_loading(&format!("public_uri = \"{ONION_URI}\"\n"))
        .await
        .unwrap();
    assert!(problem.contains("put it in onion_uri"), "{problem}");
}

fn inside(state_dir: &Path, hidden_service_dir: Option<PathBuf>) -> TorConfig {
    TorConfig {
        mode: TorMode::Inside,
        state_dir: state_dir.to_path_buf(),
        hidden_service_dir,
        ..TorConfig::defaults()
    }
}

#[tokio::test]
async fn starting_makes_a_private_state_folder_and_needs_a_real_hidden_service_dir() {
    let dir = tempfile_dir("tor-prepare");
    let state_dir = dir.join("tor");
    let reloadable = TorReloadable::new(Tor::default());
    let (change, _) = reloadable
        .prepare(&inside(&state_dir, None), &TorConfig::defaults())
        .await
        .unwrap();
    assert!(matches!(change, TorChange::Start(_)));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&state_dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
    }

    let empty = dir.join("not-a-hidden-service");
    std::fs::create_dir_all(&empty).unwrap();
    let refused = reloadable
        .prepare(
            &inside(&state_dir, Some(empty.clone())),
            &TorConfig::defaults(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(refused.key, TOR_HIDDEN_SERVICE_DIR.key);
    std::fs::write(empty.join(CTOR_SECRET_KEY_FILE), b"key").unwrap();
    assert!(reloadable
        .prepare(&inside(&state_dir, Some(empty)), &TorConfig::defaults())
        .await
        .is_ok());

    // Off or external: nothing to run.
    let (change, _) = reloadable
        .prepare(&TorConfig::defaults(), &TorConfig::defaults())
        .await
        .unwrap();
    assert!(matches!(change, TorChange::Stop));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_start_asked_for_before_the_router_exists_waits_for_it() {
    let tor = Tor::default();
    let wanted = Wanted {
        state_dir: PathBuf::from("/nonexistent"),
        hidden_service_dir: None,
    };
    tor.start(wanted.clone());
    assert_eq!(tor.running_with(), Some(wanted));
    assert_eq!(
        tor.status(),
        TorStatus::Off,
        "nothing runs until the router is ready"
    );
    tor.stop();
    assert_eq!(tor.running_with(), None);
}

/// A fresh folder for one test.
fn tempfile_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("monokulo-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
