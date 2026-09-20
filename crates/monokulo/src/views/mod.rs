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

pub mod auth;
pub mod connect;
pub mod dashboard;
pub mod integration_help;
pub mod landing;
pub mod orders;
// More page modules are added here as they're migrated off handlebars -
// `admin`, `checkout`, `invites`, `pos`, `status`, `store_detail`,
// `webhooks`.

/// The shared `<head>` content (fonts, the full color/spacing/radius token
/// system, every component's CSS) - see that file's own header comment.
/// Unchanged by this migration: it carried no handlebars syntax at all
/// (confirmed - zero `{{` in it), so there was nothing to convert.
const HEAD_PARTIAL: &str = include_str!("head.html");

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
            Some(u) => PageChrome { logged_in: true, is_admin: u.is_admin, theme: u.theme, current_path: current_path.into() },
            None => PageChrome { logged_in: false, is_admin: false, theme: Theme::System, current_path: current_path.into() },
        }
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
pub fn layout(chrome: &PageChrome, title: &str, body: Markup) -> Markup {
    page_shell(chrome, title, None, Some(nav(chrome)), body)
}

/// Same page shell, but with no nav bar - for a screen deliberately built
/// to have no site chrome at all (the POS terminal - see `views::pos`'s own
/// doc comment). Still carries the shared `<head>` and the per-user
/// `data-theme`, since a merchant who chose dark mode should get it here
/// too, even without a nav to toggle it from.
pub fn layout_bare(chrome: &PageChrome, title: &str, body: Markup) -> Markup {
    page_shell(chrome, title, None, None, body)
}

/// Same as [`layout`], plus arbitrary extra `<head>` content (e.g. a
/// conditional `<meta http-equiv="refresh">`) rendered right after the
/// shared head partial - for the handful of pages that need one.
pub fn layout_with_head(chrome: &PageChrome, title: &str, extra_head: Markup, body: Markup) -> Markup {
    page_shell(chrome, title, Some(extra_head), Some(nav(chrome)), body)
}

fn page_shell(chrome: &PageChrome, title: &str, extra_head: Option<Markup>, nav: Option<Markup>, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" data-theme=[theme_attr(chrome.theme)] {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
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

/// The status dot's own client-side poll (`GET /status/summary`) - the one
/// piece of the nav that's genuinely dynamic after load, unchanged from the
/// old `_nav.html.hbs`'s inline `<script>`.
const NAV_STATUS_SCRIPT: &str = r#"(function () {
  var dot = document.getElementById("nav-status-dot");
  if (!dot) return;
  fetch("/status/summary").then(function (r) { return r.json(); }).then(function (data) {
    if (data && data.healthy) { dot.className = "status-dot status-dot-ok"; dot.title = "all systems healthy"; }
    else { dot.className = "status-dot status-dot-error"; dot.title = "an issue was detected - see the status page"; }
  }).catch(function () { dot.className = "status-dot status-dot-unknown"; dot.title = "could not check status"; });
})();"#;

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
                    @if chrome.logged_in {
                        form method="post" action="/dashboard/logout" class="nav-logout-form" {
                            button type="submit" class="nav-link-button" { "log out" }
                        }
                        form method="post" action="/dashboard/theme" class="nav-theme-form" {
                            input type="hidden" name="next" value=(chrome.current_path);
                            button type="submit" class="nav-link-button" title="Cycle light / dark / system theme" {
                                "theme: " (chrome.theme.as_str())
                            }
                        }
                    } @else {
                        a href="/dashboard/login" { "log in" }
                        a href="/dashboard/signup" { "sign up" }
                    }
                    @if chrome.is_admin {
                        a href="/dashboard/admin/settings" { "admin" }
                        a href="/dashboard/admin/invites" { "invites" }
                    }
                    a href="/status" class="nav-status-link" {
                        span class="nav-status-text" { "status" }
                        span id="nav-status-dot" class="status-dot status-dot-unknown" title="checking..." {}
                    }
                }
            }
        }
        script { (PreEscaped(NAV_STATUS_SCRIPT)) }
    }
}
