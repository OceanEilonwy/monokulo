//! The engine page's handlers (`docs/engine_visualizer.md`), admins only:
//! it shows node addresses and every store's progress.
//!
//! - `GET /status/engine?network=`: the page, whole, as of now.
//! - `GET /status/engine/events?network=`: what the page's script follows:
//!   a `history` event (the network's live frame and every mark kept), then
//!   a `frame` event each time the engine records something, `restarted`
//!   when the history started over (the script reconnects) and
//!   `unreachable` while the engine doesn't answer. Beside them, a
//!   `machine` event at once and then every [`MACHINE_EVERY`] at most: the
//!   "Machine and links" strip and the Scanning panel, from monokulo's
//!   cached `/status` (decision D38 in docs/engine_visualizer_decisions.md).
//! - `GET /status/engine/at?network=&ms=`: what the page drew at a moment
//!   (the timeline's scrubbing).
//! - `GET /status/engine/replay?network=&from=&to=`: the frames between two
//!   moments (replay), at most a minute of them.
//! - `GET /status/engine/round?network=&number=`: a past round as the round
//!   card draws it (a click on the recent rounds). Without JavaScript the
//!   recent rounds link to the page with `&round=`.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::Event;
use axum::response::{Html, IntoResponse, Json, Response};
use futures_util::stream::{self, StreamExt as _};
use serde::{Deserialize, Serialize};

use super::{AppState, AuthedAdmin};
use crate::engine_client::{EngineClient, EngineStatusResponse};
use crate::engine_view::history::Frame;
use crate::engine_view::present::{mark_view, MarkView};
use crate::engine_view::relay::Message;
use crate::views;
use crate::views::scaling::{self, MachineView, ResourcesView};

