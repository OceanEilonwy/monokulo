//! Plain display/formatting helpers shared by several `views`/`http`
//! modules: durations, scan ranges and empty values.

/// `value`, escaped, or a muted placeholder for a field with nothing to
/// show - same `<span class="muted">-</span>` convention the status page
/// already uses for "no value", applied here to every optional
/// order/payment field so a merchant never sees a bare, unexplained empty
/// table cell.
pub fn display_or_dash(value: Option<&str>) -> maud::Markup {
    match value {
        Some(v) if !v.is_empty() => maud::html! { (v) },
        _ => no_value(),
    }
}

/// The muted dash shown for a value there isn't.
fn no_value() -> maud::Markup {
    maud::html! { span class="muted" { "-" } }
}

/// The order-detail page's "Scan range"
/// row, computed once here rather than branched on in the template.
/// `first_scanned_height` gates everything: `None` means nothing has ever
/// examined this order (predates the feature, or hasn't had its first tick yet),
/// which the other two fields can't meaningfully qualify.
pub fn display_scan_range(
    first_scanned_height: Option<i64>,
    last_scanned_height: Option<i64>,
    currently_scanning: bool,
) -> maud::Markup {
    let Some(first) = first_scanned_height else {
        return no_value();
    };
    let last = last_scanned_height.unwrap_or(first);
    if currently_scanning {
        maud::html! { (first) "+" }
    } else {
        maud::html! { (first) " - " (last) }
    }
}

/// A moment.js-style relative duration until `target_unix` ("12h", "4h
/// 15m", "2d 4h") - at most the two largest non-zero units (days, hours,
/// minutes), a zero unit skipped rather than shown ("1d 30m", never "1d 0h
/// 30m" or "1d 0h"). `"any moment"` once `target_unix` has passed.
///
/// Computed here, server-side, rather than by client JavaScript from a raw
/// timestamp: the checkout page, which must stay fully meaningful with
/// JavaScript disabled, shows it as the time left to pay.
/// How long ago `then` was, in the same units as [`format_duration_until`]:
/// `"<1m"` under a minute, never `"any moment"`.
pub fn format_duration_since(then_unix: i64, now_unix: i64) -> String {
    if now_unix - then_unix < 60 {
        "<1m".to_string()
    } else {
        format_duration_until(now_unix, then_unix)
    }
}

pub fn format_duration_until(target_unix: i64, now_unix: i64) -> String {
    let seconds_left = target_unix - now_unix;
    if seconds_left <= 0 {
        return "any moment".to_string();
    }
    let seconds_left = seconds_left as u64;
    let days = seconds_left / 86400;
    let hours = (seconds_left % 86400) / 3600;
    let minutes = (seconds_left % 3600) / 60;

    let mut parts = Vec::with_capacity(2);
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 && parts.len() < 2 {
        parts.push(format!("{minutes}m"));
    }
    if parts.is_empty() {
        parts.push("<1m".to_string());
    }
    parts.truncate(2);
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// At most two parts, the largest first, skipping a zero unit; under a
    /// minute is `<1m`; a time already here or passed is "any moment",
    /// never negative.
    #[test]
    fn format_duration_until_matches_the_moment_js_style_examples() {
        let now = 1_700_000_000;
        for (from_now, shown) in [
            (12 * 3600, "12h"),
            (4 * 3600 + 15 * 60, "4h 15m"),
            (2 * 86400 + 4 * 3600, "2d 4h"),
            (2 * 86400 + 4 * 3600 + 30 * 60, "2d 4h"),
            (86400 + 30 * 60, "1d 30m"),
            (30, "<1m"),
            (0, "any moment"),
            (-100, "any moment"),
        ] {
            assert_eq!(
                format_duration_until(now + from_now, now),
                shown,
                "{from_now}"
            );
        }
    }

    // Signup/login page tests moved to `views::auth`'s own test module -
    // those pages no longer go through this engine at all.

    // Connect / connect-platform / new-store-picker page tests moved to
    // `views::connect`'s own test module - those pages no longer go through
    // this engine at all.

    // Dashboard home page tests moved to `views::home`'s own test
    // module - that page no longer goes through this engine at all.

    // Store detail / integration-help / woocommerce-instructions page tests
    // moved to `views::store_detail`'s own test module - those pages no
    // longer go through this engine at all.

    // Admin setup / admin settings / request-invite / admin-invites page
    // tests moved to `views::admin`'s own test module - those pages no
    // longer go through this engine at all.

    // Orders list / order detail page tests moved to `views::orders`'s own
    // test module - those pages no longer go through this engine at all.

    // Checkout / checkout-not-found / checkout-share page tests moved to
    // `views::checkout`'s own test module - those pages no longer go
    // through this engine at all.
}
