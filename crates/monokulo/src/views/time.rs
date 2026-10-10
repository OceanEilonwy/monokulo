//! Dates and times as people read them, in the viewer's time zone.
//!
//! A signed-in page uses the user's zone (`users.timezone`), or with none
//! chosen, the browser's (the `tz` cookie fx-glue.js sets), else UTC. The
//! checkout takes `?timezone=`, and without one shows UTC until its script
//! turns each time into the browser's own zone (`data-local`).

use maud::{html, Markup};

/// The zone a page shows times in, and the moment the page was made (a
/// time from this year leaves the year out).
#[derive(Debug, Clone)]
pub struct Clock {
    zone: jiff::tz::TimeZone,
    name: String,
    /// No zone was chosen: it came from the browser, or is the UTC default.
    automatic: bool,
    now: i64,
}

impl Clock {
    pub fn utc(now: i64) -> Self {
        Clock {
            zone: jiff::tz::TimeZone::UTC,
            name: "UTC".into(),
            automatic: true,
            now,
        }
    }

    /// A signed-in user's clock, as every page of theirs shows times.
    pub fn for_user(user: &crate::db::UserRow) -> Self {
        Clock::new(
            user.timezone.as_deref(),
            user.browser_timezone.as_deref(),
            crate::now_unix(),
        )
    }

    /// `chosen` wins; without it, `browser`; without that (or when neither
    /// names a real zone), UTC.
    pub fn new(chosen: Option<&str>, browser: Option<&str>, now: i64) -> Self {
        let named = |name: &str| {
            jiff::tz::TimeZone::get(name)
                .ok()
                .map(|zone| (zone, name.to_string()))
        };
        match chosen.and_then(named) {
            Some((zone, name)) => Clock {
                zone,
                name,
                automatic: false,
                now,
            },
            None => match browser.and_then(named) {
                Some((zone, name)) => Clock {
                    zone,
                    name,
                    automatic: true,
                    now,
                },
                None => Clock::utc(now),
            },
        }
    }

    /// The zone's name, `Australia/Perth`.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_automatic(&self) -> bool {
        self.automatic
    }

    /// The moment the page was made.
    pub fn now(&self) -> i64 {
        self.now
    }

    /// How long from the page's moment to `unix`, roughly: `in 6 min`,
    /// `2 min ago`, `just now`.
    pub fn relative(&self, unix: i64) -> String {
        let delta = unix - self.now;
        let span = delta.unsigned_abs();
        let amount = if span < 45 {
            return if delta > 0 { "in a moment" } else { "just now" }.to_string();
        } else if span < 90 * 60 {
            format!("{} min", span.div_ceil(60).max(1))
        } else if span < 36 * 3600 {
            format!("{} h", (span + 1800) / 3600)
        } else {
            let days = (span + 43_200) / 86_400;
            if days == 1 {
                "1 day".to_string()
            } else {
                format!("{days} days")
            }
        };
        if delta > 0 {
            format!("in {amount}")
        } else {
            format!("{amount} ago")
        }
    }

    /// `28 Sep, 14:22` (with the year when it isn't this one), in a
    /// `<time>` that never wraps; the full date, seconds and zone are in
    /// its title.
    pub fn time(&self, unix: i64) -> Markup {
        self.render(unix, false)
    }

    /// [`Self::time`] for the checkout without `?timezone=`: shown in UTC,
    /// and marked for its script to show in the browser's zone.
    pub fn time_local(&self, unix: i64) -> Markup {
        self.render(unix, true)
    }

    /// [`Self::time`], or a muted dash for nothing.
    pub fn time_or_dash(&self, unix: Option<i64>) -> Markup {
        match unix {
            Some(unix) => self.time(unix),
            None => html! { span class="muted" { "-" } },
        }
    }

    /// The short text alone, for a page title or attribute.
    pub fn text(&self, unix: i64) -> String {
        let Some(zoned) = self.zoned(unix) else {
            return unix.to_string();
        };
        let this_year = self.zoned(self.now).map(|now| now.year()) == Some(zoned.year());
        zoned
            .strftime(if this_year {
                "%-d %b, %H:%M"
            } else {
                "%-d %b %Y, %H:%M"
            })
            .to_string()
    }

    fn zoned(&self, unix: i64) -> Option<jiff::Zoned> {
        jiff::Timestamp::from_second(unix)
            .ok()
            .map(|ts| ts.to_zoned(self.zone.clone()))
    }

    fn render(&self, unix: i64, local: bool) -> Markup {
        let Some(zoned) = self.zoned(unix) else {
            return html! { (unix) };
        };
        let iso = zoned.timestamp().to_string();
        let full = format!(
            "{} ({})",
            zoned.strftime("%A %-d %B %Y, %H:%M:%S"),
            self.name
        );
        html! {
            time class="when" datetime=(iso) title=(full) data-local[local] { (self.text(unix)) }
        }
    }
}

/// Every zone name this build knows, for the time zone setting.
pub fn zone_names() -> Vec<String> {
    let mut names: Vec<String> = jiff::tz::db()
        .available()
        .map(|name| name.to_string())
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-09-28 06:22:07 UTC.
    const T: i64 = 1_790_576_527;

    #[test]
    fn a_chosen_zone_wins_then_the_browsers_then_utc() {
        assert_eq!(
            Clock::new(Some("Asia/Tokyo"), Some("Australia/Perth"), T).name(),
            "Asia/Tokyo"
        );
        let browser = Clock::new(None, Some("Australia/Perth"), T);
        assert_eq!(
            (browser.name(), browser.is_automatic()),
            ("Australia/Perth", true)
        );
        assert_eq!(Clock::new(None, Some("Not/AZone"), T).name(), "UTC");
        assert_eq!(Clock::new(Some("Not/AZone"), None, T).name(), "UTC");
    }

    #[test]
    fn times_read_as_day_month_and_clock_time_in_the_zone() {
        let perth = Clock::new(Some("Australia/Perth"), None, T);
        assert_eq!(perth.text(T), "28 Sep, 14:22");
        // Another year shows its year.
        assert_eq!(perth.text(T - 365 * 86_400), "28 Sep 2025, 14:22");
        assert_eq!(Clock::utc(T).text(T), "28 Sep, 06:22");
        let html = perth.time(T).into_string();
        assert_eq!(
            html,
            r#"<time class="when" datetime="2026-09-28T06:22:07Z" title="Monday 28 September 2026, 14:22:07 (Australia/Perth)">28 Sep, 14:22</time>"#
        );
        assert!(Clock::utc(T)
            .time_local(T)
            .into_string()
            .contains(" data-local>"));
    }
}
