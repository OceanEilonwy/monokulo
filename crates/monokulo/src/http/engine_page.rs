//! The engine page's handlers (`docs/engine_visualizer.md`), admins only:
//! it shows node addresses and every store's progress.
//!
//! - `GET /status/engine?network=`: the page, whole, as of now.
//! - `GET /status/engine/events?network=`: what the page's script follows:
//!   a `history` event (the network's live frame and every mark kept), then
//!   a `frame` event each time the engine records something, `restarted`
//!   when the history started over (the script reconnects) and
//!   `unreachable` while the engine doesn't answer.
//! - `GET /status/engine/at?network=&ms=`: what the page drew at a moment
//!   (the timeline's scrubbing).
//! - `GET /status/engine/replay?network=&from=&to=`: the frames between two
//!   moments (replay), at most a minute of them.
//! - `GET /status/engine/round?network=&number=`: a past round as the round
//!   card draws it (a click on the recent rounds). Without JavaScript the
//!   recent rounds link to the page with `&round=`.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::Event;
use axum::response::{Html, IntoResponse, Json, Response};
use futures_util::stream::{self, StreamExt as _};
use serde::{Deserialize, Serialize};

use super::{AppState, AuthedAdmin};
use crate::engine_view::history::Frame;
use crate::engine_view::present::{mark_view, MarkView};
use crate::engine_view::relay::Message;
use crate::views;

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
        .map(|status| status.networks.into_iter().map(|n| n.network).collect())
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
    let networks = match networks(&state).await {
        Ok(networks) => networks,
        Err(error) => {
            return Html(
                views::engine::page(
                    &chrome,
                    &views::engine::EnginePage {
                        networks: Vec::new(),
                        network: String::new(),
                        view: None,
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
    Html(
        views::engine::page(
            &chrome,
            &views::engine::EnginePage {
                networks,
                network,
                view,
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
    let follow = stream::unfold((watch, permit), |(mut watch, permit)| async move {
        let event = match watch.messages.recv().await {
            Ok(Message::Frame(_, json)) => Event::default().event("frame").data(&*json),
            // Fell behind (a slow reader): start over from the history.
            Ok(Message::Restarted) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                Event::default().event("restarted").data("")
            }
            Ok(Message::Unreachable(error)) => Event::default().event("unreachable").data(error),
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
        };
        Some((Ok::<_, Infallible>(event), (watch, permit)))
    });
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
            engine: crate::http::Engine::new(EngineClient::for_tests(format!(
                "http://{}",
                engine.addr
            ))),
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
            "/static/engine-view.js",
            "id=\"tl\"",
            "Reload",
            "data-network=\"stagenet\"",
            // The switcher: the network scanned is a link, the others say why not.
            "href=\"/status/engine?network=stagenet\" aria-current=\"page\"",
            "this engine has no Monero node for mainnet",
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
    async fn an_unreachable_engine_is_said_so() {
        let state = AppState {
            engine: crate::http::Engine::new(EngineClient::for_tests("http://127.0.0.1:9")),
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
        let page = EngineClient::for_tests(format!("http://{}", engine.addr))
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
}
