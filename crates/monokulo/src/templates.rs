//! Minimal built-in HTML templates for the monokulo's browser-facing
//! signup/login pages (WBS 1.3.1, `http/dashboard.rs`).
//!
//! Loosely mirrors the engine's own `TemplateEngine`
//! (`../scanner/src/templates.rs` at the repo root, which loads a
//! per-tenant custom checkout template from disk with an embedded
//! fallback) but is deliberately much simpler: monokulo has no
//! per-tenant customization concept for these pages, just two fixed,
//! `include_str!`-embedded templates, always the built-in ones.

use handlebars::Handlebars;
use serde::Serialize;

// Moved to `views/head.html` as part of the Maud migration (it carried zero
// handlebars syntax to begin with) - still shared by every page here that
// hasn't moved off this engine yet.
const STYLES_PARTIAL: &str = include_str!("views/head.html");
const NAV_PARTIAL: &str = include_str!("../templates/_nav.html.hbs");

const CHECKOUT_TEMPLATE: &str = include_str!("../templates/checkout.html.hbs");
const CHECKOUT_NOT_FOUND_TEMPLATE: &str = include_str!("../templates/checkout_not_found.html.hbs");
const CHECKOUT_SHARE_TEMPLATE: &str = include_str!("../templates/checkout_share.html.hbs");
const POS_TEMPLATE: &str = include_str!("../templates/pos.html.hbs");

