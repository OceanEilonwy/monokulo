//! `GET /status` - an operator-facing HTML page reporting every configured
//! Monero node's live reachability/height and the chain-scanner loop's own
//! recent tick history, across every configured network at once. Built for
//! this session's own user-directed request: "a status page which shows
//! all the nodes in use, their block heights, networks and statuses [and]
//! the background scan process operation... informative and stylish
//! matching the style of the site."
//!
//! "The style of the site" here means the control-plane's own visual
//! identity (`control-plane/templates/_styles.html.hbs`) - the engine and
//! control-plane are separate crates with no shared template
//! infrastructure to pull from, so the CSS below is a deliberate,
//! byte-for-byte copy of that file, not a coincidence or a fork that will
//! quietly drift: if that file's look changes, this one should be updated
//! to match by hand.
//!
//! Every node's height is queried live, on every request, the same
//! "the next real call is the health check" philosophy
//! `daemon_fallback`'s own module doc comment states explicitly - this
//! page reports what's true right now, not a cached belief about what was
//! true at some earlier point. A per-node timeout keeps one unreachable
//! node from making the whole page hang.

use std::time::Duration;

use axum::extract::State;
use axum::response::{Html, IntoResponse, Response};
use handlebars::Handlebars;
use monero::Network;
use serde::Serialize;

use crate::network::network_str;
use crate::now_unix;

use super::AppState;

const NODE_HEIGHT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Serialize)]
struct NodeStatusView {
    label: String,
    is_active: bool,
    /// Pre-formatted, not `Option<u64>` rendered via `{{#if}}` - handlebars
    /// treats `0` as falsy exactly like JS's `if(0)` does, which would
    /// render a real, live height of *exactly* zero identically to "unknown"
    /// (caught by this module's own test,
    /// `status_page_shows_the_real_configured_network_and_node_with_its_live_height`,
    /// against `FakeDaemonClient`'s real starting height of 0 - not a
    /// theoretical concern).
    height_display: String,
    error: Option<String>,
}

#[derive(Serialize)]
struct NetworkStatusView {
    network: String,
    nodes: Vec<NodeStatusView>,
    scanner_ever_ticked: bool,
    scanner_last_tick_relative: String,
    scanner_tick_count: u64,
    scanner_tenants_scanned: usize,
    scanner_last_error: Option<String>,
    scanner_health_label: String,
    scanner_health_class: String,
}

#[derive(Serialize)]
struct StatusPageView {
    networks: Vec<NetworkStatusView>,
    poll_interval_secs: u64,
    generated_at: String,
}

/// Renders "3s ago" / "2m ago" / "1h ago" from a unix-seconds timestamp -
/// deliberately coarse (this page auto-refreshes, see the template's own
/// `<meta http-equiv="refresh">`, so nothing here needs sub-second
/// precision) and never negative even if `now` and `then` are milliseconds
/// apart in the wrong direction (a real, reachable case: the tick that
/// just finished may be timestamped a moment after this handler's own
/// `now_unix()` call if the clock is read between them - `saturating_sub`
/// avoids a nonsensical "-1s ago").
fn relative_time(now: i64, then: i64) -> String {
    let delta = now.saturating_sub(then).max(0);
    match delta {
        0..=59 => format!("{delta}s ago"),
        60..=3599 => format!("{}m ago", delta / 60),
        3600..=86399 => format!("{}h ago", delta / 3600),
        _ => format!("{}d ago", delta / 86400),
    }
}

/// A network's scan loop is considered stale (not just "last tick failed" -
/// genuinely not ticking at all) once it's gone more than 3x its own
/// configured poll interval without a tick - generous enough that normal
/// jitter (a slow node, GC pause, whatever) never falsely reads as stuck,
/// while still catching a genuinely wedged loop well before an operator
/// would otherwise notice only via a merchant complaint (the exact failure
/// mode `main.rs::supervise`'s own doc comment names as the reason that
/// function exists at all).
fn is_stale(now: i64, last_tick_finished_at: i64, poll_interval_secs: u64) -> bool {
    let staleness_threshold = (poll_interval_secs as i64).saturating_mul(3).max(1);
    now.saturating_sub(last_tick_finished_at) > staleness_threshold
}

