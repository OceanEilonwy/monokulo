//! Composable, compile-time-checked HTML rendering (Maud), replacing the
//! old `handlebars`-based `TemplateEngine`/`.hbs` files.
//!
//! The thing that made adding a per-user theme awkward under the old setup
//! was structural, not cosmetic: every one of the ~20 old templates carried
//! its own literal `<html>`/`<head>`/`{{> nav}}` - there was no single place
//! that owned the page shell. [`layout`] and [`nav`] below are that place
//! now: every page in this module builds its own body content and hands it
//! to [`layout`], which owns `<!doctype html>` through `</html>`, the shared
//! `<head>` (`head.html`, unchanged from the old `_styles.html.hbs` partial,
//! just renamed since it's no longer a handlebars template), and the nav bar
//! (including the theme-toggle form) - so a per-user `data-theme` attribute,
//! or anything else every page needs, is a one-place change from here on.
//!
//! One submodule per page/page-group, mirroring `http/`'s own per-feature
//! split rather than one flat file.

use maud::{html, Markup, PreEscaped, DOCTYPE};

use crate::db::{Theme, UserRow};

pub mod admin;
pub mod auth;
pub mod checkout;
pub mod connect;
pub mod create_order;
pub mod dashboard;
pub mod integration_help;
pub mod landing;
pub mod orders;
pub mod pos;
pub mod status;
pub mod store_detail;
pub mod store_settings;

/// The shared `<head>` content (fonts, the full color/spacing/radius token
/// system, every component's CSS) - see that file's own header comment.
/// Unchanged by this migration: it carried no handlebars syntax at all
/// (confirmed - zero `{{` in it), so there was nothing to convert.
const HEAD_PARTIAL: &str = include_str!("head.html");

/// Shared back links for store pages and pages nested under Orders.
pub fn store_breadcrumb(connection_id: &str, display_name: &str, include_orders: bool) -> Markup {
    html! {
        nav class="context-nav" aria-label="Breadcrumb" {
            a href=(format!("/dashboard/stores/{connection_id}")) title=(display_name) { (display_name) }
            @if include_orders {
                span class="breadcrumb-sep" aria-hidden="true" { "›" }
                a href=(format!("/dashboard/stores/{connection_id}/orders")) { "Orders" }
            }
        }
    }
}

/// Every value a page needs to render the parts every page shares (the
/// `<html data-theme>` attribute and the nav bar) - built once per request,
/// almost always via [`PageChrome::from_user`], and threaded through to
/// whichever `views::*::page(...)` function is actually rendering.
pub struct PageChrome {
    pub logged_in: bool,
    pub is_admin: bool,
    pub theme: Theme,
    /// The current request's own path (+ query string, where relevant) -
    /// carried as the theme-toggle form's hidden `next` field so toggling
    /// theme redirects back to the page it was toggled from, not a fixed
    /// default. Validated with [`crate::http::dashboard::is_safe_redirect_path`]
    /// before ever being used as a redirect target, same as the login
    /// flow's own `next` - never trusted at face value just because it
    /// came from this struct.
    pub current_path: String,
    /// The engine's last known health for the status indicator:
    /// `Some(true)` healthy, `Some(false)` a problem, `None` not known yet.
    /// See `crate::http::status_page::known_health`.
    pub health: Option<bool>,
}

impl PageChrome {
    /// The constructor almost every caller wants: `user` is whatever
    /// [`AuthedUser`](crate::http::AuthedUser)/[`AuthedAdmin`](crate::http::AuthedAdmin)
    /// already resolved, or `None` for an unauthenticated page (still real
    /// per-request state for `/`/`/status`, which show "log out" for a
    /// visitor who happens to have a session - see `views::landing`/
    /// `views::status`'s own callers).
    pub fn from_user(user: Option<&UserRow>, current_path: impl Into<String>) -> Self {
        match user {
            Some(u) => PageChrome { logged_in: true, is_admin: u.is_admin, theme: u.theme, current_path: current_path.into(), health: None },
            None => PageChrome { logged_in: false, is_admin: false, theme: Theme::System, current_path: current_path.into(), health: None },
        }
    }

    pub fn with_health(mut self, health: Option<bool>) -> Self {
        self.health = health;
        self
    }
}

fn theme_attr(theme: Theme) -> Option<&'static str> {
    match theme {
        Theme::System => None,
        Theme::Light => Some("light"),
        Theme::Dark => Some("dark"),
    }
}