#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("failed to register template: {0}")]
    Register(#[from] handlebars::TemplateError),
    #[error("failed to render template: {0}")]
    Render(#[from] handlebars::RenderError),
}

/// The three `<option>` "selected" flags both connect forms' network
/// `<select>` need, derived from a submitted (or default) network value.
/// Handlebars-rust has no built-in string-equality helper - three plain
/// bools computed once here is simpler and more consistent with this
/// codebase's style than registering one. `network` not matching any known
/// value (shouldn't happen - the `<select>` only ever offers these three -
/// but a resubmitted form is still untrusted input) selects none of them,
/// same as an unrecognized value would render in a plain `<select>` anyway.
pub fn network_selected_flags(network: &str) -> (bool, bool, bool) {
    (network == "mainnet", network == "stagenet", network == "testnet")
}

/// A muted placeholder for a field with nothing to show - same
/// `<span class="muted">-</span>` convention the status page already uses
/// for "no value" (`height_display`, `_nav.html.hbs`'s own "Active" column),
/// applied here to every optional order/payment field so a merchant never
/// sees a bare, unexplained empty table cell.
const NO_VALUE: &str = "<span class=\"muted\">-</span>";

pub fn display_or_dash(value: Option<&str>) -> String {
    match value {
        Some(v) if !v.is_empty() => v.to_string(),
        _ => NO_VALUE.to_string(),
    }
}

/// Compact, non-human-readable form (raw Unix seconds) - a deliberate,
/// user-requested reversion from an earlier human-readable-date attempt on
/// this page. Kept as its own function (rather than inlining `.to_string()`
/// at every call site) purely so the muted-dash-for-`None` behavior stays
/// centralized in one place - a plain number is still `Option`-aware here,
/// it's just no longer formatted as a calendar date.
pub fn display_timestamp_or_dash(value: Option<i64>) -> String {
    match value {
        Some(v) => v.to_string(),
        None => NO_VALUE.to_string(),
    }
}

/// Same as [`display_timestamp_or_dash`] but for a timestamp that's always
/// present (`created_at`/`expires_at`/`updated_at`) - never a dash.
pub fn display_timestamp(value: i64) -> String {
    value.to_string()
}

/// `docs/order_rescan_wbs.md` Phase 5.4 - the order-detail page's "Scan range"
/// row, computed once here rather than branched on in the template.
/// `first_scanned_height` gates everything: `None` means nothing has ever
/// examined this order (predates the feature, or hasn't had its first tick yet),
/// which the other two fields can't meaningfully qualify.
pub fn display_scan_range(
    first_scanned_height: Option<i64>,
    last_scanned_height: Option<i64>,
    currently_scanning: bool,
) -> String {
    let Some(first) = first_scanned_height else {
        return NO_VALUE.to_string();
    };
    let last = last_scanned_height.unwrap_or(first);
    if currently_scanning {
        format!("{first}+")
    } else {
        format!("{first} - {last}")
    }
}

/// A moment.js-style relative duration until `target_unix` ("12h", "4h
/// 15m", "2d 4h") - at most the two largest non-zero units (days, hours,
/// minutes), a zero unit skipped rather than shown ("1d 30m", never "1d 0h
/// 30m" or "1d 0h"). `"any moment"` once `target_unix` has passed.
///
/// Computed here, server-side, rather than by client JavaScript from a raw
/// timestamp - `checkout.html.hbs` (the only caller) is the real customer-
/// facing payment page, which must stay fully meaningful with JavaScript
/// disabled.
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

/// Days since the Unix epoch (1970-01-01) for a proleptic Gregorian calendar
/// date - Howard Hinnant's well-known constant-time algorithm
/// (<http://howardhinnant.github.io/date_algorithms.html>), used instead of
/// pulling calendar formatting/parsing into the `time` crate dependency this
/// crate already has (currently used here only for `time::Duration::ZERO` on
/// a cookie) for one narrow, exactly-specified need: converting between a
/// `<input type="date">`'s `YYYY-MM-DD` value and a unix timestamp for the
/// order-rescan form's date bounds (`docs/order_rescan_wbs.md` Phase 3.2).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m as i64 + 9) % 12; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// The inverse of [`days_from_civil`] - the proleptic Gregorian calendar date
/// `z` days after the Unix epoch.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// A unix timestamp as a `YYYY-MM-DD` UTC calendar date - the exact format
/// `<input type="date">`'s `value`/`min`/`max` attributes require.
pub fn unix_to_date_string(ts: i64) -> String {
    let (y, m, d) = civil_from_days(ts.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// The inverse of [`unix_to_date_string`] - a `<input type="date">`'s
/// submitted `YYYY-MM-DD` value as the unix timestamp of that date's own UTC
/// midnight. `None` for anything not shaped like a real, syntactically valid
/// date (a browser's own native date picker should never submit one, but
/// this is form input from the network regardless - never trusted at face
/// value). Deliberately does not reject a date `days_from_civil` can compute
/// but that isn't a *real* calendar date (e.g. April 31st) - the native
/// picker itself won't offer one, and `Store::trigger_rescan`'s own `from`/
/// `to` bounds checking is what actually decides whether the resulting
/// timestamp is acceptable, not this parser.
pub fn date_string_to_unix_midnight(s: &str) -> Option<i64> {
    let mut parts = s.splitn(3, '-');
    let y = parts.next()?.parse::<i64>().ok()?;
    let m = parts.next()?.parse::<u32>().ok()?;
    let d = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400)
}

/// One payment row on the checkout page's payments table
/// (`docs/fx_refactor.md` Phase 2.2) - mirrors the engine's own (soon-
/// removed) `PaymentViewModel` field-for-field.
#[derive(Debug, Serialize)]
pub struct CheckoutPaymentViewModel {
    pub txid_short: String,
    pub amount_xmr: String,
    pub confirmations: u64,
    pub is_zero_conf: bool,
}

/// The view model `GET /pay/{pk}/orders/{payment_id}` takes - mirrors the
/// engine's own (soon-removed) `CheckoutViewModel` field-for-field, with
/// one addition (`pk`, needed by the page's own polling script to build
/// its status-check URL) and no `logged_in` at all (this page carries no
/// site nav - see `http::checkout`'s own module doc comment for why).
#[derive(Debug, Serialize)]
pub struct CheckoutViewModel {
    pub payment_id: String,
    pub status: String,
    pub status_label: String,
    pub status_class: String,
    pub address: String,
    pub qr_code_svg: String,
    pub xmr_amount: String,
    pub amount_received_xmr: String,
    pub amount: String,
    pub currency: String,
    pub confirmations: u64,
    pub confirmations_required: u64,
    /// `min(100, round(confirmations / confirmations_required * 100))`,
    /// pre-computed server-side - the progress bar's fill width is a plain
    /// `style="width: {{progress_percent}}%"`, not something a `<script>`
    /// sets after load (this page has none - see `expires_in_display`'s own
    /// doc comment on why).
    pub progress_percent: u8,
    pub is_terminal: bool,
    /// Presence only - gates the `{{#if}}` banner in the template. The
    /// actual text comes from `double_spend_detected_at_display` below,
    /// pre-rendered server-side.
    pub double_spend_detected_at: Option<i64>,
    pub double_spend_detected_at_display: String,
    /// A moment.js-style relative duration ("12h", "4h 15m", "2d 4h"),
    /// pre-rendered server-side by `format_duration_until` - this page is
    /// customer-facing and must stay fully meaningful with JavaScript
    /// disabled, so nothing about it (including this) may depend on
    /// `<script>` to be understandable. Only shown when `!is_terminal`;
    /// meaningless (and unused) otherwise.
    pub expires_in_display: String,
    /// `""`, `"expiry-soon"`, or `"expiry-urgent"` - a CSS class picked
    /// server-side from how much time is actually left (see
    /// `http::checkout::render_checkout_page`), so the timer can shift color
    /// as expiry nears without any client-side timer/JS of its own. Always
    /// `""` once `is_terminal` (an expired/paid/overpaid order has nothing
    /// left to be urgent about).
    pub expiry_urgency_class: String,
    pub merchant_order_id: Option<String>,
    /// `Some` once a customer (or their storefront, on their behalf) has
    /// set one via the form below - shown read-only from then on. `None`
    /// shows the form instead. Raw, not pre-rendered to a trusted-HTML
    /// display string - a refund address is caller-supplied free text (the
    /// engine's own `set_refund_address` does no format validation of its
    /// own either), so it stays ordinary escaped template output, same
    /// caveat `merchant_order_id`'s own doc comment already carries.
    pub refund_address: Option<String>,
    /// Set only when the refund-address form below was just rejected (empty
    /// submission, or a real engine failure) - `None` on a plain page load.
    pub refund_address_error: Option<String>,
    pub pk: String,
    pub payments: Vec<CheckoutPaymentViewModel>,
}

/// The view model `GET /pay/{pk}/orders/{payment_id}/share` takes (a real
/// follow-up to `docs/fx_refactor.md` - see `http::checkout::checkout_share_page`'s
/// own doc comment). Unlike `CheckoutViewModel` this page *does* carry the
/// site nav, so `logged_in` is real here, computed the same
/// authenticated-or-not way `status_page.rs`'s own unauthenticated page
/// does - the customer paying an invoice usually isn't a logged-in
/// merchant, but the nav should reflect reality either way, not assume one.
#[derive(Debug, Serialize)]
pub struct CheckoutShareViewModel {
    pub pk: String,
    pub payment_id: String,
    /// Whether the order actually exists - `false` renders a real
    /// not-found state (still with the site's own nav around it, unlike
    /// `checkout_not_found`'s bare equivalent), rather than a page whose
    /// only content is a broken iframe.
    pub found: bool,
    pub logged_in: bool,
    pub is_admin: bool,
}

/// `http/pos.rs::pos_page` - the terminal screen's own static shell. Every
/// live value (the entered amount, the QR/URI/NFC payment view, the
/// tick/progress overlay, backgrounded payments stacked at the bottom) is
/// driven client-side by JS talking to `http::pos`'s JSON endpoints - see
/// `http::pos`'s own module doc comment for why this screen, unlike the
/// public checkout page, leans on JS rather than working around it.
#[derive(Debug, Serialize)]
pub struct PosViewModel {
    pub connection_id: String,
    pub display_name: String,
    pub base_currency: String,
    /// How many decimal places the keypad's digit-shift should keep before
    /// inserting a decimal point - `2` for every fiat currency (matching
    /// `shared::exchange_rate::compute_xmr_amount`'s own 2-decimal-place
    /// limit), `12` when this store's `base_currency` is itself `"XMR"`
    /// (matching `shared::exchange_rate::parse_xmr_to_piconero`'s own native
    /// precision) - see `http::pos::pos_page`'s own doc comment.
    pub base_currency_decimals: u8,
    pub logged_in: bool,
    pub is_admin: bool,
}

// SetupViewModel/RequestInviteViewModel/AdminInviteRequestRow/AdminInvitesViewModel/
// AdminScalarFieldView/AdminNetworkFieldView/AdminSettingsViewModel moved to
// `views::admin` as part of the Maud migration - those pages no longer go
// through this engine at all.

pub struct TemplateEngine {
    handlebars: Handlebars<'static>,
}

impl TemplateEngine {
    pub fn new() -> Result<Self, TemplateError> {
        let mut handlebars = Handlebars::new();
        handlebars.set_strict_mode(true);
        // Partials shared by every page below - see their own files'
        // comments. Registered under names with no leading underscore
        // (handlebars-rust has no notion of "partial vs. template", a
        // registered template is callable as `{{> name}}` by whatever name
        // it's registered under) so `{{> styles}}`/`{{> nav}}` read cleanly
        // from every page.
        handlebars.register_template_string("styles", STYLES_PARTIAL)?;
        handlebars.register_template_string("nav", NAV_PARTIAL)?;

        handlebars.register_template_string("checkout", CHECKOUT_TEMPLATE)?;
        handlebars.register_template_string("checkout_not_found", CHECKOUT_NOT_FOUND_TEMPLATE)?;
        handlebars.register_template_string("checkout_share", CHECKOUT_SHARE_TEMPLATE)?;
        handlebars.register_template_string("pos", POS_TEMPLATE)?;
        Ok(TemplateEngine { handlebars })
    }

    pub fn render_checkout(&self, data: &CheckoutViewModel) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("checkout", data)?)
    }

    pub fn render_checkout_not_found(&self) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("checkout_not_found", &())?)
    }

    pub fn render_checkout_share(&self, data: &CheckoutShareViewModel) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("checkout_share", data)?)
    }

    pub fn render_pos(&self, data: &PosViewModel) -> Result<String, TemplateError> {
        Ok(self.handlebars.render("pos", data)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_timestamp_or_dash_shows_a_raw_compact_number_not_a_human_readable_date() {
        assert_eq!(display_timestamp_or_dash(Some(1_700_000_000)), "1700000000");
        assert_eq!(display_timestamp_or_dash(None), NO_VALUE);
        assert_eq!(display_timestamp(1_700_000_000), "1700000000");
    }

    #[test]
    fn unix_to_date_string_matches_known_dates() {
        assert_eq!(unix_to_date_string(0), "1970-01-01");
        assert_eq!(unix_to_date_string(86_399), "1970-01-01", "one second before the next day rolls over");
        assert_eq!(unix_to_date_string(86_400), "1970-01-02");
        // 2024-02-29 12:00:00 UTC - a real leap day, not a hypothetical one.
        assert_eq!(unix_to_date_string(1_709_208_000), "2024-02-29");
        // 2026-01-01 00:00:00 UTC.
        assert_eq!(unix_to_date_string(1_767_225_600), "2026-01-01");
        // A negative timestamp (before the epoch) must still resolve to a real
        // date, not panic or wrap - `div_euclid` is what makes this correct.
        assert_eq!(unix_to_date_string(-1), "1969-12-31");
    }

    #[test]
    fn date_string_to_unix_midnight_round_trips_with_unix_to_date_string() {
        for ts in [0i64, 86_400, 1_709_208_000, 1_767_225_600, 1_700_000_000] {
            let date = unix_to_date_string(ts);
            let midnight = date_string_to_unix_midnight(&date).unwrap();
            assert_eq!(unix_to_date_string(midnight), date, "midnight of {date} must itself format back to {date}");
        }
        // A known, hand-checked pair, not just an internal round trip.
        assert_eq!(date_string_to_unix_midnight("2024-02-29").unwrap(), 1_709_164_800);
    }

    #[test]
    fn date_string_to_unix_midnight_rejects_malformed_input_rather_than_panicking() {
        for bad in ["", "not-a-date", "2024-02", "2024-13-01", "2024-01-32", "2024-01-01-extra", "2024/01/01"] {
            assert!(date_string_to_unix_midnight(bad).is_none(), "expected {bad:?} to be rejected");
        }
    }

    #[test]
    fn display_or_dash_shows_the_muted_placeholder_for_none_or_empty() {
        assert_eq!(display_or_dash(Some("real value")), "real value");
        assert_eq!(display_or_dash(None), NO_VALUE);
        assert_eq!(display_or_dash(Some("")), NO_VALUE, "an empty string is not a real value either");
    }

    #[test]
    fn format_duration_until_matches_the_moment_js_style_examples() {
        let now = 1_700_000_000;
        assert_eq!(format_duration_until(now + 12 * 3600, now), "12h");
        assert_eq!(format_duration_until(now + 4 * 3600 + 15 * 60, now), "4h 15m");
        assert_eq!(format_duration_until(now + 2 * 86400 + 4 * 3600, now), "2d 4h");
    }

    #[test]
    fn format_duration_until_skips_a_zero_unit_rather_than_showing_it() {
        let now = 1_700_000_000;
        // 1 day, 0 hours, 30 minutes - the zero hour must not crowd out the
        // real second part or appear as "1d 0h".
        assert_eq!(format_duration_until(now + 86400 + 30 * 60, now), "1d 30m");
    }

    #[test]
    fn format_duration_until_never_shows_more_than_two_parts() {
        let now = 1_700_000_000;
        // 2 days, 4 hours, 30 minutes - minutes is dropped, not appended as a third part.
        assert_eq!(format_duration_until(now + 2 * 86400 + 4 * 3600 + 30 * 60, now), "2d 4h");
    }

    #[test]
    fn format_duration_until_handles_under_a_minute_and_already_passed() {
        let now = 1_700_000_000;
        assert_eq!(format_duration_until(now + 30, now), "<1m");
        assert_eq!(format_duration_until(now, now), "any moment");
        assert_eq!(format_duration_until(now - 100, now), "any moment", "an already-passed target must not show a negative duration");
    }

    // Signup/login page tests moved to `views::auth`'s own test module -
    // those pages no longer go through this engine at all.

    // Connect / connect-platform / new-store-picker page tests moved to
    // `views::connect`'s own test module - those pages no longer go through
    // this engine at all.

    // Landing page tests moved to `views::landing`'s own test module - that
    // page no longer goes through this engine at all.

    // Dashboard home page tests moved to `views::dashboard`'s own test
    // module - that page no longer goes through this engine at all.

    // Store detail / integration-help / woocommerce-instructions page tests
    // moved to `views::store_detail`'s own test module - those pages no
    // longer go through this engine at all.

    // Admin setup / admin settings / request-invite / admin-invites page
    // tests moved to `views::admin`'s own test module - those pages no
    // longer go through this engine at all.

    fn test_checkout_view_model(is_terminal: bool) -> CheckoutViewModel {
        CheckoutViewModel {
            payment_id: "pay_abc123".to_string(),
            status: if is_terminal { "paid".to_string() } else { "pending".to_string() },
            status_label: if is_terminal { "Paid".to_string() } else { "Waiting for payment".to_string() },
            status_class: if is_terminal { "status-paid".to_string() } else { "status-pending".to_string() },
            address: "86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC".to_string(),
            qr_code_svg: "<svg></svg>".to_string(),
            xmr_amount: "0.500000000000".to_string(),
            amount_received_xmr: "0.000000000000".to_string(),
            amount: "0.5".to_string(),
            currency: "XMR".to_string(),
            confirmations: 0,
            confirmations_required: 10,
            progress_percent: 0,
            is_terminal,
            double_spend_detected_at: None,
            double_spend_detected_at_display: display_timestamp_or_dash(None),
            expires_in_display: "30m".to_string(),
            expiry_urgency_class: String::new(),
            merchant_order_id: None,
            refund_address: None,
            refund_address_error: None,
            pk: "pk_abc123".to_string(),
            payments: vec![],
        }
    }

    #[test]
    fn checkout_page_carries_no_script_and_meta_refreshes_a_still_in_progress_order() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine.render_checkout(&test_checkout_view_model(false)).unwrap();
        assert!(!html.contains("<script"), "the checkout page must carry no JavaScript at all, got: {html}");
        assert!(
            html.contains(r#"<meta http-equiv="refresh""#),
            "expected a meta-refresh directive on a still-in-progress order, got: {html}"
        );
    }

    #[test]
    fn checkout_page_stops_meta_refreshing_once_the_order_is_terminal() {
        let engine = TemplateEngine::new().unwrap();
        let html = engine.render_checkout(&test_checkout_view_model(true)).unwrap();
        assert!(!html.contains("<script"), "the checkout page must carry no JavaScript at all, got: {html}");
        assert!(
            !html.contains(r#"<meta http-equiv="refresh""#),
            "a paid/terminal order must not keep re-fetching itself, got: {html}"
        );
    }

    // Orders list / order detail page tests moved to `views::orders`'s own
    // test module - those pages no longer go through this engine at all.
}