pub async fn status_page(State(state): State<AppState>) -> Response {
    let now = now_unix();

    let mut networks: Vec<(Network, _)> = state.daemons.iter().map(|(n, d)| (*n, d.clone())).collect();
    networks.sort_by_key(|(network, _)| network_str(*network));

    let mut network_views = Vec::with_capacity(networks.len());
    for (network, daemon) in networks {
        let current_index = daemon.current_index();
        let mut nodes = Vec::with_capacity(daemon.nodes().len());
        for (i, node) in daemon.nodes().iter().enumerate() {
            let (height, error) = match tokio::time::timeout(NODE_HEIGHT_TIMEOUT, node.client.get_height()).await {
                Ok(Ok(h)) => (Some(h), None),
                Ok(Err(e)) => (None, Some(e.to_string())),
                Err(_) => (None, Some(format!("timed out after {}s", NODE_HEIGHT_TIMEOUT.as_secs()))),
            };
            let height_display = height.map(|h| h.to_string()).unwrap_or_else(|| "-".to_string());
            nodes.push(NodeStatusView { label: node.label.clone(), is_active: i == current_index, height_display, error });
        }

        let scan_status = state.scanner_status.read().unwrap().get(&network).cloned();
        let (scanner_ever_ticked, scanner_last_tick_relative, scanner_tick_count, scanner_tenants_scanned, scanner_last_error, scanner_health_label, scanner_health_class) =
            match scan_status {
                None => (false, "never ticked yet".to_string(), 0, 0, None, "never ticked".to_string(), "tag-unknown".to_string()),
                Some(s) => {
                    let finished_at = s.last_tick_finished_at.unwrap_or(now);
                    let relative = relative_time(now, finished_at);
                    let (label, class) = if is_stale(now, finished_at, state.scan_poll_interval_secs) {
                        ("stale".to_string(), "tag-error".to_string())
                    } else if s.last_tick_ok {
                        ("healthy".to_string(), "tag-ok".to_string())
                    } else {
                        ("tick failing".to_string(), "tag-error".to_string())
                    };
                    (true, relative, s.tick_count, s.tenants_scanned, s.last_error, label, class)
                }
            };

        network_views.push(NetworkStatusView {
            network: network_str(network).to_string(),
            nodes,
            scanner_ever_ticked,
            scanner_last_tick_relative,
            scanner_tick_count,
            scanner_tenants_scanned,
            scanner_last_error,
            scanner_health_label,
            scanner_health_class,
        });
    }

    let view = StatusPageView {
        networks: network_views,
        poll_interval_secs: state.scan_poll_interval_secs,
        generated_at: chrono_like_utc_string(now),
    };

    let mut handlebars = Handlebars::new();
    handlebars.set_strict_mode(true);
    handlebars
        .register_template_string("status", STATUS_TEMPLATE)
        .expect("the built-in status template must always register");
    let html = handlebars.render("status", &view).expect("the built-in status template must always render");
    Html(html).into_response()
}

