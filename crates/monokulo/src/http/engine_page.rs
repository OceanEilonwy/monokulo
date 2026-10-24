//! The engine page's handlers, admins only:
//! it shows node addresses and every store's progress. The page is an app
//! (`apps/engine`) that needs JavaScript; these send it data, and it draws
//! and words everything.
//!
//! - `GET /status/engine?network=`: the app's shell: the page's header and
//!   network picker, and where the app goes.
//! - `GET /status/engine/events?network=`: what the app follows: a
//!   `history` event (the network's live frame and every mark kept), then
//!   a `frame` event each time the engine records something, `restarted`
//!   when the history started over (the app reconnects) and `unreachable`
//!   while the engine doesn't answer. Beside them, a `machine` event at
//!   once and then every [`MACHINE_EVERY`] at most: the figures behind the
//!   "Machine and links" strip and the Scanning panel, from monokulo's
//!   cached `/status`.
//! - `GET /status/engine/at?network=&ms=`: the frame at a moment (the
//!   timeline's scrubbing).
//! - `GET /status/engine/replay?network=&from=&to=`: the frames between two
//!   moments (replay), at most a minute of them.
//! - `GET /status/engine/round?network=&number=`: the frame a past round
//!   ended on (a click on the recent rounds).
//!
//! A frame is the state machine's state at a moment, the tuning its rounds
//! are drawn to, and the effects and marks that led there
//! (`engine_view::history::Frame`).

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::Event;
use axum::response::{Html, IntoResponse, Json, Response};
use futures_util::stream::{self, StreamExt as _};
use serde::{Deserialize, Serialize};
use shared::resources::ResourceReport;
use shared::scaling::{LinkSnapshot, NetworkScaling};

use super::{AppState, AuthedAdmin};
use crate::engine_client::{EngineClient, EngineStatusResponse};
use crate::engine_view::history::Frame;
use crate::engine_view::machine::Mark;
use crate::engine_view::relay::Message;
use crate::views;

/// The `machine` event comes at most this often: it is built from
/// monokulo's cached `/status` (itself at most one engine request every
/// ten seconds), not from each frame, so a busy scan costs it nothing more.
/// Monokulo runs this; the relay's [`Timing`](crate::engine_view::relay::Timing)
/// carries it, so a test can pass another explicitly.
pub const MACHINE_EVERY: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize)]
pub struct NetworkQuery {
    network: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RoundQuery {
    network: String,
    number: u64,
}

#[derive(Debug, Deserialize)]
pub struct AtQuery {
    network: String,
    ms: i64,
}

#[derive(Debug, Deserialize)]
pub struct ReplayQuery {
    network: String,
    from: i64,
    to: i64,
}

/// What the app starts from.
#[derive(Serialize)]
struct Start<'a> {
    epoch: &'a str,
    /// The engine's clock, so the app can tell how far behind it plays.
    engine_now_ms: i64,
    /// The oldest moment that can be shown.
    oldest_ms: Option<i64>,
    frame: Frame,
    marks: Vec<&'a Mark>,
}

/// The networks the engine scans, in its order.
async fn networks(state: &AppState) -> Result<Vec<String>, String> {
    super::status_page::get_status_cached(&state.engine)
        .await
        .map(|status| network_names(&status))
}

fn network_names(status: &EngineStatusResponse) -> Vec<String> {
    status.networks.iter().map(|n| n.network.clone()).collect()
}

/// The figures behind the "Machine and links" strip and the Scanning
/// panel for one network, from monokulo's cached `/status`: the app
/// charts and words them.
#[derive(Debug, Serialize)]
pub(crate) struct Machine<'a> {
    network: &'a str,
    /// When the engine wrote the `/status` this is from (its clock).
    at_unix: i64,
    /// Now, by monokulo's clock: where the charts' hour ends.
    now_unix: i64,
    resources: Resources,
    /// The node the engine reads the network's chain from right now.
    node: Option<NodeInUse<'a>>,
    scaling: Option<&'a NetworkScaling>,
}

/// Both processes' CPU and memory.
#[derive(Debug, Serialize)]
struct Resources {
    /// `None` when the engine didn't report its own (or didn't answer).
    engine: Option<ResourceReport>,
    monokulo: ResourceReport,
    /// The engine runs inside monokulo: one process, its CPU split by
    /// thread (`ResourceReport::hosted`) and its memory shared.
    one_process: bool,
}