/// The page shell every page in this module renders through: everything
/// from `<!doctype html>` to `</html>`, nav bar included. `title` is the
/// full `<title>` text (not auto-suffixed - some pages, e.g. the landing
/// page, deliberately have no " - Monokulo" suffix, so each caller states
/// its own title exactly).
const DEFAULT_VIEWPORT: &str = "width=device-width, initial-scale=1";

pub fn layout(chrome: &PageChrome, title: &str, body: Markup) -> Markup {
    page_shell(chrome, title, DEFAULT_VIEWPORT, None, Some(nav(chrome)), body)
}

/// Same page shell, but with no nav bar - for a screen deliberately built
/// to have no site chrome at all (the POS terminal - see `views::pos`'s own
/// doc comment). Still carries the shared `<head>` and the per-user
/// `data-theme`, since a merchant who chose dark mode should get it here
/// too, even without a nav to toggle it from.
pub fn layout_bare(chrome: &PageChrome, title: &str, body: Markup) -> Markup {
    page_shell(chrome, title, DEFAULT_VIEWPORT, None, None, body)
}

/// Same as [`layout`], plus arbitrary extra `<head>` content (e.g. a
/// conditional `<meta http-equiv="refresh">`) rendered right after the
/// shared head partial - for the handful of pages that need one.
pub fn layout_with_head(chrome: &PageChrome, title: &str, extra_head: Markup, body: Markup) -> Markup {
    page_shell(chrome, title, DEFAULT_VIEWPORT, Some(extra_head), Some(nav(chrome)), body)
}

/// [`layout_bare`] plus extra `<head>` content and a custom `viewport`
/// (the POS terminal wants `maximum-scale=1, viewport-fit=cover` - no
/// accidental pinch-zoom on a counter device, and safe-area insets around a
/// notch/home-indicator - see `views::pos`'s own doc comment).
pub fn layout_bare_with_head(chrome: &PageChrome, title: &str, viewport: &str, extra_head: Markup, body: Markup) -> Markup {
    page_shell(chrome, title, viewport, Some(extra_head), None, body)
}

fn page_shell(chrome: &PageChrome, title: &str, viewport: &str, extra_head: Option<Markup>, nav: Option<Markup>, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" data-theme=[theme_attr(chrome.theme)] {
            head {
                meta charset="utf-8";
                meta name="viewport" content=(viewport);
                title { (title) }
                (PreEscaped(HEAD_PARTIAL))
                @if let Some(extra_head) = extra_head {
                    (extra_head)
                }
            }
            body {
                @if let Some(nav) = nav {
                    (nav)
                }
                (body)
            }
        }
    }
}

/// Polls `GET /status/summary` and updates the indicator in place. Starts
/// straight away only when the page was rendered without a known health;
/// skips polls while the tab is hidden.
const STATUS_INDICATOR_SCRIPT: &str = r#"(function () {
  var link = document.getElementById("status-indicator");
  if (!link) return;
  var dot = link.querySelector(".status-dot");
  var POLL_MS = 30000;
  function show(state, title) { dot.className = "status-dot status-dot-" + state; link.title = title; }
  function poll() {
    if (document.hidden) { setTimeout(poll, POLL_MS); return; }
    fetch("/status/summary", { cache: "no-store" }).then(function (r) {
      if (!r.ok) throw new Error("status unavailable");
      return r.json();
    }).then(function (data) {
      if (data && data.healthy) show("ok", "all systems healthy");
      else show("error", "an issue was detected - see the status page");
    }).catch(function () {
      show("unknown", "could not check status");
    }).finally(function () {
      setTimeout(poll, POLL_MS);
    });
  }
  setTimeout(poll, dot.classList.contains("status-dot-unknown") ? 0 : POLL_MS);
})();"#;

/// The status indicator: a dot linking to `/status`, rendered with the
/// engine's last known health so it is right without JavaScript (every
/// page load renders it afresh). Its script only adds live updates by
/// polling. `class` is the link's own class, for where it sits (nav bar,
/// POS top bar); `label` adds the visible "status" text.
pub fn status_indicator(health: Option<bool>, class: &str, label: bool) -> Markup {
    let (state, title) = match health {
        Some(true) => ("ok", "all systems healthy"),
        Some(false) => ("error", "an issue was detected - see the status page"),
        None => ("unknown", "status not checked yet"),
    };
    html! {
        a href="/status" class=(class) id="status-indicator" title=(title) {
            @if label { span class="nav-status-text" { "status" } }
            span class=(format!("status-dot status-dot-{state}")) {}
        }
        script { (PreEscaped(STATUS_INDICATOR_SCRIPT)) }
    }
}

