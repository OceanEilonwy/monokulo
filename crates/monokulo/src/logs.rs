//! Reading logs from both stores (structured_logging.md 3.4): monokulo's
//! own `logs.db`, read directly, and the engine's, read through its admin
//! API. The Logs page shows them as one list.
//!
//! Both are read with the same query and page size, then merged by
//! `(ts, service, id)`, the order every store pages in, so a cursor from
//! the merged list pages both stores correctly. When the engine can't be
//! reached, its lines are missing and [`Page::engine_problem`] says why;
//! monokulo's own still show.

use telemetry::query::ParseError;
use telemetry::store::api::{HistogramRequest, LogsRequest};
use telemetry::store::{LogRow, LogStore, Trace};

use crate::engine_client::EngineClient;

/// Where lines come from.
#[derive(Clone)]
pub struct Sources {
    pub local: Option<LogStore>,
    pub engine: EngineSource,
}

/// How to reach the engine's logs, or why they can't be.
#[derive(Clone)]
pub enum EngineSource {
    Api {
        client: EngineClient,
    },
    /// Not read, for the reason given (a page of monokulo's own lines only).
    Unavailable(String),
}

impl Sources {
    pub async fn from_state(state: &crate::http::AppState) -> Sources {
        Sources {
            local: state.log_store.clone(),
            engine: EngineSource::Api {
                client: state.engine.client.clone(),
            },
        }
    }
}

/// One page of the merged list, newest first.
#[derive(Debug, Default)]
pub struct Page {
    pub rows: Vec<LogRow>,
    /// Why the engine's lines are missing, when they are.
    pub engine_problem: Option<String>,
    /// Why monokulo's own lines are missing, when they are.
    pub local_problem: Option<String>,
}

fn engine_problem(e: impl std::fmt::Display) -> String {
    format!("The engine's lines aren't shown: {e}.")
}

async fn local<T: Send + 'static>(
    store: &Option<LogStore>,
    read: impl FnOnce(&LogStore) -> Result<T, telemetry::store::StoreError> + Send + 'static,
) -> Result<T, String> {
    let Some(store) = store.clone() else {
        return Err("This server's log store isn't open, so its own lines aren't shown.".into());
    };
    match tokio::task::spawn_blocking(move || read(&store)).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(format!("This server's lines aren't shown: {e}.")),
        Err(e) => Err(format!("This server's lines aren't shown: {e}.")),
    }
}

/// Merges two newest-first lists into one, keeping `limit` rows: the
/// newest ones, or for a page of newer lines (`newer`), the ones closest
/// to the cursor.
pub fn merge(mut a: Vec<LogRow>, b: Vec<LogRow>, limit: usize, newer: bool) -> Vec<LogRow> {
    a.extend(b);
    a.sort_by_key(|row| std::cmp::Reverse(row.cursor()));
    if newer {
        let skip = a.len().saturating_sub(limit);
        a.drain(..skip);
    } else {
        a.truncate(limit);
    }
    a
}

/// One page of lines matching `request`. A query that doesn't parse is an
/// error before anything is read.
pub async fn read(sources: &Sources, request: &LogsRequest) -> Result<Page, ParseError> {
    let query = request.to_query()?;
    let limit = query.limit as usize;
    let newer = query.after.is_some() && query.before.is_none();
    let local_query = query.clone();
    let (local_rows, engine_rows) = tokio::join!(
        local(&sources.local, move |s| s.query(&local_query)),
        async {
            match &sources.engine {
                EngineSource::Api { client } => client.logs(request).await.map_err(engine_problem),
                EngineSource::Unavailable(why) => Err(why.clone()),
            }
        }
    );
    let mut page = Page::default();
    let local_rows = local_rows.unwrap_or_else(|e| {
        page.local_problem = Some(e);
        Vec::new()
    });
    let engine_rows = engine_rows.unwrap_or_else(|e| {
        page.engine_problem = Some(e);
        Vec::new()
    });
    page.rows = merge(local_rows, engine_rows, limit, newer);
    Ok(page)
}