/// The `machine` event comes at most this often: it is built from
/// monokulo's cached `/status` (itself at most one engine request every
/// ten seconds), not from each frame, so a busy scan costs it nothing more.
pub const MACHINE_EVERY: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize)]
pub struct NetworkQuery {
    network: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PageQuery {
    network: Option<String>,
    /// A past round to show in the round card.
    round: Option<u64>,
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

/// What the script starts from.
#[derive(Serialize)]
struct Start<'a> {
    epoch: &'a str,
    /// The engine's clock, so the script can tell how far behind it plays.
    engine_now_ms: i64,
    /// The oldest moment that can be shown.
    oldest_ms: Option<i64>,
    frame: Frame,
    marks: Vec<MarkView>,
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

/// Both processes' CPU and memory. Inside monokulo, the engine's
/// `/status` reports the same process as monokulo's own sampler, so the
/// one report is split by thread instead: counted twice, it would double.
fn resources_view(client: &EngineClient, status: &EngineStatusResponse, now: i64) -> ResourcesView {
    let process = shared::resources::sampler().report();
    if client.is_embedded() {
        ResourcesView {
            engine: Some(process.hosted()),
            monokulo: process.without_hosted(),
            now_unix: now,
            one_process: true,
        }
    } else {
        ResourcesView {
            engine: status.resources.clone(),
            monokulo: process,
            now_unix: now,
            one_process: false,
        }
    }
}

/// What the "Machine and links" strip and the Scanning panel show for
/// `network`: the machine's CPU and memory, the link to the node the
/// engine is using right now (a fallback said so; standby nodes have no
/// figures), the blocks' size and the scan.
pub(crate) fn machine_view(
    client: &EngineClient,
    status: &EngineStatusResponse,
    network: &str,
) -> MachineView {
    let now = shared::time::now_unix();
    let mut tiles = scaling::resource_tiles(&resources_view(client, status, now));
    let stacked = tiles.iter().any(
        |tile| matches!(&tile.chart, scaling::TileChart::Stack { layers, .. } if layers.len() > 1),
    );
    let reported = status.networks.iter().find(|n| n.network == network);
    let in_use = reported.and_then(|reported| {
        let at = reported.nodes.iter().position(|node| node.is_active)?;
        let node = &reported.nodes[at];
        Some(scaling::InUseNode {
            label: &node.label,
            fallback: at > 0,
            link: node.link.as_ref(),
        })
    });
    tiles.extend(scaling::link_tiles(in_use, now));
    let scaling_of = reported.and_then(|reported| reported.scaling.as_ref());
    tiles.push(scaling::block_size_tile(scaling_of));
    let active = in_use.map(|node| scaling::ActiveNode {
        label: node.label,
        rate_bytes_per_sec: node
            .link
            .filter(|link| link.measured)
            .map(|link| link.rate_bytes_per_sec),
    });
    MachineView {
        tiles,
        stacked,
        scanning: scaling_of.map(|scan| scaling::scanning(network, scan, active)),
        at_unix: status.generated_at,
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
        .json_data(machine_view(&state.engine.client, &status, network))
        .ok()
}

/// `network` if the engine scans it, else `None`.
fn known(networks: &[String], network: &str) -> Option<String> {
    networks.iter().find(|n| *n == network).cloned()
}

pub async fn page(
    State(state): State<AppState>,
    AuthedAdmin(admin, _): AuthedAdmin,
    Query(query): Query<PageQuery>,
) -> Response {
    let chrome = super::page_chrome(&state, Some(&admin), "/status/engine".to_owned()).await;
    let status = match super::status_page::get_status_cached(&state.engine).await {
        Ok(status) => status,
        Err(error) => {
            return Html(
                views::engine::page(
                    &chrome,
                    &views::engine::EnginePage {
                        networks: Vec::new(),
                        network: String::new(),
                        view: None,
                        machine: None,
                        pinned: None,
                        marks: Vec::new(),
                        error: Some(error),
                    },
                )
                .into_string(),
            )
            .into_response();
        }
    };
    let networks = network_names(&status);
    let network = match query.network.as_deref() {
        Some(asked) => match known(&networks, asked) {
            Some(network) => network,
            None => return StatusCode::NOT_FOUND.into_response(),
        },
        None => networks.first().cloned().unwrap_or_default(),
    };
    let read = if network.is_empty() {
        Err("no Monero node is configured on this engine".to_owned())
    } else {
        state
            .engine
            .activity
            .read(&network, |history| {
                let marks: Vec<MarkView> = history
                    .marks()
                    .rev()
                    .take(views::engine::MARKS_SHOWN)
                    .map(mark_view)
                    .collect();
                let pinned = query.round.and_then(|number| history.round(number));
                (history.live_frame().view, pinned, marks)
            })
            .await
            .map_err(|error| error.to_string())
    };
    let (view, pinned, marks, error) = match read {
        Ok((view, pinned, marks)) => (Some(view), pinned, marks, None),
        Err(error) => (None, None, Vec::new(), Some(error)),
    };
    let machine =
        (!network.is_empty()).then(|| machine_view(&state.engine.client, &status, &network));
    Html(
        views::engine::page(
            &chrome,
            &views::engine::EnginePage {
                networks,
                network,
                view,
                machine,
                pinned,
                marks,
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
                marks: history.marks().map(mark_view).collect(),
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
    // event once straight away and then every MACHINE_EVERY at most.
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
                        next_machine = tokio::time::Instant::now() + MACHINE_EVERY;
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

/// A past round, as the round card draws it: 404 once it has left the
/// history.
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
    use crate::http::test_support::{
        admin_session_cookie, body_json, body_text, signed_up_and_logged_in_session_token,
    };

    /// monokulo with a real engine that scans stagenet, whose record holds
    /// a snapshot, a commit and a reorg; and an admin's session.
    async fn site() -> (engine_test_support::TestEngineHandle, Router, String) {
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
        let state = AppState {
            engine: crate::http::Engine::new(EngineClient::embedded_for_tests(engine.router())),
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
                    .header("authorization", format!("Bearer {merchant}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// Without JavaScript, the page is the network as of now: the summary,
    /// the chain with its groups, the round and the events in words, and a
    /// Reload button. Its script and timeline come with it.
    #[tokio::test]
    async fn the_page_shows_the_network_as_of_now() {
        let (_engine, router, cookie) = site().await;
        let response = get(&router, "/status/engine", Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        for expected in [
            "3,412,881",
            "Frontier, 41 stores",
            "Round 7",
            "Block 3,412,881 scanned for 41 stores and committed, 1 payment found in it.",
            "The node has a new block: 3,412,881.",
            "id=\"tl\"",
            "Refresh page",
            "data-network=\"stagenet\"",
            // The switcher: the network scanned is a link, the others say why not.
            "<option value=\"stagenet\" selected>",
            "Mainnet - no node configured",
        ] {
            assert!(html.contains(expected), "no {expected:?} in the page");
        }
        let unknown = get(&router, "/status/engine?network=mainnet", Some(&cookie)).await;
        assert_eq!(
            unknown.status(),
            StatusCode::NOT_FOUND,
            "the engine scans no mainnet"
        );
    }

    /// A past round: as JSON for the script, and in the page's round card
    /// with a chip back to live for a page asked for with `&round=`.
    #[tokio::test]
    async fn a_past_round_is_shown_on_request() {
        let (_engine, router, cookie) = site().await;
        let response = get(
            &router,
            "/status/engine/round?network=stagenet&number=7",
            Some(&cookie),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["title"], "Round 7");
        let gone = get(
            &router,
            "/status/engine/round?network=stagenet&number=99",
            Some(&cookie),
        )
        .await;
        assert_eq!(gone.status(), StatusCode::NOT_FOUND);

        let pinned = body_text(
            get(
                &router,
                "/status/engine?network=stagenet&round=7",
                Some(&cookie),
            )
            .await,
        )
        .await;
        assert!(pinned.contains("× Paused"), "{pinned}");
        assert!(pinned.contains("id=\"round-resume\" href=\"/status/engine?network=stagenet\""));
        let live = body_text(get(&router, "/status/engine", Some(&cookie)).await).await;
        assert!(!live.contains("id=\"round-resume\""), "live: no chip");
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
        assert_eq!(data["frame"]["view"]["summary"][0]["value"], "3,412,881");
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
        let page = EngineClient::embedded_for_tests(engine.router())
            .engine_activity("stagenet", None)
            .await
            .unwrap();
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
        assert_eq!(before["view"]["summary"][1]["value"], "3,412,880");
        let after = body_json(
            get(
                &router,
                &format!("/status/engine/at?network=stagenet&ms={last}"),
                Some(&cookie),
            )
            .await,
        )
        .await;
        assert_eq!(after["view"]["summary"][1]["value"], "3,412,881");
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
        let marks: Vec<&str> = frames
            .iter()
            .flat_map(|f| f["marks"].as_array().unwrap())
            .map(|m| m["text"].as_str().unwrap())
            .collect();
        assert!(
            marks.iter().any(|m| m.contains("1 payment found")),
            "{marks:?}"
        );
        let unknown = get(
            &router,
            "/status/engine/at?network=mainnet&ms=0",
            Some(&cookie),
        )
        .await;
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    }

    /// The page reads top down in one column: the back link above the
    /// title, whose row holds the help, the network picker and Reload;
    /// then the summary, the "Machine and links" strip, Chain, Round and
    /// the panels, Scanning first and open, no Webhooks panel. Without
    /// JavaScript the picker's Go submits it.
    #[tokio::test]
    async fn the_page_reads_top_down_in_one_column() {
        let (_engine, router, cookie) = site().await;
        let html = body_text(get(&router, "/status/engine", Some(&cookie)).await).await;
        let at = |needle: &str| {
            html.find(needle)
                .unwrap_or_else(|| panic!("no {needle:?} in the page"))
        };
        let order = [
            r#"<nav class="context-nav" aria-label="Breadcrumb"><a href="/status">Status</a></nav><div class="engine-titlebar"><h1>Engine</h1><details class="engine-help""#,
            r#"<span class="titlebar-spacer"></span><form class="engine-network" method="get" action="/status/engine">"#,
            r#"<button type="submit" class="network-go no-js-only">Go</button></form><a class="btn btn-secondary reload" href="/status/engine?network=stagenet""#,
            r#"id="engine-summary""#,
            r#"id="engine-machine""#,
            r#"id="engine-chain""#,
            r#"id="engine-round""#,
            r#"<section class="engine-panels" id="engine-panels""#,
            r#"<details class="mini t-blocks" id="d-scanning" open><summary><span class="tierchip t-blocks">Scanning</span>"#,
            r#"id="d-reorg""#,
            r#"id="d-restart""#,
            r#"id="h-events""#,
        ];
        let places: Vec<usize> = order.iter().map(|needle| at(needle)).collect();
        assert!(
            places.windows(2).all(|pair| pair[0] < pair[1]),
            "out of order: {places:?}"
        );
        assert!(html.contains(r#"<label for="engine-network" class="visually-hidden">Network</label><mk-select compact><select id="engine-network" name="network">"#));
        for gone in ["engine-side", "engine-main", "d-webhooks", ">Webhooks<"] {
            assert!(!html.contains(gone), "{gone} is gone");
        }
        // The strip's tiles, as loaded.
        for tile in [
            "cpu",
            "memory",
            "transfer",
            "round-trip",
            "first-byte",
            "block-size",
        ] {
            assert!(
                html.contains(&format!(r#"data-tile="{tile}""#)),
                "no {tile} tile"
            );
        }
        assert!(html.contains(r#"<span id="machine-age">as loaded</span>"#));
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

    /// Transfer, Round trip and First byte follow the node the engine is
    /// using now, named on the tile, a fallback said so; the standby node
    /// has no figures.
    #[test]
    fn the_link_tiles_follow_the_node_in_use_a_fallback_too() {
        let client = EngineClient::for_tests("http://engine.invalid");
        let tile = |view: &MachineView, key: &str| {
            view.tiles
                .iter()
                .find(|t| t.key == key)
                .cloned()
                .unwrap_or_else(|| panic!("no {key} tile"))
        };
        let primary = machine_view(&client, &status_with_nodes(0), "stagenet");
        let transfer = tile(&primary, "transfer");
        assert_eq!(
            (transfer.value.as_str(), transfer.note.as_str()),
            ("3.1 Mbit/s", "node.example.com:38081")
        );
        assert_eq!(tile(&primary, "round-trip").value, "120 ms");
        assert_eq!(primary.at_unix, 1_800_000_000);

        let fallback = machine_view(&client, &status_with_nodes(1), "stagenet");
        assert_eq!(
            tile(&fallback, "first-byte").note,
            "backup.example.org:38089 · fallback"
        );
        assert_eq!(tile(&fallback, "first-byte").value, "45 ms");
        let html: String = fallback
            .tiles
            .iter()
            .map(|t| views::engine::tile_markup(t).into_string())
            .collect();
        assert!(
            !html.contains("node.example.com"),
            "the standby node has no figures: {html}"
        );
        assert!(!html.contains("999 ms"), "{html}");

        let elsewhere = machine_view(&client, &status_with_nodes(0), "mainnet");
        assert_eq!(tile(&elsewhere, "transfer").note, "no node in use");
        assert_eq!(elsewhere.scanning, None);
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
    /// history, and then at most every MACHINE_EVERY however many frames
    /// come between: it is built from the cached `/status`, not per frame.
    #[tokio::test]
    async fn the_machine_event_comes_at_once_then_at_most_every_few_seconds() {
        let (engine, router, cookie) = site().await;
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
        let keys: Vec<&str> = machine["tiles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["key"].as_str().unwrap())
            .collect();
        assert_eq!(
            keys,
            [
                "cpu",
                "memory",
                "transfer",
                "round-trip",
                "first-byte",
                "block-size"
            ]
        );
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
            second >= MACHINE_EVERY - Duration::from_millis(50),
            "the next machine event waited: {second:?}"
        );
    }
}