/// The site nav - brand, log-in-state links, the no-JS theme toggle (only
/// shown once logged in: there's no account to persist a preference
/// against otherwise, and an anonymous visitor already gets a real
/// `prefers-color-scheme` experience with no control needed), admin links,
/// and the status dot.
fn nav(chrome: &PageChrome) -> Markup {
    html! {
        nav class="site-nav" {
            div class="wrap site-nav-row" {
                a href="/" class="site-nav-brand" {
                    img src="/static/logo-inverted.svg" alt="" width="24" height="24" class="site-nav-logo";
                    "Monokulo"
                }
                input type="checkbox" id="nav-toggle" class="nav-toggle-checkbox";
                label for="nav-toggle" class="nav-toggle-label" aria-label="Menu" { "☰" }
                div class="site-nav-links" {
                    a href="/dashboard" { "dashboard" }
                    @if chrome.is_admin {
                        a href="/dashboard/admin/settings" { "admin" }
                        a href="/dashboard/admin/invites" { "invites" }
                    }
                    @if chrome.logged_in {
                        form method="post" action="/dashboard/logout" class="nav-logout-form" {
                            button type="submit" class="nav-link-button" { "log out" }
                        }
                    } @else {
                        a href="/dashboard/login" { "log in" }
                        a href="/dashboard/signup" { "sign up" }
                    }
                    (status_indicator(chrome.health, "nav-status-link", true))
                    @if chrome.logged_in {
                        (theme_toggle(chrome))
                    }
                }
            }
        }
    }
}