#[derive(Debug, Serialize)]
struct NodeInUse<'a> {
    /// The engine's label for it (`host:port`).
    label: &'a str,
    /// Not the network's primary: the engine fell back to it.
    fallback: bool,
    link: Option<&'a LinkSnapshot>,
}

/// Both processes' CPU and memory. Inside monokulo, the engine's
/// `/status` reports the same process as monokulo's own sampler, so the
/// one report is split by thread instead: counted twice, it would double.
fn resources(client: &EngineClient, status: &EngineStatusResponse) -> Resources {
    let process = shared::resources::sampler().report();
    if client.is_embedded() {
        Resources {
            engine: Some(process.hosted()),
            monokulo: process.without_hosted(),
            one_process: true,
        }
    } else {
        Resources {
            engine: status.resources.clone(),
            monokulo: process,
            one_process: false,
        }
    }
}

/// What the "Machine and links" strip and the Scanning panel show for
/// `network`: the machine's CPU and memory, the link to the node the
/// engine is using right now (a fallback said so; standby nodes have no
/// figures), and the network's scan.
pub(crate) fn machine<'a>(
    client: &EngineClient,
    status: &'a EngineStatusResponse,
    network: &'a str,
) -> Machine<'a> {
    let reported = status.networks.iter().find(|n| n.network == network);
    let node = reported.and_then(|reported| {
        let at = reported.nodes.iter().position(|node| node.is_active)?;
        let node = &reported.nodes[at];
        Some(NodeInUse {
            label: &node.label,
            fallback: at > 0,
            link: node.link.as_ref(),
        })
    });
    Machine {
        network,
        at_unix: status.generated_at,
        now_unix: shared::time::now_unix(),
        resources: resources(client, status),
        node,
        scaling: reported.and_then(|reported| reported.scaling.as_ref()),
    }
}

/// The `machine` event for `network`, or nothing while `/status` can't be
/// read.
async fn machine_event(state: &AppState, network: &str) -> Option<Event> {
    let status = tokio::time::timeout(
        crate::engine_client::ENGINE_CALL_TIMEOUT,
        super::status_page::get_status_cached(&state.engine),
    )
    .await
    .ok()?
    .ok()?;
    Event::default()
        .event("machine")
        .json_data(machine(&state.engine.client, &status, network))
        .ok()
}

/// `network` if the engine scans it, else `None`.
fn known(networks: &[String], network: &str) -> Option<String> {
    networks.iter().find(|n| *n == network).cloned()
}

pub async fn page(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    Query(query): Query<NetworkQuery>,
) -> Response {
    let chrome = super::page_chrome(&state, Some(&admin), "/status/engine".to_owned()).await;
    let networks = match networks(&state).await {
        Ok(networks) => networks,
        Err(error) => {
            return Html(
                views::engine::page(
                    &chrome,
                    &views::engine::EnginePage {
                        networks: Vec::new(),
                        network: String::new(),
                        error: Some(error),
                    },
                )
                .into_string(),
            )
            .into_response();
        }
    };
    let network = match query.network.as_deref() {
        Some(asked) => match known(&networks, asked) {
            Some(network) => network,
            None => return StatusCode::NOT_FOUND.into_response(),
        },
        None => networks.first().cloned().unwrap_or_default(),
    };
    let error = network
        .is_empty()
        .then(|| "no Monero node is configured on this engine".to_owned());
    Html(
        views::engine::page(
            &chrome,
            &views::engine::EnginePage {
                networks,
                network,
                error,
            },
        )
        .into_string(),
    )
    .into_response()
}

