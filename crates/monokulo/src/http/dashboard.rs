//! Browser-facing signup/login pages (WBS 1.3.1) - a separate, human-facing
//! surface layered on top of the same account-creation/login logic the JSON
//! `POST /signup`/`POST /login` API uses (`signup::create_account`,
//! `login::authenticate` - factored out of those handlers for exactly this
//! reuse, see their own doc comments). The JSON API stays exactly as-is;
//! nothing here changes its behavior, request/response shape, or routes.
//!
//! Ends in a session cookie rather than a bearer token a human would have
//! to copy-paste out of a JSON body, per the WBS's own design decision for
//! this task. The cookie holds the exact same raw session token
//! `POST /login`'s JSON response returns as `session_token` - just delivered
//! as `Set-Cookie` instead. [`super::AuthedUser`] accepts either form (see
//! its doc comment), so this is genuinely one auth system with two ways in,
//! not two parallel ones.
//!
//! A plain HTML `<form>` posts `application/x-www-form-urlencoded`, not
//! JSON - hence `axum::extract::Form` here instead of `axum::extract::Json`.

use axum::extract::{Form, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use serde::Deserialize;

use crate::db::{Theme, UserRow};
use crate::views;

use super::connections::{self, CreateConnectionError, CreateConnectionFields};
use super::login::{self, LoginError};
use super::signup::{self, CreateAccountError};
use super::AppState;
use super::AuthedUser;
use crate::db::Database;

#[derive(Deserialize)]
pub struct SignupForm {
    pub email: String,
    pub password: String,
    /// The hidden field `signup.html.hbs` always renders (see
    /// `views::auth::SignupViewModel::invite_token`'s own doc comment) - empty in
    /// `"public"` mode, where it's submitted but simply ignored.
    #[serde(default)]
    pub invite: String,
    /// Where the visitor was going before signing up (a plugin's connect
    /// page): carried through wallet setup and back there.
    #[serde(default)]
    pub next: Option<String>,
}

#[derive(Deserialize)]
pub struct SignupQuery {
    /// `GET /dashboard/signup?invite=<token>` - carried straight into the
    /// rendered form's hidden field, unvalidated (see
    /// `views::auth::SignupViewModel::invite_token`'s own doc comment on why validation
    /// only ever happens at submit time).
    #[serde(default)]
    pub invite: Option<String>,
    #[serde(default)]
    pub next: Option<String>,
}

#[derive(Deserialize)]
pub struct LoginForm {
    pub email: String,
    pub password: String,
    /// Carried through from `GET /dashboard/login?next=...`'s hidden form
    /// field (see `LoginQuery`/`login_form` below) - `None` whenever no
    /// `next` was ever in play, which keeps every existing caller that never
    /// sends this field working unchanged (see module doc comment on
    /// `login_submit`). Never trusted as a redirect target as-is - see
    /// [`is_safe_redirect_path`].
    pub next: Option<String>,
}

/// Query parameters `GET /dashboard/login` accepts (WBS 1.4.1) - just the
/// optional `next` value the generic connect flow (`http/connect.rs`)
/// redirects here with when the browser has no session yet. Rendered
/// straight into the login page's hidden `next` field, unvalidated - see
/// [`is_safe_redirect_path`]'s own doc comment on why validating it only
/// matters, and only happens, at the point it's actually used as a redirect
/// location (`login_submit`), not here at render time.
#[derive(Deserialize)]
pub struct LoginQuery {
    pub next: Option<String>,
}

/// Validates that `next` is safe to use as an internal redirect target -
/// this is the one thing standing between `login_submit` and an
/// open-redirect vulnerability, since `next` is otherwise fully
/// attacker-controlled (anyone can link a victim straight to
/// `/dashboard/login?next=<anything>`, not just the connect flow that
/// legitimately sets it).
///
/// Requires `next` to be a same-origin, relative path:
/// - starts with exactly one `/` (a bare relative path);
/// - never starts with `//` - a protocol-relative URL (`//evil.example.com`
///   resolves, in every browser, to `https://evil.example.com`) is the
///   classic bypass a plain "starts with /" check misses entirely;
/// - never contains a backslash - some browsers normalize a leading
///   backslash to a forward slash while parsing a URL, so `/\evil.example.com`
///   can behave identically to `//evil.example.com` even though it doesn't
///   start with two literal `/` characters;
/// - never contains a `:` before the first `/`, `?`, or `#` - rules out a
///   `next` value that is actually an absolute URL carrying its own scheme
///   (`https://evil.example.com`, `javascript:...`), which wouldn't be
///   caught by the checks above since those only look at the very start of
///   the string.
///
/// A `next` that fails any of these is not an error - the caller
/// (`login_submit`) just falls back to its existing default behavior, same
/// as if `next` had never been provided at all.
fn is_safe_redirect_path(next: &str) -> bool {
    if !next.starts_with('/') || next.starts_with("//") || next.contains('\\') {
        return false;
    }
    let path_part = next.split(['?', '#']).next().unwrap_or(next);
    !path_part.contains(':')
}

/// A path on this site, safe to send a browser to: made only by
/// [`SafePath::parse`], so a `next` value a request carried can't reach
/// [`redirect_to`] unchecked (an open redirect).
pub(crate) struct SafePath(String);

impl SafePath {
    /// `next`, if [`is_safe_redirect_path`] accepts it.
    pub(crate) fn parse(next: &str) -> Option<Self> {
        is_safe_redirect_path(next).then(|| SafePath(next.to_string()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// A `302` to a checked path that came from a request.
pub(crate) fn redirect_to(path: &SafePath) -> Response {
    redirect_302(path.as_str())
}

/// `POST /dashboard/connect`'s form fields (WBS 1.3.2) - the browser
/// equivalent of `POST /connections`'s JSON body, minus `platform` (hardcoded
/// to `"woocommerce"` below - a real "choose a platform" UI is a later, fuller
/// dashboard concern) and `order_expiry_seconds` (left `None` here so the
/// engine's own default applies - no UI field for it yet).
/// `confirmations_required` *is* carried (same `#[serde(default)]`-to-`None`,
/// no-UI-field-yet treatment `connect.rs::ConfirmForm` already gives it, for
/// the identical reason: a caller that needs a non-default value - most
/// concretely, a real stagenet e2e test that wants `0` (native 0-conf)
/// rather than waiting on real block times - has a real way to set it
/// through this flow instead of only the JSON `POST /connections` surface.
/// There's no allowed-origins field: which websites may show a store's
/// checkout is now the store's verified domains (`crate::embed_domains`),
/// and the site URL's domain is added there for the merchant to verify.
#[derive(Deserialize)]
pub struct ConnectForm {
    pub site_url: String,
    #[serde(default)]
    pub view_key_hex: String,
    #[serde(default)]
    pub spend_pubkey_hex: String,
    /// The keys encrypted for SEV-SNP key storage (`key_entry`), instead of
    /// the two above.
    #[serde(default)]
    pub encrypted_keys: Option<String>,
    #[serde(default)]
    pub network: String,
    /// Validated against `crate::currencies` in `connect_submit` - see
    /// that module's own doc comment.
    #[serde(default)]
    pub base_currency: String,
    #[serde(default)]
    pub confirmations_required: Option<u64>,
    /// Only sent when the form offered a choice (part 5).
    #[serde(default)]
    pub key_custody_backend: Option<String>,
    /// The merchant's wallet the store takes payments into (docs/wallets.md).
    /// A caller that sends keys instead gets them added as a wallet.
    #[serde(default)]
    pub wallet_id: Option<String>,
}

/// `chrome.logged_in` is always `false` here, not a real per-request
/// session check - this page's whole purpose is establishing a *new*
/// session, so showing the sign-up/log-in links regardless of any existing
/// one is the reasonable default (see `views::auth`'s own doc comment).
async fn render_signup(
    state: &AppState,
    error: Option<&str>,
    invite_token: &str,
    next: Option<&str>,
) -> Response {
    let invite_only = state.settings.signup_mode() == crate::settings::SignupMode::InviteOnly;
    let invite_required = invite_only && invite_token.trim().is_empty();
    let chrome = super::page_chrome(state, None, "").await;
    let data = views::auth::SignupViewModel {
        error: error.map(str::to_string),
        invite_required,
        invite_token: invite_token.to_string(),
        next: next
            .and_then(SafePath::parse)
            .map(|p| p.as_str().to_owned()),
    };
    views::auth::signup_page(&chrome, &data).into_response()
}

/// Same `chrome.logged_in == false` reasoning as `render_signup` above.
async fn render_login(state: &AppState, error: Option<&str>, next: Option<&str>) -> Response {
    let chrome = super::page_chrome(state, None, "").await;
    let data = views::auth::LoginViewModel {
        error: error.map(str::to_string),
        connecting_site: super::wallets::connecting_site(next),
        next: next.map(str::to_string),
    };
    views::auth::login_page(&chrome, &data).into_response()
}

/// `resubmit` is `None` on a plain `GET` (empty form, mainnet selected by
/// default) or `Some(&form)` when re-rendering after a rejected `POST` - in
/// which case every field the merchant typed, including the two key hex
/// fields, is echoed straight back rather than lost. See
/// `ConnectViewModel`'s own doc comment for why that's the right call here
/// (these are plain-text inputs already, not password fields - echoing
/// doesn't change what was ever visible on the merchant's own screen).
async fn render_connect_form(
    state: &AppState,
    error: Option<&str>,
    resubmit: Option<&ConnectForm>,
    user: &UserRow,
) -> Response {
    let selected_currency = resubmit.map(|f| f.base_currency.as_str()).unwrap_or("XMR");
    let (selected, user_id) = (selected_currency.to_string(), user.id.clone());
    let (currency_options, wallets) = state
        .db
        .read(move |db| {
            Ok::<_, crate::db::DbError>((
                crate::currencies::currency_options(db, &selected).unwrap_or_default(),
                db.list_wallets(&user_id).unwrap_or_default(),
            ))
        })
        .await
        .unwrap_or_default();
    let chrome = super::page_chrome(state, Some(user), "/dashboard/connect").await;
    let data = views::connect::ConnectViewModel {
        error: error.map(str::to_string),
        public_key: None,
        connection_id: None,
        public_url: None,
        site_url: resubmit.map(|f| f.site_url.clone()).unwrap_or_default(),
        currency_options,
        wallets,
        selected_wallet: resubmit.and_then(|f| f.wallet_id.clone()),
    };
    views::connect::page(&chrome, &data).into_response()
}

/// `303`-free, deliberate `302 Found` redirect (axum's own `Redirect::to`
/// issues `303 See Other` instead - see its doc comment - and the WBS spec
/// for this task calls out `302` specifically). `pub(super)` since the
/// generic connect flow (`http/connect.rs`, WBS 1.4.1) issues the exact same
/// kind of redirect and shouldn't reimplement it.
pub(crate) fn redirect_302(location: &str) -> Response {
    (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
}

/// After a form post: "see this page", fetched with a `GET` whatever the
/// post was, so reloading it never posts again.
pub(crate) fn redirect_303(location: &str) -> Response {
    (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response()
}

pub async fn signup_form(
    State(state): State<AppState>,
    Query(query): Query<SignupQuery>,
) -> Response {
    render_signup(
        &state,
        None,
        query.invite.as_deref().unwrap_or(""),
        query.next.as_deref(),
    )
    .await
}

pub async fn signup_submit(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Form(form): Form<SignupForm>,
) -> Response {
    match signup::create_account(
        &state,
        &form.email,
        &form.password,
        false,
        Some(&form.invite),
    )
    .await
    {
        // Logged in straight away, and on to setting up the first wallet:
        // a store needs one. Where they were going (a plugin's connect
        // page) comes after it.
        Ok(user_id) => {
            let user_id = crate::db::UserId::new(user_id);
            let Ok(raw_token) = login::start_session(&state, &user_id).await else {
                return redirect_302("/dashboard/login");
            };
            let cookie = super::session_cookie(&headers, raw_token.expose().to_string());
            let jar = CookieJar::new().add(cookie);
            let setup = match form.next.as_deref().and_then(SafePath::parse) {
                Some(next) => format!(
                    "/dashboard/wallets/setup?next={}",
                    url::form_urlencoded::byte_serialize(next.as_str().as_bytes())
                        .collect::<String>()
                ),
                None => "/dashboard/wallets/setup".to_owned(),
            };
            (jar, redirect_302(&setup)).into_response()
        }
        Err(CreateAccountError::DuplicateEmail) => {
            render_signup(
                &state,
                Some("That email is already registered. Try logging in instead."),
                &form.invite,
                form.next.as_deref(),
            )
            .await
        }
        Err(CreateAccountError::Internal) => {
            render_signup(
                &state,
                Some("Something went wrong. Please try again."),
                &form.invite,
                form.next.as_deref(),
            )
            .await
        }
        Err(CreateAccountError::InviteRequired) => {
            render_signup(&state, None, "", form.next.as_deref()).await
        }
        Err(CreateAccountError::InvalidOrUsedInvite) => render_signup(
            &state,
            Some("That invite link is invalid or has already been used. Please request a new one."),
            "",
            form.next.as_deref(),
        )
        .await,
        Err(CreateAccountError::WeakPassword) => {
            render_signup(
                &state,
                Some(&format!(
                    "The password must be at least {} characters.",
                    signup::MIN_PASSWORD_LEN
                )),
                &form.invite,
                form.next.as_deref(),
            )
            .await
        }
        Err(CreateAccountError::InvalidEmail) => {
            render_signup(
                &state,
                Some("That is not an email address."),
                &form.invite,
                form.next.as_deref(),
            )
            .await
        }
        // Not a path a non-admin signup takes.
        Err(CreateAccountError::AlreadySetUp) => redirect_302("/dashboard/login"),
    }
}

pub async fn login_form(
    State(state): State<AppState>,
    Query(query): Query<LoginQuery>,
) -> Response {
    render_login(&state, None, query.next.as_deref()).await
}

/// `POST /dashboard/logout` - the browser-facing nav's "log out" link (a
/// tiny same-origin form, not a bare `<a href>`, since this is a
/// state-changing action - see `_nav.html.hbs`). Behind [`AuthedUser`] like
/// every other `/dashboard/*` route, so it accepts the session cookie the
/// same way every other protected page already does; reuses the exact same
/// delete-session logic `POST /logout` (the JSON API, `http/logout.rs`)
/// runs, just via `AuthedUser`'s cookie path rather than its `Bearer` one.
/// Clears the cookie in the response (an empty value with `max_age(0)`) so
/// the browser doesn't keep presenting a now-deleted session token on its
/// next request, then redirects to `/` - a human clicking "log out" expects
/// a real page back, not `logout::logout`'s bare `204` (which is correct
/// for the JSON API, wrong for a browser form submission).
pub async fn logout_submit(
    State(db): State<Database>,
    headers: axum::http::HeaderMap,
    AuthedUser(_user, token_hash): AuthedUser,
) -> Response {
    // The cookie goes either way; a session row that stays is logged, as
    // it would still work for whoever has the token.
    if let Err(e) = db.write(move |db| db.delete_session(&token_hash)).await {
        tracing::error!(error = %e, "logging out failed to delete the session");
    }
    let mut cookie = super::session_cookie(&headers, String::new());
    cookie.set_max_age(time::Duration::ZERO);
    let jar = CookieJar::new().add(cookie);
    (jar, redirect_302("/")).into_response()
}

/// `POST /dashboard/theme` - the nav's no-JS theme selector. Named submit
/// buttons select a theme directly; older forms without a choice still cycle.
/// Persists the result against the
/// authenticated user (not a cookie - see the design-language rollout's own
/// migration comment, `migrations/0017_user_theme.sql`, for why this is
/// server-side per-user state rather than client storage), then redirects
/// back to wherever the form was submitted from - the same validated-`next`
/// pattern `login_submit` already uses (see [`is_safe_redirect_path`]),
/// falling back to `/dashboard` for anything that doesn't validate. No CSRF
/// token needed beyond what `logout_submit` above already relies on
/// (`SameSite=Lax`) - same-origin `<form method="post">`, nothing more.
#[derive(Deserialize)]
pub struct TimezoneForm {
    /// A zone name, or empty for automatic.
    pub timezone: String,
}

/// `POST /dashboard/timezone`: the zone dates and times are shown in. An
/// unknown name is ignored rather than saved.
pub async fn timezone_submit(
    State(db): State<Database>,
    AuthedUser(user, _): AuthedUser,
    Form(form): Form<TimezoneForm>,
) -> Response {
    let chosen = form.timezone.trim().to_string();
    let zone = if chosen.is_empty() {
        Some(None)
    } else if jiff::tz::TimeZone::get(&chosen).is_ok() {
        Some(Some(chosen))
    } else {
        None
    };
    if let Some(zone) = zone {
        db.write(move |db| db.update_user_timezone(&user.id, zone.as_deref()))
            .await
            .ok();
    }
    redirect_302("/dashboard#timezone")
}

#[derive(Deserialize)]
pub struct ThemeForm {
    pub next: Option<String>,
    pub theme: Option<String>,
}

fn selected_theme(current: Theme, submitted: Option<&str>) -> Theme {
    match submitted {
        Some("light") => Theme::Light,
        Some("system") => Theme::System,
        Some("dark") => Theme::Dark,
        _ => current.next(),
    }
}

pub async fn theme_submit(
    State(db): State<Database>,
    AuthedUser(user, _): AuthedUser,
    Form(form): Form<ThemeForm>,
) -> Response {
    let next_theme = selected_theme(user.theme, form.theme.as_deref());
    let user_id = user.id.clone();
    db.write(move |db| db.update_user_theme(&user_id, next_theme))
        .await
        .ok();
    match form.next.as_deref().and_then(SafePath::parse) {
        Some(next) => redirect_to(&next),
        None => redirect_302("/dashboard"),
    }
}

#[cfg(test)]
mod theme_selector_tests {
    use super::*;

    #[test]
    fn named_theme_options_select_directly() {
        for current in [Theme::Light, Theme::System, Theme::Dark] {
            assert_eq!(selected_theme(current, Some("light")), Theme::Light);
            assert_eq!(selected_theme(current, Some("system")), Theme::System);
            assert_eq!(selected_theme(current, Some("dark")), Theme::Dark);
            assert_eq!(selected_theme(current, None), current.next());
        }
    }
}

/// `POST /dashboard/login`. WBS 1.4.1 adds `next`-redirect support on top of
/// WBS 1.3.1's original behavior: if a *validated* `next` was carried
/// through the form (see [`is_safe_redirect_path`]), a successful login
/// redirects there instead of rendering the inline "you're logged in"
/// confirmation below - this is what lets the generic connect flow
/// (`http/connect.rs`) send an unauthenticated browser to log in and land
/// back exactly where it started. When `next` is absent (the overwhelming
/// majority of logins - anyone reaching `/dashboard/login` directly, not via
/// the connect flow) or fails validation, behavior is *exactly* what it was
/// before this task: the same inline confirmation, unchanged.
pub async fn login_submit(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
    match login::authenticate(&state, &form.email, &form.password).await {
        Ok((_user, raw_token)) => {
            // See `http::session_cookie` for the cookie's attributes.
            let cookie = super::session_cookie(&headers, raw_token.expose().to_string());
            let jar = CookieJar::new().add(cookie);

            // A validated `next` wins over the default confirmation - see
            // this function's own doc comment and [`is_safe_redirect_path`].
            if let Some(next) = form.next.as_deref().and_then(SafePath::parse) {
                return (jar, redirect_to(&next)).into_response();
            }

            // A real dashboard home page exists now (`http/home.rs`) - a
            // plain login with no `next` lands there, same as any other
            // "you're logged in, here's your stuff" flow.
            (jar, redirect_302("/dashboard")).into_response()
        }
        Err(LoginError::Unauthorized) => {
            render_login(
                &state,
                Some("Invalid email or password."),
                form.next.as_deref(),
            )
            .await
        }
        Err(LoginError::Internal) => {
            render_login(
                &state,
                Some("Something went wrong. Please try again."),
                form.next.as_deref(),
            )
            .await
        }
    }
}

/// `GET /dashboard/connect` (WBS 1.3.2) - behind [`AuthedUser`]. A missing or
/// invalid session gets the exact same `401` `AuthedUser` already returns for
/// every other protected route in this crate (the JSON API's own
/// `/connections` included) - no redirect-on-401 behavior exists anywhere in
/// the dashboard yet, so a bare `401` here is the consistent choice rather
/// than inventing new behavior for just this one route.
pub async fn connect_form(
    State(state): State<AppState>,
    AuthedUser(user, _token_hash): AuthedUser,
) -> Response {
    render_connect_form(&state, None, None, &user).await
}

/// `POST /dashboard/connect` (WBS 1.3.2) - the form equivalent of
/// `POST /connections`, calling the exact same
/// [`connections::create_connection_for_user`] both surfaces share.
/// `platform` isn't a visible field yet (hardcoded to `"woocommerce"` below -
/// a real "choose a platform" UI is a later, fuller dashboard concern per the
/// WBS's own Stage 4/dashboard notes); `confirmations_required`/
/// `order_expiry_seconds` aren't visible fields either and are left `None` so
/// the engine's own defaults apply, consistent with the JSON API already
/// treating them as optional.
pub async fn connect_submit(
    State(state): State<AppState>,
    AuthedUser(user, _token_hash): AuthedUser,
    Form(form): Form<ConnectForm>,
) -> Response {
    let fields = CreateConnectionFields {
        // "custom", not "woocommerce" - this is the advanced/direct-API
        // form (WBS follow-up UI work's own "custom (advanced)" picker
        // option, distinct from the real `/connect/{platform}` flow a
        // WooCommerce plugin drives). Was wrongly hardcoded to
        // "woocommerce" (a copy-paste leftover from before that picker
        // existed) - real user-reported bug: a store connected here showed
        // "woocommerce" as its platform on the dashboard and got shown
        // WooCommerce-specific integration instructions on its store page,
        // neither of which is true for a store connected this way.
        platform: "custom".to_string(),
        site_url: form.site_url.clone(),
        view_key_hex: form.view_key_hex.clone(),
        spend_pubkey_hex: form.spend_pubkey_hex.clone(),
        encrypted_keys: form.encrypted_keys.clone(),
        network: Some(form.network.clone()).filter(|n| !n.is_empty()),
        domains: Vec::new(),
        confirmations_required: form.confirmations_required,
        order_expiry_seconds: None,
        base_currency: form.base_currency.clone(),
        key_custody_backend: form.key_custody_backend.clone().filter(|b| !b.is_empty()),
        wallet_id: form
            .wallet_id
            .clone()
            .filter(|w| !w.is_empty())
            .map(crate::db::WalletId::new),
    };

    match connections::create_connection_for_user(&state, &user, fields).await {
        Ok(outcome) => redirect_302(&format!(
            "/dashboard/stores/{}/setup",
            outcome.connection_id
        )),
        Err(CreateConnectionError::BadRequest(message)) => {
            render_connect_form(&state, Some(&message), Some(&form), &user).await
        }
        Err(CreateConnectionError::Internal) => {
            render_connect_form(
                &state,
                Some("Something went wrong. Please try again."),
                Some(&form),
                &user,
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_safe_redirect_path, SafePath};

    /// A redirect target from a request exists only once checked.
    #[test]
    fn a_safe_path_is_made_only_from_a_path_on_this_site() {
        assert_eq!(
            SafePath::parse("/dashboard/connect").map(|p| p.as_str().to_string()),
            Some("/dashboard/connect".to_string())
        );
        assert!(SafePath::parse("//evil.example.com").is_none());
        assert!(SafePath::parse("https://evil.example.com").is_none());
    }

    // The load-bearing open-redirect proof (WBS 1.4.1): every one of these
    // must be *rejected* - if any were accepted, `login_submit` would follow
    // an attacker-controlled `next` value after a real, successful login.
    #[test]
    fn a_protocol_relative_next_is_rejected() {
        assert!(!is_safe_redirect_path("//evil.example.com"));
        assert!(!is_safe_redirect_path("//evil.example.com/path"));
    }

    #[test]
    fn an_absolute_url_next_is_rejected() {
        assert!(!is_safe_redirect_path("https://evil.example.com"));
        assert!(!is_safe_redirect_path("http://evil.example.com/dashboard"));
    }

    #[test]
    fn a_backslash_smuggled_next_is_rejected() {
        // Some browsers normalize a leading backslash to a forward slash
        // while parsing a URL, so this can behave identically to
        // `//evil.example.com` even though it doesn't start with two
        // literal `/` characters.
        assert!(!is_safe_redirect_path("/\\evil.example.com"));
    }

    #[test]
    fn a_next_with_no_leading_slash_is_rejected() {
        assert!(!is_safe_redirect_path("evil.example.com"));
        assert!(!is_safe_redirect_path("dashboard/connect"));
    }

    #[test]
    fn a_javascript_scheme_next_is_rejected() {
        assert!(!is_safe_redirect_path("/javascript:alert(1)"));
    }

    #[test]
    fn a_genuine_relative_path_is_accepted() {
        assert!(is_safe_redirect_path("/connect/woocommerce"));
        assert!(is_safe_redirect_path(
            "/connect/woocommerce?site_url=https%3A%2F%2Fshop.example.com&nonce=abc"
        ));
        assert!(is_safe_redirect_path("/dashboard/connect"));
    }

    /// A merchant picks Light, Dark or System from any page's theme toggle
    /// and lands back on that page; the choice is kept on their account, so
    /// every later page (and the POS) renders in it without JavaScript. A
    /// crafted off-site `next` falls back to the dashboard.
    #[tokio::test]
    async fn the_theme_a_merchant_picks_is_kept_and_they_return_to_their_page() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let engine =
            engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
        let state = crate::http::AppState {
            engine: crate::http::Engine::new(
                crate::engine_client::EngineClient::embedded_for_tests(engine.router()),
            ),
            ..crate::http::AppState::for_tests()
        };
        let router = crate::http::build_router(state);
        let json = |uri: &str, body: serde_json::Value| {
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        let credentials = serde_json::json!({ "email": "theme@example.com", "password": "correct horse battery staple" });
        assert_eq!(
            router
                .clone()
                .oneshot(json("/signup", credentials.clone()))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );
        let login = router
            .clone()
            .oneshot(json("/login", credentials))
            .await
            .unwrap();
        let body: serde_json::Value =
            serde_json::from_slice(&login.into_body().collect().await.unwrap().to_bytes()).unwrap();
        let session = body["session_token"].as_str().unwrap().to_string();

        let pick = |theme: &str, next: &str| {
            Request::builder()
                .method("POST")
                .uri("/dashboard/theme")
                .header("authorization", format!("Bearer {session}"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "theme={theme}&next={}",
                    next.replace('/', "%2F").replace(':', "%3A")
                )))
                .unwrap()
        };
        let page_theme = |router: axum::Router| {
            let session = session.clone();
            async move {
                let response = router
                    .oneshot(
                        Request::builder()
                            .uri("/dashboard")
                            .header("authorization", format!("Bearer {session}"))
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let html = String::from_utf8(
                    response
                        .into_body()
                        .collect()
                        .await
                        .unwrap()
                        .to_bytes()
                        .to_vec(),
                )
                .unwrap();
                html.split("<html")
                    .nth(1)
                    .unwrap()
                    .split('>')
                    .next()
                    .unwrap()
                    .to_string()
            }
        };

        for (theme, attribute) in [
            ("dark", Some("data-theme=\"dark\"")),
            ("light", Some("data-theme=\"light\"")),
            ("system", None),
        ] {
            let response = router
                .clone()
                .oneshot(pick(theme, "/dashboard/stores"))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FOUND);
            assert_eq!(
                response.headers()["location"],
                "/dashboard/stores",
                "{theme}: back to the page it was picked on"
            );
            let html_tag = page_theme(router.clone()).await;
            match attribute {
                Some(attribute) => assert!(html_tag.contains(attribute), "{theme}: {html_tag}"),
                None => assert!(
                    !html_tag.contains("data-theme"),
                    "system follows the device: {html_tag}"
                ),
            }
        }
        let response = router
            .clone()
            .oneshot(pick("dark", "https://evil.example/phish"))
            .await
            .unwrap();
        assert_eq!(response.headers()["location"], "/dashboard");
    }

    #[tokio::test]
    async fn times_follow_the_zone_a_merchant_picks_or_else_their_browsers() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let router = crate::http::build_router(crate::http::AppState::for_tests());
        let json = |uri: &str, body: serde_json::Value| {
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        let credentials = serde_json::json!({ "email": "zone@example.com", "password": "correct horse battery staple" });
        assert_eq!(
            router
                .clone()
                .oneshot(json("/signup", credentials.clone()))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );
        let login = router
            .clone()
            .oneshot(json("/login", credentials))
            .await
            .unwrap();
        let body: serde_json::Value =
            serde_json::from_slice(&login.into_body().collect().await.unwrap().to_bytes()).unwrap();
        let session = body["session_token"].as_str().unwrap().to_string();

        let pick = |zone: &str| {
            Request::builder()
                .method("POST")
                .uri("/dashboard/timezone")
                .header("authorization", format!("Bearer {session}"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("timezone={}", zone.replace('/', "%2F"))))
                .unwrap()
        };
        let dashboard = |cookie: Option<&str>| {
            let mut request = Request::builder()
                .uri("/dashboard")
                .header("authorization", format!("Bearer {session}"));
            if let Some(cookie) = cookie {
                request = request.header("cookie", cookie.to_string());
            }
            let router = router.clone();
            async move {
                let response = router
                    .oneshot(request.body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                String::from_utf8(
                    response
                        .into_body()
                        .collect()
                        .await
                        .unwrap()
                        .to_bytes()
                        .to_vec(),
                )
                .unwrap()
            }
        };

        // Automatic: the browser's zone, else UTC.
        assert!(dashboard(None).await.contains(">tz: utc</a>"));
        let html = dashboard(Some("tz=Australia%2FPerth")).await;
        assert!(
            html.contains(r##"<a href="/dashboard#timezone" class="nav-tz-link""##)
                && html.contains(">tz: perth</a>"),
            "{html}"
        );
        assert!(
            html.contains("Automatic (this browser: Australia/Perth)"),
            "{html}"
        );

        // A picked zone wins over the browser's; one that doesn't exist is ignored.
        let response = router
            .clone()
            .oneshot(pick("America/New_York"))
            .await
            .unwrap();
        assert_eq!(
            (response.status(), &response.headers()["location"]),
            (StatusCode::FOUND, &"/dashboard#timezone".parse().unwrap())
        );
        let html = dashboard(Some("tz=Australia%2FPerth")).await;
        assert!(
            html.contains(">tz: new york</a>")
                && html.contains(r#"<option value="America/New_York" selected>"#),
            "{html}"
        );
        router.clone().oneshot(pick("Not/AZone")).await.unwrap();
        assert!(dashboard(None).await.contains(">tz: new york</a>"));
        router.clone().oneshot(pick("")).await.unwrap();
        assert!(dashboard(Some("tz=Asia%2FTokyo"))
            .await
            .contains(">tz: tokyo</a>"));
    }
}