/// Three submit buttons select Light, System, or Dark directly without JS.
/// CSS previews each option on hover/focus and animates the indicator across
/// the form navigation in browsers that support view transitions.
fn theme_toggle(chrome: &PageChrome) -> Markup {
    html! {
        form method="post" action="/dashboard/theme" class="nav-theme-form" {
            input type="hidden" name="next" value=(chrome.current_path);
            div class=(format!("theme-toggle theme-toggle-{}", chrome.theme.as_str())) role="group" aria-label="Theme" {
                span class="theme-toggle-track" {
                    button type="submit" name="theme" value="system" class="theme-toggle-option theme-toggle-option-system" aria-label="System theme" aria-pressed=(chrome.theme == Theme::System) title="System theme" {
                        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"
                            stroke-linejoin="round" aria-hidden="true" focusable="false" {
                            rect x="3" y="3" width="18" height="13" rx="2" {}
                            line x1="12" y1="16" x2="12" y2="21" {}
                            line x1="8" y1="21" x2="16" y2="21" {}
                        }
                    }
                    button type="submit" name="theme" value="light" class="theme-toggle-option theme-toggle-option-light" aria-label="Light theme" aria-pressed=(chrome.theme == Theme::Light) title="Light theme" {
                        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"
                            stroke-linejoin="round" aria-hidden="true" focusable="false" {
                            circle cx="12" cy="12" r="4" {}
                            line x1="12" y1="2" x2="12" y2="4" {}
                            line x1="12" y1="20" x2="12" y2="22" {}
                            line x1="4.2" y1="4.2" x2="5.6" y2="5.6" {}
                            line x1="18.4" y1="18.4" x2="19.8" y2="19.8" {}
                            line x1="2" y1="12" x2="4" y2="12" {}
                            line x1="20" y1="12" x2="22" y2="12" {}
                            line x1="4.2" y1="19.8" x2="5.6" y2="18.4" {}
                            line x1="18.4" y1="5.6" x2="19.8" y2="4.2" {}
                        }
                    }
                    button type="submit" name="theme" value="dark" class="theme-toggle-option theme-toggle-option-dark" aria-label="Dark theme" aria-pressed=(chrome.theme == Theme::Dark) title="Dark theme" {
                        svg viewBox="0 0 24 24" fill="currentColor" stroke="none" aria-hidden="true" focusable="false" {
                            path d="M21 12.5A9 9 0 1 1 11.5 3 7 7 0 0 0 21 12.5Z" {}
                        }
                    }
                    span class="theme-toggle-thumb" {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_indicator_is_rendered_with_the_known_health_and_polls_only_as_an_enhancement() {
        let healthy = status_indicator(Some(true), "nav-status-link", true).into_string();
        assert!(healthy.contains(r#"<a href="/status" class="nav-status-link" id="status-indicator" title="all systems healthy"><span class="nav-status-text">status</span><span class="status-dot status-dot-ok"></span></a>"#), "got: {healthy}");
        assert!(healthy.contains("/status/summary"));

        let problem = status_indicator(Some(false), "pos-status-link", false).into_string();
        assert!(problem.contains(r#"class="status-dot status-dot-error""#), "got: {problem}");
        assert!(!problem.contains("nav-status-text"));

        let unknown = status_indicator(None, "nav-status-link", true).into_string();
        assert!(unknown.contains(r#"class="status-dot status-dot-unknown""#), "got: {unknown}");

        let chrome = PageChrome::from_user(None, "/").with_health(Some(true));
        assert!(nav(&chrome).into_string().contains("status-dot status-dot-ok"));
    }

    #[test]
    fn logged_in_admin_nav_order_is_dashboard_admin_invites_logout_status_theme() {
        let chrome = PageChrome { logged_in: true, is_admin: true, theme: Theme::Dark, current_path: "/dashboard".to_string(), health: None };
        let html = nav(&chrome).into_string();

        let dashboard = html.find(r#"href="/dashboard""#).expect("dashboard link");
        let admin = html.find(r#"href="/dashboard/admin/settings""#).expect("admin link");
        let invites = html.find(r#"href="/dashboard/admin/invites""#).expect("invites link");
        let logout = html.find(r#"action="/dashboard/logout""#).expect("logout form");
        let status = html.find(r#"href="/status""#).expect("status link");
        let theme = html.find(r#"action="/dashboard/theme""#).expect("theme form");

        assert!(dashboard < admin, "dashboard must come before admin, got: {html}");
        assert!(admin < invites, "admin must come before invites, got: {html}");
        assert!(invites < logout, "invites must come before log out, got: {html}");
        assert!(logout < status, "log out must come before status, got: {html}");
        assert!(status < theme, "status must come before the theme toggle (rightmost), got: {html}");
    }

    #[test]
    fn logged_out_nav_has_no_admin_logout_or_theme_controls() {
        let chrome = PageChrome::from_user(None, "/dashboard");
        let html = nav(&chrome).into_string();
        assert!(html.contains(r#"href="/dashboard/login""#));
        assert!(html.contains(r#"href="/dashboard/signup""#));
        assert!(!html.contains("admin"));
        assert!(!html.contains(r#"action="/dashboard/logout""#));
        assert!(!html.contains("theme-toggle"));
    }

    #[test]
    fn theme_toggle_renders_a_slider_with_a_thumb_positioned_for_the_current_theme() {
        for (theme, class) in [(Theme::Light, "theme-toggle-light"), (Theme::System, "theme-toggle-system"), (Theme::Dark, "theme-toggle-dark")] {
            let chrome = PageChrome { logged_in: true, is_admin: false, theme, current_path: "/dashboard".to_string(), health: None };
            let html = nav(&chrome).into_string();
            assert!(html.contains(&class.to_string()), "expected {class} on the toggle for {theme:?}, got: {html}");
            assert!(html.contains("theme-toggle-option-light") && html.contains("theme-toggle-option-dark"), "expected both sun and moon options, got: {html}");
            assert!(html.contains("theme-toggle-thumb"), "expected a thumb element, got: {html}");
            // Each option submits its theme through the same no-JS form.
            assert!(html.contains(r#"<form method="post" action="/dashboard/theme""#));
            assert!(html.contains(r#"<input type="hidden" name="next" value="/dashboard">"#));
            for value in ["light", "system", "dark"] {
                assert!(html.contains(&format!(r#"name="theme" value="{value}""#)));
            }
            assert_eq!(html.matches("aria-pressed=\"true\"").count(), 1, "only the current theme should be pressed");
        }
    }

    #[test]
    fn theme_hover_moves_the_thumb_and_recolors_only_the_hovered_icon() {
        let css = include_str!("head.html");
        for option in ["light", "system", "dark"] {
            assert!(css.contains(&format!(".theme-toggle-option-{option}:is(:hover, :focus-visible) ~ .theme-toggle-thumb")));
        }
        assert!(css.contains(".theme-toggle:has(.theme-toggle-option:is(:hover, :focus-visible)) .theme-toggle-option { color: var(--muted); }"));
        assert!(css.contains(".theme-toggle:has(.theme-toggle-option:is(:hover, :focus-visible)) .theme-toggle-option:is(:hover, :focus-visible) { color: var(--accent-ink); }"));
    }
}