pub async fn events(
    State(state): State<AppState>,
    AuthedAdmin(_, _): AuthedAdmin,
    extensions: axum::http::Extensions,
    Query(query): Query<NetworkQuery>,
) -> Response {
    let permit = match extensions.get::<crate::abuse::ClientIdentity>() {
        Some(client) => match state.abuse.streams.try_acquire(client, "engine") {
            Some(permit) => Some(permit),
            None => {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    "too many open update streams",
                )
                    .into_response()
            }
        },
        None => None,
    };
    let networks = networks(&state).await.unwrap_or_default();
    let Some(network) = query.network.as_deref().and_then(|n| known(&networks, n)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let relay = Arc::clone(&state.engine.activity);
    let watch = match relay.watch(&network).await {
        Ok(watch) => watch,
        Err(error) => return (StatusCode::BAD_GATEWAY, error.to_string()).into_response(),
    };
    let start = relay
        .read(&network, |history| {
            Event::default().event("history").json_data(Start {
                epoch: history.epoch(),
                engine_now_ms: history.engine_now_ms(),
                oldest_ms: history.oldest_ms(),
                frame: history.live_frame(),
                marks: history.marks().collect(),
            })
        })
        .await;
    let first = match start {
        Ok(Ok(event)) => event,
        Ok(Err(error)) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response()
        }
        Err(error) => return (StatusCode::BAD_GATEWAY, error.to_string()).into_response(),
    };
    // Frames as the engine records them; between them, the `machine`
    // event once straight away and then every `machine_every` at most.
    let follow = stream::unfold(
        (watch, permit, state, network, tokio::time::Instant::now()),
        |(mut watch, permit, state, network, mut next_machine)| async move {
            loop {
                let event = tokio::select! {
                    message = watch.messages.recv() => match message {
                        Ok(Message::Frame(_, json)) => Event::default().event("frame").data(&*json),
                        // Fell behind (a slow reader): start over from the history.
                        Ok(Message::Restarted)
                        | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            Event::default().event("restarted").data("")
                        }
                        Ok(Message::Unreachable(error)) => {
                            Event::default().event("unreachable").data(error)
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                    },
                    () = tokio::time::sleep_until(next_machine) => {
                        next_machine =
                            tokio::time::Instant::now() + state.engine.activity.timing().machine_every;
                        match machine_event(&state, &network).await {
                            Some(event) => event,
                            None => continue,
                        }
                    }
                };
                return Some((
                    Ok::<_, Infallible>(event),
                    (watch, permit, state, network, next_machine),
                ));
            }
        },
    );
    crate::live::sse(stream::once(async move { Ok(first) }).chain(follow))
}

pub async fn at(
    State(state): State<AppState>,
    AuthedAdmin(_, _): AuthedAdmin,
    Query(query): Query<AtQuery>,
) -> Response {
    let networks = networks(&state).await.unwrap_or_default();
    let Some(network) = known(&networks, &query.network) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match state
        .engine
        .activity
        .read(&network, |history| history.frame_at(query.ms))
        .await
    {
        Ok(frame) => Json(frame).into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, error.to_string()).into_response(),
    }
}

/// The frame a past round ended on, which the round card draws: 404 once
/// it has left the history.
pub async fn round(
    State(state): State<AppState>,
    AuthedAdmin(_, _): AuthedAdmin,
    Query(query): Query<RoundQuery>,
) -> Response {
    let networks = networks(&state).await.unwrap_or_default();
    let Some(network) = known(&networks, &query.network) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match state
        .engine
        .activity
        .read(&network, |history| history.round(query.number))
        .await
    {
        Ok(Some(round)) => Json(round).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, error.to_string()).into_response(),
    }
}