/// Everything both stores hold for one trace, spans by start time and
/// lines by time, and why the engine's part is missing if it is.
pub async fn trace(sources: &Sources, trace_id: &str) -> (Trace, Option<String>) {
    let id = trace_id.to_string();
    let (local_trace, engine_trace) =
        tokio::join!(local(&sources.local, move |s| s.trace(&id)), async {
            match &sources.engine {
                EngineSource::Api { client } => {
                    client.log_trace(trace_id).await.map_err(engine_problem)
                }
                EngineSource::Unavailable(why) => Err(why.clone()),
            }
        });
    let mut trace = local_trace.unwrap_or_default();
    let problem = match engine_trace {
        Ok(engine) => {
            trace.spans.extend(engine.spans);
            trace.logs.extend(engine.logs);
            None
        }
        Err(e) => Some(e),
    };
    trace.spans.sort_by_key(|s| (s.start, s.end));
    trace.logs.sort_by_key(LogRow::cursor);
    (trace, problem)
}

/// Line counts per slice of `[from, to)` across both stores.
pub async fn histogram(sources: &Sources, request: &HistogramRequest) -> Vec<u64> {
    // A query that doesn't parse matches nothing: an empty histogram, not
    // one of every line (the page reports the query's problem).
    let filter = match telemetry::query::parse(request.q.as_deref().unwrap_or("")) {
        Ok(filter) => filter,
        Err(_) => return vec![0; request.buckets.clamp(1, 500) as usize],
    };
    let (from, to, buckets) = (request.from, request.to, request.buckets);
    let (local_counts, engine_counts) = tokio::join!(
        local(&sources.local, move |s| s.histogram(
            filter.as_ref(),
            from,
            to,
            buckets
        )),
        async {
            match &sources.engine {
                EngineSource::Api { client } => client.log_histogram(request).await.ok(),
                EngineSource::Unavailable(_) => None,
            }
        }
    );
    let mut counts = local_counts.unwrap_or_default();
    for (i, n) in engine_counts.unwrap_or_default().into_iter().enumerate() {
        match counts.get_mut(i) {
            Some(slot) => *slot += n,
            None => counts.push(n),
        }
    }
    counts
}