/// A plain `YYYY-MM-DD HH:MM:SS UTC` timestamp with no chrono/time crate
/// dependency - this page's only use for one is a human-readable "as of"
/// line, not worth a new dependency for. Correct for any unix-seconds
/// input (proper leap-year handling via the standard 400/100/4-year rule),
/// not just "close enough."
fn chrono_like_utc_string(unix_seconds: i64) -> String {
    let days_since_epoch = unix_seconds.div_euclid(86_400);
    let seconds_of_day = unix_seconds.rem_euclid(86_400);
    let (hour, minute, second) = (seconds_of_day / 3600, (seconds_of_day % 3600) / 60, seconds_of_day % 60);

    // Civil-from-days algorithm (Howard Hinnant's well-known constant-time
    // Gregorian conversion) - avoids pulling in a date/time crate for one
    // display line.
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { y + 1 } else { y };

    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

const STATUS_TEMPLATE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="refresh" content="15">
<title>Status - MoneroPay</title>
<style>
/* Byte-for-byte copy of control-plane/templates/_styles.html.hbs - see this
   file's own module doc comment for why. */
:root {
  --ink: #111111;
  --paper: #f4f1ea;
  --paper-raised: #ffffff;
  --line: #111111;
  --accent: #ff6600;
  --accent-ink: #111111;
  --error: #b00020;
  --muted: #5a564c;
}
* { box-sizing: border-box; }
html, body {
  margin: 0;
  padding: 0;
  background: var(--paper);
  color: var(--ink);
  font-family: "SFMono-Regular", Consolas, "Liberation Mono", Menlo, monospace;
  font-size: 15px;
  line-height: 1.5;
}
body { padding-bottom: 4rem; }
a { color: var(--ink); text-decoration: underline; text-decoration-thickness: 1px; }
a:hover { color: var(--accent); }
h1, h2, h3 { font-weight: 700; letter-spacing: -0.01em; margin: 1.4em 0 0.6em; }
h1 { font-size: 1.6rem; border-bottom: 3px solid var(--line); padding-bottom: 0.3em; }
h2 { font-size: 1.15rem; border-bottom: 1px solid var(--line); padding-bottom: 0.2em; }
p { margin: 0.6em 0; }
code, pre {
  font-family: inherit;
  background: var(--paper-raised);
  border: 1px solid var(--line);
  padding: 0.1em 0.35em;
}
pre { padding: 0.8em; overflow-x: auto; white-space: pre-wrap; word-break: break-all; }
.wrap { max-width: 880px; margin: 0 auto; padding: 0 1.2rem; }
.error {
  border: 2px solid var(--error);
  color: var(--error);
  padding: 0.6em 0.8em;
  font-weight: 700;
  margin: 1em 0;
}
.hint { color: var(--muted); font-size: 0.9em; }
.box {
  border: 2px solid var(--line);
  background: var(--paper-raised);
  padding: 1.1rem;
  margin: 1.2rem 0;
}
.box + .box { margin-top: 1.2rem; }
table { border-collapse: collapse; width: 100%; margin: 0.8em 0; }
th, td { border: 1px solid var(--line); padding: 0.4em 0.6em; text-align: left; vertical-align: top; }
th { background: var(--ink); color: var(--paper); font-weight: 700; }
tr:nth-child(even) td { background: rgba(0,0,0,0.03); }
.tag {
  display: inline-block;
  border: 1px solid var(--line);
  padding: 0.05em 0.5em;
  font-size: 0.8em;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.03em;
}
.tag-ok { background: #dff5d8; }
.tag-error { background: #f8d7da; }
.tag-unknown { background: #eee; }
.muted { color: var(--muted); }
.rule { border: none; border-top: 2px dashed var(--line); margin: 1.6em 0; }
</style>
</head>
<body>
<div style="background:var(--ink);color:var(--paper);border-bottom:3px solid var(--accent);">
<div class="wrap" style="padding-top:0.7rem;padding-bottom:0.7rem;">
<span style="font-weight:700;letter-spacing:-0.02em;">[ MoneroPay ] status</span>
</div>
</div>
<div class="wrap">

<h1>Engine status</h1>
<p class="hint">As of {{generated_at}} - refreshes automatically every 15s. Scan loop polls every {{poll_interval_secs}}s.</p>

{{#each networks}}
<div class="box">
<h2>{{this.network}} <span class="tag {{this.scanner_health_class}}">{{this.scanner_health_label}}</span></h2>

<h3>Nodes</h3>
<table>
<thead>
<tr><th>Node</th><th>Active</th><th>Height</th><th>Status</th></tr>
</thead>
<tbody>
{{#each this.nodes}}
<tr>
<td><code>{{this.label}}</code></td>
<td>{{#if this.is_active}}<span class="tag tag-ok">active</span>{{else}}<span class="muted">fallback</span>{{/if}}</td>
<td>{{this.height_display}}</td>
<td>
{{#if this.error}}
<span class="tag tag-error">error</span> <span class="muted">{{this.error}}</span>
{{else}}
<span class="tag tag-ok">reachable</span>
{{/if}}
</td>
</tr>
{{/each}}
</tbody>
</table>

<h3>Chain scanner</h3>
{{#if this.scanner_ever_ticked}}
<table>
<tr><th>Last tick</th><td>{{this.scanner_last_tick_relative}}</td></tr>
<tr><th>Ticks so far</th><td>{{this.scanner_tick_count}}</td></tr>
<tr><th>Tenants scanned (last tick)</th><td>{{this.scanner_tenants_scanned}}</td></tr>
{{#if this.scanner_last_error}}
<tr><th>Last error</th><td class="error" style="margin:0;">{{this.scanner_last_error}}</td></tr>
{{/if}}
</table>
{{else}}
<p class="muted">This network has not been scanned yet.</p>
{{/if}}
</div>
{{/each}}

{{#unless networks}}
<p class="muted">No Monero nodes are configured on this instance.</p>
{{/unless}}

<hr class="rule">
<p class="hint">This page makes a real, live call to every listed node on every load - the numbers above are current as of the timestamp shown, not cached.</p>

</div>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_time_formats_each_real_bucket_correctly() {
        assert_eq!(relative_time(1000, 1000), "0s ago");
        assert_eq!(relative_time(1000, 970), "30s ago");
        assert_eq!(relative_time(1000, 940), "1m ago");
        assert_eq!(relative_time(1000, 1005), "0s ago", "a `then` slightly after `now` must not go negative");
        assert_eq!(relative_time(20_000, 0), "5h ago", "20_000s is 5.56h - within this function's own 24h hour-bucket range");
        assert_eq!(relative_time(100_000, 0), "1d ago", "100_000s is 27.8h - past the 24h cutoff into the day bucket");
        assert_eq!(relative_time(1_000_000, 0), "11d ago");
    }

    #[test]
    fn is_stale_uses_three_times_the_configured_poll_interval_as_its_threshold() {
        assert!(!is_stale(1000, 995, 2), "5s since the last tick, 2s interval x3 = 6s threshold - not stale yet");
        assert!(is_stale(1000, 990, 2), "10s since the last tick, 2s interval x3 = 6s threshold - genuinely stale");
        assert!(!is_stale(1000, 1000, 0), "a poll_interval of 0 must not make every tick instantly stale");
    }

    /// Real, known reference points - not just "internally consistent with
    /// itself" - each cross-checked against `date -u -d @<seconds>`
    /// independently before being written down here.
    #[test]
    fn chrono_like_utc_string_matches_known_real_timestamps() {
        assert_eq!(chrono_like_utc_string(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(chrono_like_utc_string(1_000_000_000), "2001-09-09 01:46:40 UTC");
        assert_eq!(chrono_like_utc_string(1_700_000_000), "2023-11-14 22:13:20 UTC");
        // A leap-year boundary (2024-02-29 exists; 2023-02-29 doesn't) - the
        // real reason a hand-rolled date function needs this class of
        // dedicated test rather than trusting it because one live page load
        // looked right.
        assert_eq!(chrono_like_utc_string(1_709_251_199), "2024-02-29 23:59:59 UTC");
        assert_eq!(chrono_like_utc_string(1_709_251_200), "2024-03-01 00:00:00 UTC");
    }
}