pub async fn replay(
    State(state): State<AppState>,
    AuthedAdmin(_, _): AuthedAdmin,
    Query(query): Query<ReplayQuery>,
) -> Response {
    let networks = networks(&state).await.unwrap_or_default();
    let Some(network) = known(&networks, &query.network) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match state
        .engine
        .activity
        .read(&network, |history| history.frames(query.from, query.to))
        .await
    {
        Ok(frames) => Json(frames).into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, error.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use axum::Router;
    use http_body_util::BodyExt as _;
    use monero::Network;
    use shared::activity::{Event, Snapshot, StoreGroup};
    use tower::ServiceExt as _;

    use super::*;
    use crate::engine_client::EngineClient;
    use crate::engine_view::relay::{Relay, Timing};
    use crate::http::test_support::{
        admin_session_cookie, body_json, body_text, signed_up_and_logged_in_session_token,
    };

    /// monokulo with a real engine that scans stagenet, whose record holds
    /// a snapshot, a commit and a reorg; and an admin's session.
    async fn site() -> (engine_test_support::TestEngineHandle, Router, String) {
        site_with(Timing::DEFAULT).await
    }

    /// [`site`], its relay following with `timing`.
    async fn site_with(timing: Timing) -> (engine_test_support::TestEngineHandle, Router, String) {
        let engine =
            engine_test_support::spawn_test_engine_with_networks(&[Network::Stagenet]).await;
        let activity = engine.activity(Network::Stagenet);
        activity.record(Event::Snapshot(Box::new(Snapshot {
            tip: Some(3_412_880),
            high_water: Some(3_412_880),
            groups: vec![StoreGroup {
                cursor: 3_412_880,
                stores: 41,
            }],
            ..Snapshot::default()
        })));
        // Apart in time, so a moment between them can be asked for.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        activity.record(Event::RoundStarted {
            round: 7,
            budget_ms: 10_000,
            tip: Some(3_412_881),
        });
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        activity.record(Event::Committed {
            height: 3_412_881,
            group: shared::activity::Group::Frontier,
            stores: 41,
            matches: 1,
            idle_moved: 0,
            header_only: false,
        });
        let client = EngineClient::embedded_for_tests(engine.router());
        let state = AppState {
            engine: crate::http::Engine {
                activity: Arc::new(Relay::with_timing(client.clone(), timing).unwrap()),
                ..crate::http::Engine::new(client)
            },
            ..AppState::for_tests()
        };
        let router = crate::http::build_router(state);
        let cookie = admin_session_cookie(&router).await;
        (engine, router, cookie)
    }

    async fn get(router: &Router, uri: &str, cookie: Option<&str>) -> axum::response::Response {
        let mut request = Request::builder().uri(uri);
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    /// The page is for admins: a visitor is turned away, and so is a
    /// signed-in merchant.
    #[tokio::test]
    async fn only_admins_see_the_engine_page() {
        let (_engine, router, _) = site().await;
        for uri in [
            "/status/engine",
            "/status/engine/events?network=stagenet",
            "/status/engine/at?network=stagenet&ms=0",
            "/status/engine/replay?network=stagenet&from=0&to=1",
            "/status/engine/round?network=stagenet&number=7",
        ] {
            let anonymous = get(&router, uri, None).await;
            assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED, "{uri}");
        }
        let merchant = signed_up_and_logged_in_session_token(
            &router,
            "merchant@example.com",
            "a long enough password",
        )
        .await;
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/status/engine")
                    .header("cookie", format!("session={merchant}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// A past round: the frame it ended on, with the tuning it is drawn to;
    /// 404 once it has left the history.
    #[tokio::test]
    async fn a_past_round_is_its_last_frame() {
        let (_engine, router, cookie) = site().await;
        let response = get(
            &router,
            "/status/engine/round?network=stagenet&number=7",
            Some(&cookie),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let frame = body_json(response).await;
        assert_eq!(frame["state"]["round"]["number"], 7);
        assert_eq!(frame["tuning"]["round_ms"], 10_000);
        let gone = get(
            &router,
            "/status/engine/round?network=stagenet&number=99",
            Some(&cookie),
        )
        .await;
        assert_eq!(gone.status(), StatusCode::NOT_FOUND);
    }

    /// An engine that can't be reached is said so on the page, not a 500.
    #[tokio::test]
    async fn an_unreachable_remote_engine_is_said_so_over_http() {
        let state = AppState {
            engine: crate::http::Engine::new(EngineClient::for_tests(format!(
                "http://{}",
                shared::unreachable::address()
            ))),
            ..AppState::for_tests()
        };
        let router = crate::http::build_router(state);
        let cookie = admin_session_cookie(&router).await;
        let response = get(&router, "/status/engine", Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(body_text(response)
            .await
            .contains("The engine's activity could not be read"));
    }

    /// The stream opens with the history: the live frame and every mark.
    #[tokio::test]
    async fn the_stream_opens_with_the_history() {
        let (_engine, router, cookie) = site().await;
        let response = get(
            &router,
            "/status/engine/events?network=stagenet",
            Some(&cookie),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body();
        let mut text = String::new();
        while !text.contains("\n\n") {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(10), body.frame())
                .await
                .expect("the history within 10 s")
                .expect("a frame")
                .unwrap();
            if let Ok(data) = frame.into_data() {
                text.push_str(&String::from_utf8_lossy(&data));
            }
        }
        assert!(text.starts_with("event: history\n"), "{text}");
        let data: serde_json::Value = serde_json::from_str(
            text.lines()
                .find_map(|line| line.strip_prefix("data: "))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(data["marks"].as_array().unwrap().len(), 2);
        assert_eq!(data["marks"][1]["what"]["kind"], "committed");
        assert_eq!(data["frame"]["state"]["chain"]["tip"], 3_412_881);
        assert_eq!(
            data["frame"]["tuning"]["shares"].as_array().unwrap().len(),
            5
        );
        assert!(data["epoch"]
            .as_str()
            .is_some_and(|epoch| !epoch.is_empty()));
    }

    /// Scrubbing and replay answer from the history: before the commit, the
    /// high-water mark was lower; the replay over the commit carries its
    /// effects and mark.
    #[tokio::test]
    async fn scrubbing_and_replay_answer_from_the_history() {
        let (engine, router, cookie) = site().await;
        let page = engine.activity(Network::Stagenet).page(None);
        let (first, last) = (page.events[0].at_ms, page.events[2].at_ms);
        let before = body_json(
            get(
                &router,
                &format!("/status/engine/at?network=stagenet&ms={first}"),
                Some(&cookie),
            )
            .await,
        )
        .await;
        assert_eq!(before["state"]["chain"]["high_water"], 3_412_880);
        let after = body_json(
            get(
                &router,
                &format!("/status/engine/at?network=stagenet&ms={last}"),
                Some(&cookie),
            )
            .await,
        )
        .await;
        assert_eq!(after["state"]["chain"]["high_water"], 3_412_881);
        let frames = body_json(
            get(
                &router,
                &format!(
                    "/status/engine/replay?network=stagenet&from={}&to={last}",
                    first - 1
                ),
                Some(&cookie),
            )
            .await,
        )
        .await;
        let frames = frames.as_array().unwrap();
        assert!(!frames.is_empty());
        let found: Vec<&serde_json::Value> = frames
            .iter()
            .flat_map(|f| f["marks"].as_array().unwrap())
            .filter(|m| m["what"]["kind"] == "committed")
            .map(|m| &m["what"]["found"])
            .collect();
        assert_eq!(found, [&serde_json::json!(1)], "{frames:?}");
        let unknown = get(
            &router,
            "/status/engine/at?network=mainnet&ms=0",
            Some(&cookie),
        )
        .await;
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    }

    /// A `/status` with stagenet's two nodes, the one in use at `active`,
    /// its link measured.
    fn status_with_nodes(active: usize) -> EngineStatusResponse {
        let link = serde_json::json!({
            "measured": true, "rtt_ms": 120, "ttfb_per_block_ms": 45, "rate_bytes_per_sec": 387_500,
            "bytes_per_block": 50_000, "last_measured_unix": null, "timeouts_last_hour": 0,
            "failures_last_hour": 0, "history": []
        });
        let node = |label: &str, at: usize| {
            serde_json::json!({
                "label": label, "is_active": at == active, "height": 5, "error": null,
                "link": if at == active { link.clone() } else { serde_json::json!({
                    "measured": true, "rtt_ms": 999, "ttfb_per_block_ms": 999, "rate_bytes_per_sec": 1,
                    "bytes_per_block": 1, "last_measured_unix": null, "timeouts_last_hour": 0,
                    "failures_last_hour": 0, "history": []
                }) }
            })
        };
        serde_json::from_value(serde_json::json!({
            "networks": [{
                "network": "stagenet",
                "nodes": [node("node.example.com:38081", 0), node("backup.example.org:38089", 1)],
                "scanner": {
                    "ever_ticked": true, "last_tick_started_at": 1, "last_tick_finished_at": 2, "tick_count": 3,
                    "tenants_scanned": 1, "last_tick_ok": true, "last_error": null, "is_stale": false
                }
            }],
            "poll_interval_secs": 2,
            "generated_at": 1_800_000_000
        }))
        .unwrap()
    }

    /// The machine figures follow the node the engine is using now, a
    /// fallback said so; the standby node's figures aren't sent.
    #[test]
    fn the_machine_figures_follow_the_node_in_use_a_fallback_too() {
        let client = EngineClient::for_tests("http://engine.invalid");
        let json = |status: &EngineStatusResponse, network: &str| {
            serde_json::to_value(machine(&client, status, network)).unwrap()
        };
        let primary_status = status_with_nodes(0);
        let primary = json(&primary_status, "stagenet");
        assert_eq!(primary["node"]["label"], "node.example.com:38081");
        assert_eq!(primary["node"]["fallback"], false);
        assert_eq!(primary["node"]["link"]["rate_bytes_per_sec"], 387_500);
        assert_eq!(primary["at_unix"], 1_800_000_000);
        assert!(primary["resources"]["monokulo"]["samples"].is_array());

        let fallback_status = status_with_nodes(1);
        let fallback = json(&fallback_status, "stagenet");
        assert_eq!(fallback["node"]["label"], "backup.example.org:38089");
        assert_eq!(fallback["node"]["fallback"], true);
        assert_eq!(fallback["node"]["link"]["ttfb_per_block_ms"], 45);
        let text = fallback.to_string();
        assert!(
            !text.contains("node.example.com"),
            "the standby node: {text}"
        );
        // The standby's figures (999) aren't the node's; only the node is
        // checked, as the rest carries the clock and this machine's figures.
        let node = fallback["node"].to_string();
        assert!(!node.contains("999"), "{node}");

        let elsewhere = json(&primary_status, "mainnet");
        assert!(elsewhere["node"].is_null());
        assert!(elsewhere["scaling"].is_null());
    }

    /// Reads the stream's next event: its name and data.
    async fn next_event(body: &mut Body, buffer: &mut String) -> (String, String) {
        loop {
            if let Some(end) = buffer.find("\n\n") {
                let block: String = buffer.drain(..end + 2).collect();
                let name = block
                    .lines()
                    .find_map(|line| line.strip_prefix("event: "))
                    .unwrap_or_default()
                    .to_owned();
                let data = block
                    .lines()
                    .filter_map(|line| line.strip_prefix("data: "))
                    .collect::<Vec<_>>()
                    .join("\n");
                if name.is_empty() {
                    continue; // a keep-alive
                }
                return (name, data);
            }
            let frame = body.frame().await.expect("a frame").unwrap();
            if let Ok(data) = frame.into_data() {
                buffer.push_str(&String::from_utf8_lossy(&data));
            }
        }
    }

    async fn read(body: &mut Body, buffer: &mut String) -> (String, String) {
        tokio::time::timeout(Duration::from_secs(15), next_event(body, buffer))
            .await
            .expect("an event within 15 s")
    }

    /// The stream sends the strip's `machine` event at once, after the
    /// history, and then at most every `machine_every` however many frames
    /// come between: it is built from the cached `/status`, not per frame.
    /// Monokulo waits [`MACHINE_EVERY`]; this passes a shorter interval.
    #[tokio::test]
    async fn the_machine_event_comes_at_once_then_at_most_every_few_seconds() {
        let timing = Timing {
            machine_every: Duration::from_millis(400),
            ..Timing::DEFAULT
        };
        let (engine, router, cookie) = site_with(timing).await;
        let response = get(
            &router,
            "/status/engine/events?network=stagenet",
            Some(&cookie),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body();
        let mut buffer = String::new();
        assert_eq!(read(&mut body, &mut buffer).await.0, "history");
        let (name, data) = read(&mut body, &mut buffer).await;
        assert_eq!(name, "machine");
        let machine: serde_json::Value = serde_json::from_str(&data).unwrap();
        assert_eq!(machine["network"], "stagenet");
        assert!(machine["resources"]["monokulo"].is_object());
        assert_eq!(machine["resources"]["one_process"], true);
        let first = tokio::time::Instant::now();

        // Frames as the engine records things; no machine event among them.
        let activity = engine.activity(Network::Stagenet);
        for round in 8..12 {
            activity.record(Event::RoundStarted {
                round,
                budget_ms: 10_000,
                tip: Some(3_412_881),
            });
        }
        let mut frames = 0;
        let second = loop {
            let (name, _) = read(&mut body, &mut buffer).await;
            match name.as_str() {
                "frame" => frames += 1,
                "machine" => break first.elapsed(),
                other => panic!("{other}"),
            }
        };
        assert!(frames > 0, "the frames came through");
        assert!(
            second >= timing.machine_every - Duration::from_millis(50),
            "the next machine event waited: {second:?}"
        );
    }
}
