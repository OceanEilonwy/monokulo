use crate::api::{self, prelude::*};

api::resource!(
    "/api/v1/logs",
    rel = "logs",
    doc = "Where a shop plugin sends its warnings and errors, when its \"Send errors to Monokulo\" option is on.\n\nThey are kept, on the instance admin's Logs page, only when the store turned client logs on (structured_logging.md 2.4)."
);

/// Most entries one request may carry.
pub const MAX_ENTRIES: usize = 20;

/// A plugin's warnings and errors, at most 20 at a time.
#[api::input]
struct Input {
    /// The entries, oldest first.
    entries: Vec<Entry>,
}

/// One warning or error.
#[api::input]
struct Entry {
    /// A PSR-3 level name (`error`, `warning`, ...), as WordPress's
    /// `WC_Log_Levels` and most PHP shops use. Only `warning` and louder
    /// are kept.
    level: String,
    /// What happened; cut to 1000 characters.
    message: String,
    /// The trace of the plugin's request to Monokulo this happened in (32
    /// hex digits), to show the entry beside it.
    trace_id: Option<String>,
}

/// Send the plugin's warnings and errors.
#[post(sk, form = "send-logs")]
async fn send(
    store: Store,
    instance: Instance,
    client: Client,
    input: Input,
) -> api::Result<NoContent> {
    let state = instance.0;
    if input.entries.len() > MAX_ENTRIES {
        return Err(
            Problem::TooLarge.detail(format!("Send at most {MAX_ENTRIES} entries at a time."))
        );
    }
    // The store's own setting wins over the plugin's option: refused unless
    // the store opted in to client logs. (Not a silent success: the shop's
    // server is authenticated, so it can be told why.)
    let id = store.id.clone();
    if !state
        .db
        .read(move |db| db.client_logging(&id))
        .await
        .unwrap_or(false)
    {
        return Err(Problem::LogsOff.detail(
            "This store hasn't turned on client logs. Its owner can turn them on in the store's settings.",
        ));
    }
    // The plugin, by the slug it names itself with; any other client is
    // just a plugin.
    let source = client
        .0
        .and_then(|(name, _)| crate::plugins::PluginSlug::parse(&name.to_ascii_lowercase()))
        .map_or_else(|| "plugin".to_owned(), |slug| slug.to_string());
    for entry in input.entries {
        let span = tracing::info_span!(parent: None, "plugin report", source = %source, store.id = %store.id);
        if let Some(trace_id) = entry
            .trace_id
            .as_deref()
            .filter(|t| telemetry::store::api::is_trace_id(t))
        {
            // The plugin names only its trace, not a span in it.
            telemetry::trace::set_remote_parent(
                &span,
                &format!("00-{trace_id}-0000000000000001-01"),
            );
        }
        let _entered = span.enter();
        let message = crate::http::clip_report(&entry.message);
        match entry.level.as_str() {
            "emergency" | "alert" | "critical" | "error" => tracing::error!("{message}"),
            "warning" => tracing::warn!("{message}"),
            // Anything quieter isn't forwarded by the plugin; ignore it.
            _ => {}
        }
    }
    Ok(NoContent)
}