/// Attribute names seen recently in either store.
pub async fn attribute_names(sources: &Sources) -> Vec<String> {
    let (local_names, engine_names) =
        tokio::join!(local(&sources.local, |s| s.attribute_names()), async {
            match &sources.engine {
                EngineSource::Api { client } => client.log_attributes().await.ok(),
                EngineSource::Unavailable(_) => None,
            }
        });
    let mut names = local_names.unwrap_or_default();
    names.extend(engine_names.unwrap_or_default());
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(ts: i64, service: &str, id: i64) -> LogRow {
        LogRow {
            id,
            ts,
            level: 9,
            service: service.into(),
            target: "t".into(),
            message: format!("{service} {ts}"),
            trace_id: None,
            span_id: None,
            attributes: Default::default(),
            spans: vec![],
        }
    }

    fn messages(rows: &[LogRow]) -> Vec<String> {
        rows.iter().map(|r| r.message.clone()).collect()
    }

    #[test]
    fn two_stores_merge_newest_first_and_page_consistently() {
        let monokulo = vec![
            row(50, "monokulo", 5),
            row(30, "monokulo", 3),
            row(10, "monokulo", 1),
        ];
        let engine = vec![
            row(40, "scanner", 4),
            row(30, "scanner", 3),
            row(20, "scanner", 2),
        ];
        let first = merge(monokulo.clone(), engine.clone(), 3, false);
        assert_eq!(
            messages(&first),
            ["monokulo 50", "scanner 40", "scanner 30"]
        );
        // The next page: each store asked for rows before the last one shown.
        let cursor = first.last().unwrap().cursor();
        let older = |rows: &[LogRow]| {
            rows.iter()
                .filter(|r| r.cursor() < cursor)
                .cloned()
                .collect::<Vec<_>>()
        };
        let second = merge(older(&monokulo), older(&engine), 3, false);
        assert_eq!(
            messages(&second),
            ["monokulo 30", "scanner 20", "monokulo 10"]
        );
    }

    #[test]
    fn a_page_of_newer_lines_keeps_those_closest_to_the_cursor() {
        let monokulo = vec![row(50, "monokulo", 5), row(30, "monokulo", 3)];
        let engine = vec![row(40, "scanner", 4), row(35, "scanner", 3)];
        assert_eq!(
            messages(&merge(monokulo, engine, 2, true)),
            ["scanner 35", "monokulo 30"]
        );
    }

    #[tokio::test]
    async fn without_the_engine_or_a_store_the_page_says_why() {
        let sources = Sources {
            local: None,
            engine: EngineSource::Unavailable("not read".into()),
        };
        let page = read(&sources, &LogsRequest::default()).await.unwrap();
        assert!(page.rows.is_empty());
        assert_eq!(page.engine_problem.as_deref(), Some("not read"));
        assert!(page.local_problem.is_some());
        let error = read(
            &sources,
            &LogsRequest {
                q: Some("level = loud".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(error.message.contains("a level is one of"));
    }

    /// A store in a temporary directory with `lines` logged into it as
    /// `service`, waited for.
    fn store_with(service: &'static str, dir: &std::path::Path, lines: &[&str]) -> LogStore {
        let (telemetry, subscriber) = telemetry::build(
            service,
            telemetry::Format::Json,
            false,
            "info",
            std::io::sink,
        );
        let store = telemetry
            .open_store(&dir.join(format!("{service}.logs.db")))
            .unwrap();
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("reader test request");
            span.in_scope(|| {
                for line in lines {
                    tracing::info!(order.id = "o_reader", "{line}");
                }
            });
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60); // bounds only a hung run
        while store
            .query(&telemetry::store::LogQuery {
                limit: 100,
                ..Default::default()
            })
            .unwrap()
            .len()
            < lines.len()
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the lines were never stored"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        store
    }

    #[tokio::test]
    async fn lines_from_monokulo_and_a_real_engine_come_back_as_one_list() {
        let dir = std::env::temp_dir().join(format!(
            "monokulo-log-reader-{}-{}",
            std::process::id(),
            crate::now_unix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let local_store = store_with("monokulo", &dir, &["monokulo one", "monokulo two"]);
        let engine_store = store_with("scanner", &dir, &["engine one"]);
        let engine = scanner_test_support::TestEngineConfig::new()
            .with_log_store(engine_store)
            .spawn()
            .await;
        let sources = Sources {
            local: Some(local_store),
            engine: EngineSource::Api {
                client: EngineClient::for_tests(format!("http://{}", engine.addr)),
            },
        };

        let page = read(
            &sources,
            &LogsRequest {
                q: Some("order.id = 'o_reader'".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(page.engine_problem, None);
        let mut messages = messages(&page.rows);
        messages.sort();
        assert_eq!(messages, ["engine one", "monokulo one", "monokulo two"]);
        assert!(
            page.rows.windows(2).all(|w| w[0].cursor() > w[1].cursor()),
            "newest first"
        );

        let trace_id = page
            .rows
            .iter()
            .find(|r| r.service == "scanner")
            .unwrap()
            .trace_id
            .clone()
            .unwrap();
        let (trace, problem) = trace(&sources, &trace_id).await;
        assert_eq!(problem, None);
        assert_eq!(messages_of(&trace.logs), ["engine one"]);
        assert_eq!(trace.spans.len(), 1, "the engine's span came back too");

        assert!(attribute_names(&sources)
            .await
            .contains(&"order.id".to_string()));

        let wrong = Sources {
            local: sources.local.clone(),
            engine: EngineSource::Api {
                client: EngineClient::new(
                    format!("http://{}", engine.addr),
                    shared::auth::RawToken::presented("wrong_engine_token_0123456789abcdef"),
                ),
            },
        };
        let page = read(&wrong, &LogsRequest::default()).await.unwrap();
        assert!(page.engine_problem.is_some());
        assert_eq!(page.rows.len(), 2, "monokulo's own lines still show");
        let _ = std::fs::remove_dir_all(dir);
    }

    fn messages_of(rows: &[LogRow]) -> Vec<String> {
        messages(rows)
    }
}
