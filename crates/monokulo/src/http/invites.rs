//! Two halves of the same feature, one module:
//!
//! - `GET`/`POST /request-invite` - the public "let me in" form
//!   (unauthenticated by necessity, same as `/signup`) an invite-only
//!   instance's landing page links to instead of a plain sign-up button
//!   (`http::home::landing`).
//! - `GET /dashboard/admin/invites` + its three `POST` actions - the admin
//!   page (`AuthedAdmin`-gated) that reviews those requests, generates
//!   single-use invite links, and dismisses/bulk-clears requests.
//!
//! **Why one module, not two.** Both halves work with the exact same two
//! tables (`invite_requests`/`invite_links`) and the same
//! encrypt-for-later-redisplay/`mailto:` concerns - see `invite_links`'s own
//! migration comment for the full reasoning on why a request-linked token
//! is stored reversibly at all. Splitting the public and admin handlers into
//! separate files would just mean two modules both reaching into the same
//! narrow slice of `Db`/`crypto`, with no real seam between them.

use axum::extract::{Form, Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::db::{InviteRequestRow, UserRow};
use crate::now_unix;
use crate::views;
use crate::views::admin::{AdminInviteRequestRow, AdminInvitesViewModel, RequestInviteViewModel};

use super::dashboard::redirect_302;
use super::fx::FxRequest;
use super::{AppState, AuthedAdmin};

const PAGE_SIZE: i64 = 10;

#[derive(Deserialize)]
pub struct RequestInviteForm {
    pub email: String,
    pub message: String,
}

async fn render_request_invite(state: &AppState, error: Option<&str>, submitted: bool) -> Response {
    let chrome = super::page_chrome(state, None, "/request-invite").await;
    let data = RequestInviteViewModel {
        error: error.map(str::to_string),
        submitted,
    };
    views::admin::request_invite_page(&chrome, &data).into_response()
}

/// `GET /request-invite`.
pub async fn request_invite_form(State(state): State<AppState>) -> Response {
    render_request_invite(&state, None, false).await
}

/// `POST /request-invite` - records the request and, in the same call,
/// creates its own matching single-use invite link (encrypted at rest - see
/// `invite_links`'s own migration comment) so the admin invites page can
/// build a real `mailto:` link for it on its very first render, with no
/// separate "generate" step. Does *not* check `signup.mode` at all - this
/// form is only ever linked to from the landing page when the mode is
/// already `"invite_only"` (`http::home::landing`), but nothing about the
/// request-invite flow itself is unsafe to leave reachable in `"public"`
/// mode too (worst case, an admin gets a request nobody needed to send).
pub async fn request_invite_submit(
    State(state): State<AppState>,
    Form(form): Form<RequestInviteForm>,
) -> Response {
    let email = form.email.trim();
    let message = form.message.trim();
    if email.is_empty() || message.is_empty() {
        return render_request_invite(
            &state,
            Some("Please fill in both your email and a short message."),
            false,
        )
        .await;
    }
    // Anyone may post this form: what it stores (and the admin page later
    // decrypts and renders) is bounded, and an address with a request
    // still waiting doesn't get a second one, each with a live invite.
    if super::signup::normalize_email(email).is_none() {
        return render_request_invite(&state, Some("That is not an email address."), false).await;
    }
    if message.chars().count() > MAX_REQUEST_MESSAGE_CHARS {
        return render_request_invite(
            &state,
            Some("Please keep your message under 2000 characters."),
            false,
        )
        .await;
    }

    let request_id = uuid::Uuid::new_v4().to_string();
    let now = now_unix();
    let raw_token = shared::auth::generate_invite_token();
    let token_hash = raw_token.hash();
    let token_encrypted = crate::crypto::encrypt(
        &state.encryption_key,
        crate::crypto::Binding::InviteToken(&request_id),
        raw_token.expose(),
    );
    let link_id = uuid::Uuid::new_v4().to_string();
    let (email, message) = (email.to_string(), message.to_string());
    let created = state
        .db
        .write(move |db| {
            if db.has_pending_invite_request(&email)? {
                // Already asked and not yet answered: nothing more to add,
                // and the same confirmation, so the form tells a visitor
                // nothing about who has asked before.
                return Ok::<_, crate::db::DbError>(());
            }
            db.create_invite_request(&request_id, &email, &message, now)?;
            // A failure here leaves a request with no linked invite - not
            // ideal, but the admin invites page tolerates it gracefully (no
            // `mailto:` link shown for that row -
            // `InviteRequestRow::invite_token_encrypted`'s own doc comment)
            // rather than losing the request itself, which is the one thing
            // this handler must not silently drop.
            db.create_invite_link(
                &link_id,
                &token_hash,
                Some(&token_encrypted),
                Some(&request_id),
                now,
            )
            .ok();
            Ok::<_, crate::db::DbError>(())
        })
        .await;
    if created.is_err() {
        return render_request_invite(
            &state,
            Some("Something went wrong. Please try again."),
            false,
        )
        .await;
    }

    render_request_invite(&state, None, true).await
}

/// Most characters an invite request's message may have.
pub const MAX_REQUEST_MESSAGE_CHARS: usize = 2000;

/// Minimal, correct percent-encoding for a `mailto:` URI's `subject`/`body`
/// query components (RFC 6068) - deliberately *not* `url::form_urlencoded`
/// (`http::connect::encode_query_value`'s own helper), which encodes a
/// space as `+`. That's the right encoding for
/// `application/x-www-form-urlencoded` bodies, but `+` has no special
/// meaning in a `mailto:` URI - a mail client would show a literal `+`
/// instead of a space, so this needs its own encoder using `%20`.
fn mailto_percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn build_mailto_href(email: &str, raw_invite_link: &str) -> String {
    format!(
        "mailto:{}?subject={}&body={}",
        mailto_percent_encode(email),
        mailto_percent_encode("You're invited to Monokulo"),
        mailto_percent_encode(&format!(
            "Hi,\n\nHere's your invite link - it works once:\n{raw_invite_link}\n\nSee you soon!"
        )),
    )
}

/// Same "reconstruct the real external origin from what the browser itself
/// used to reach us, falling back to plain `http` for local/dev use" scheme
/// `http::orders::render_order_detail_page`'s own `payment_link` uses - see
/// that call site's doc comment for the reasoning (a reverse proxy in front
/// of real TLS termination communicates it via `X-Forwarded-Proto`).
fn base_url(headers: &HeaderMap) -> String {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    format!("{scheme}://{host}")
}

/// The full, absolute signup URL a raw invite token resolves to -
/// `GET /dashboard/signup?invite=<token>` carries it straight into the
/// form's hidden field (`http::dashboard::signup_form`).
fn invite_signup_url(base_url: &str, raw_token: &str) -> String {
    format!("{base_url}/dashboard/signup?invite={raw_token}")
}

fn to_row_view(
    encryption_key: &crate::crypto::AtRestKey,
    base_url: &str,
    clock: &crate::views::time::Clock,
    row: InviteRequestRow,
    just_deleted: bool,
) -> AdminInviteRequestRow {
    let mailto_href = row
        .invite_token_encrypted
        .as_deref()
        .and_then(|enc| {
            crate::crypto::decrypt(
                encryption_key,
                crate::crypto::Binding::InviteToken(&row.id),
                enc,
            )
            .ok()
        })
        .map(|raw_token| build_mailto_href(&row.email, &invite_signup_url(base_url, &raw_token)));
    AdminInviteRequestRow {
        id: row.id,
        email: row.email,
        message: row.message,
        created_at_display: clock.time(row.created_at),
        mailto_href,
        just_deleted,
    }
}

/// `requested.clamp(1, total_pages)` - the admin invites page's own
/// self-correcting pagination: deleting the last row on the last page (or
/// any other cause of a page going stale, e.g. another admin clearing
/// requests concurrently) lands the next load on the last page that still
/// exists, rather than a blank one. `total_pages` is always at least 1 even
/// with zero rows, so this never divides by (or clamps into) zero.
fn clamp_page(total_pages: u32, requested: u32) -> u32 {
    requested.max(1).min(total_pages)
}

#[derive(Deserialize)]
pub struct InvitesPageQuery {
    #[serde(default)]
    pub page: Option<u32>,
    /// Set only by this module's own `POST` redirects, right after a real
    /// delete - the id of the row to render struck-through-once, outside
    /// the page's own real pagination count. See
    /// `AdminInviteRequestRow::just_deleted`'s own doc comment.
    #[serde(default)]
    pub deleted: Option<String>,
    /// Set only by `delete_all_invite_requests`'s own redirect - how many
    /// requests it actually cleared, for a "N requests deleted." banner.
    #[serde(default)]
    pub cleared: Option<usize>,
}

#[allow(clippy::too_many_arguments)]
async fn render_invites_page(
    state: &AppState,
    admin_user: &UserRow,
    headers: &HeaderMap,
    requested_page: u32,
    deleted_id: Option<&str>,
    created_link: Option<String>,
    error: Option<String>,
    success: Option<String>,
    fx: FxRequest,
) -> Response {
    let base = base_url(headers);
    let clock = crate::views::time::Clock::for_user(admin_user);
    let deleted_id = deleted_id.map(str::to_string);
    let (total_pages, page, rows, just_deleted) = state
        .db
        .read(move |db| {
            let total = db.count_unactioned_invite_requests().unwrap_or(0).max(0) as u32;
            let total_pages = total.div_ceil(PAGE_SIZE as u32).max(1);
            let page = clamp_page(total_pages, requested_page);
            let offset = (page as i64 - 1) * PAGE_SIZE;
            let rows = db
                .list_unactioned_invite_requests(PAGE_SIZE, offset)
                .unwrap_or_default();
            let just_deleted = deleted_id.and_then(|id| db.get_invite_request(&id).ok().flatten());
            Ok::<_, crate::db::DbError>((total_pages, page, rows, just_deleted))
        })
        .await
        .unwrap_or((1, 1, Vec::new(), None));
    let row_views = rows
        .into_iter()
        .map(|r| to_row_view(&state.encryption_key, &base, &clock, r, false))
        .collect();

    let just_deleted_row =
        just_deleted.map(|r| to_row_view(&state.encryption_key, &base, &clock, r, true));

    let view = AdminInvitesViewModel {
        error,
        success,
        just_deleted_row,
        rows: row_views,
        page,
        total_pages,
        has_previous: page > 1,
        has_next: page < total_pages,
        previous_page: page.saturating_sub(1).max(1),
        next_page: (page + 1).min(total_pages),
        created_link,
    };
    if fx.0 {
        return axum::response::Html(views::admin::invites_section(&view).into_string())
            .into_response();
    }
    let chrome = super::page_chrome(state, Some(admin_user), "/dashboard/admin/invites").await;
    views::admin::admin_invites_page(&chrome, &view).into_response()
}

/// `GET /dashboard/admin/invites?page=N&deleted=<id>` (or `?cleared=N` right
/// after "delete all").
pub async fn invites_page(
    State(state): State<AppState>,
    AuthedAdmin(admin_user, _): AuthedAdmin,
    fx: FxRequest,
    headers: HeaderMap,
    Query(query): Query<InvitesPageQuery>,
) -> Response {
    let success = query.cleared.map(cleared_message);
    render_invites_page(
        &state,
        &admin_user,
        &headers,
        query.page.unwrap_or(1),
        query.deleted.as_deref(),
        None,
        None,
        success,
        fx,
    )
    .await
}

/// `POST /dashboard/admin/invites/create-link` - the standalone-link
/// button: a fresh, never-request-linked invite, shown exactly once (see
/// `AdminInvitesViewModel::created_link`'s own doc comment) and only ever
/// stored hashed.
pub async fn create_invite_link(
    State(state): State<AppState>,
    AuthedAdmin(admin_user, _): AuthedAdmin,
    fx: FxRequest,
    headers: HeaderMap,
) -> Response {
    let raw_token = shared::auth::generate_invite_token();
    let token_hash = raw_token.hash();
    let link_id = uuid::Uuid::new_v4().to_string();
    let created = state
        .db
        .write(move |db| db.create_invite_link(&link_id, &token_hash, None, None, now_unix()))
        .await;
    if created.is_err() {
        return render_invites_page(
            &state,
            &admin_user,
            &headers,
            1,
            None,
            None,
            Some("Something went wrong creating the link. Please try again.".to_string()),
            None,
            fx,
        )
        .await;
    }
    render_invites_page(
        &state,
        &admin_user,
        &headers,
        1,
        None,
        Some(invite_signup_url(&base_url(&headers), raw_token.expose())),
        None,
        None,
        fx,
    )
    .await
}

/// `POST /dashboard/admin/invites/{id}/delete?page=N` - see this module's
/// own doc comment and `Db::delete_invite_request`'s for what "delete" means
/// here (a soft-delete plus revoking the request's own unused link, not a
/// row delete). Redirects back to the *same* page it was submitted from
/// (`page`, carried through the form's own action URL), carrying `?deleted=<id>`
/// so the reload can show the one-time struck-through confirmation.
pub async fn delete_invite_request(
    State(state): State<AppState>,
    AuthedAdmin(admin_user, _): AuthedAdmin,
    fx: FxRequest,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<InvitesPageQuery>,
) -> Response {
    let deleted = id.clone();
    let page = query.page.unwrap_or(1);
    let result = state
        .db
        .write(move |db| db.delete_invite_request(&deleted, now_unix()))
        .await;
    // A delete that didn't happen is said so, not shown struck through.
    if let Err(e) = result {
        tracing::error!(error = %e, "failed to delete an invite request");
        return render_invites_page(
            &state,
            &admin_user,
            &headers,
            page,
            None,
            None,
            Some("Something went wrong deleting that request. Please try again.".to_string()),
            None,
            fx,
        )
        .await;
    }
    if fx.0 {
        return render_invites_page(
            &state,
            &admin_user,
            &headers,
            page,
            Some(&id),
            None,
            None,
            None,
            fx,
        )
        .await;
    }
    redirect_302(&format!(
        "/dashboard/admin/invites?page={page}&deleted={id}"
    ))
}

/// `POST /dashboard/admin/invites/delete-all` - clears every currently
/// unactioned request (across every page, not just the one being viewed -
/// `Db::delete_all_unactioned_invite_requests` operates on the whole
/// table), then redirects to page 1 with a count-bearing confirmation
/// banner. No single-row struck-through treatment here - see this
/// feature's own design discussion on why that doesn't make sense once
/// more than one row is involved.
pub async fn delete_all_invite_requests(
    State(state): State<AppState>,
    AuthedAdmin(admin_user, _): AuthedAdmin,
    fx: FxRequest,
    headers: HeaderMap,
) -> Response {
    let cleared = state
        .db
        .write(|db| db.delete_all_unactioned_invite_requests(now_unix()))
        .await
        .unwrap_or(0);
    if fx.0 {
        return render_invites_page(
            &state,
            &admin_user,
            &headers,
            1,
            None,
            None,
            None,
            Some(cleared_message(cleared)),
            fx,
        )
        .await;
    }
    redirect_302(&format!("/dashboard/admin/invites?cleared={cleared}"))
}

fn cleared_message(n: usize) -> String {
    format!("{n} request{} deleted.", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use tower::ServiceExt;

    use crate::db::{TEST_ADMIN_EMAIL, TEST_ADMIN_PASSWORD};
    use crate::http::{build_router, AppState};

    fn test_state() -> AppState {
        AppState::for_tests()
    }

    use crate::http::test_support::body_text;

    use crate::http::test_support::urlencoding_encode;

    fn form_request(method: &str, uri: &str, fields: &[(&str, &str)]) -> Request<Body> {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding_encode(k), urlencoding_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap()
    }

    fn authed_form_request(
        method: &str,
        uri: &str,
        cookie: &str,
        fields: &[(&str, &str)],
    ) -> Request<Body> {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding_encode(k), urlencoding_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("cookie", cookie)
            .body(Body::from(body))
            .unwrap()
    }

    async fn admin_session_cookie(router: &Router) -> String {
        let response = router
            .clone()
            .oneshot(form_request(
                "POST",
                "/dashboard/login",
                &[
                    ("email", TEST_ADMIN_EMAIL),
                    ("password", TEST_ADMIN_PASSWORD),
                ],
            ))
            .await
            .unwrap();
        let set_cookie = response
            .headers()
            .get("set-cookie")
            .expect("expected a session cookie from a correct admin login")
            .to_str()
            .unwrap();
        set_cookie.split(';').next().unwrap().to_string()
    }

    async fn signed_up_session_cookie(
        router: &Router,
        email: &str,
        password: &str,
        extra_fields: &[(&str, &str)],
    ) -> axum::response::Response {
        let mut fields = vec![("email", email), ("password", password)];
        fields.extend_from_slice(extra_fields);
        router
            .clone()
            .oneshot(form_request("POST", "/dashboard/signup", &fields))
            .await
            .unwrap()
    }

    async fn admin_invites_get(router: &Router, cookie: &str) -> axum::response::Response {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/dashboard/admin/invites")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// Extracts the raw invite token from a page containing a
    /// `/dashboard/signup?invite=<token>` link (either the "create invite
    /// link" confirmation, or a row's own `mailto:` body) - a small,
    /// deliberately narrow scraper for test purposes only. Handbars
    /// HTML-escapes `=` as `&#x3D;` (see `AdminInviteRequestRow::mailto_href`'s
    /// own template usage), so this looks for either form.
    fn extract_invite_token(html: &str) -> String {
        let marker = html
            .find("invite=")
            .map(|i| i + "invite=".len())
            .or_else(|| html.find("invite&#x3D;").map(|i| i + "invite&#x3D;".len()));
        let start = marker.expect("expected an invite link in the page");
        let rest = &html[start..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        rest[..end].to_string()
    }

    /// The real row delete form's own action URL is
    /// `/dashboard/admin/invites/<uuid>/delete` - distinct from the fixed
    /// `create-link`/`delete-all` action paths this same marker prefix also
    /// matches, so this skips any match that isn't followed by `/delete`
    /// starting right after a real id.
    fn extract_first_row_delete_id(html: &str) -> String {
        let marker = "/dashboard/admin/invites/";
        let mut search_from = 0;
        loop {
            let found = html[search_from..]
                .find(marker)
                .expect("expected a row delete form in the page")
                + search_from;
            let start = found + marker.len();
            let rest = &html[start..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                .unwrap_or(rest.len());
            let candidate = &rest[..end];
            if rest[end..].starts_with("/delete")
                && candidate != "create-link"
                && candidate != "delete-all"
            {
                return candidate.to_string();
            }
            search_from = start;
        }
    }

    #[tokio::test]
    async fn the_request_invite_form_is_reachable_without_a_session() {
        let router = build_router(test_state());
        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/request-invite")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains(r#"<form method="post" action="/request-invite">"#));
    }

    #[tokio::test]
    async fn submitting_a_request_invite_form_saves_it_and_an_admin_can_see_it_with_a_real_mailto_link(
    ) {
        let router = build_router(test_state());

        let submit = router
            .clone()
            .oneshot(form_request(
                "POST",
                "/request-invite",
                &[
                    ("email", "hopeful@example.com"),
                    ("message", "let me in please"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(submit.status(), StatusCode::OK);
        let html = body_text(submit).await;
        assert!(
            html.to_lowercase().contains("thanks"),
            "expected the thank-you confirmation, got: {html}"
        );

        let cookie = admin_session_cookie(&router).await;
        let admin_view = admin_invites_get(&router, &cookie).await;
        let html = body_text(admin_view).await;
        assert!(
            html.contains("hopeful@example.com"),
            "expected the request listed, got: {html}"
        );
        assert!(html.contains("let me in please"));
        assert!(
            html.contains("mailto:hopeful%40example.com"),
            "expected a real mailto link, got: {html}"
        );
        assert!(
            html.contains("dashboard%2Fsignup%3Finvite")
                || html.contains("dashboard/signup?invite"),
            "expected the mailto body to carry a real invite link, got: {html}"
        );
    }

    #[tokio::test]
    async fn submitting_an_empty_request_invite_form_is_rejected_and_creates_nothing() {
        let router = build_router(test_state());
        let response = router
            .clone()
            .oneshot(form_request(
                "POST",
                "/request-invite",
                &[("email", ""), ("message", "")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains("Please fill in"),
            "expected a clear validation error, got: {html}"
        );

        let cookie = admin_session_cookie(&router).await;
        let admin_view = admin_invites_get(&router, &cookie).await;
        assert!(body_text(admin_view)
            .await
            .contains("No pending invite requests"));
    }

    #[tokio::test]
    async fn the_invites_page_is_unauthorized_without_a_session_and_forbidden_for_a_non_admin() {
        let router = build_router(test_state());

        let unauthed = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/dashboard/admin/invites")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthed.status(), StatusCode::UNAUTHORIZED);

        router
            .clone()
            .oneshot(form_request(
                "POST",
                "/dashboard/signup",
                &[
                    ("email", "merchant@example.com"),
                    ("password", "correct horse battery staple"),
                ],
            ))
            .await
            .unwrap();
        let login = router
            .clone()
            .oneshot(form_request(
                "POST",
                "/dashboard/login",
                &[
                    ("email", "merchant@example.com"),
                    ("password", "correct horse battery staple"),
                ],
            ))
            .await
            .unwrap();
        let cookie = login
            .headers()
            .get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();

        let forbidden = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/dashboard/admin/invites")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_standalone_invite_link_is_shown_once_and_actually_works_for_signup() {
        let state = test_state()
            .with_options("[signup]\nmode = \"public\"\n")
            .await;
        let admin_page = state.clone();
        let router = build_router(state);
        let cookie = admin_session_cookie(&router).await;

        let create = router
            .clone()
            .oneshot(authed_form_request(
                "POST",
                "/dashboard/admin/invites/create-link",
                &cookie,
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(create.status(), StatusCode::OK);
        let html = body_text(create).await;
        let token = extract_invite_token(&html);

        // Switch to invite-only mode to actually prove the token is real -
        // "public" mode (this crate's own test default) would let anyone
        // sign up regardless, which wouldn't prove anything about the token.
        admin_page.save_setting("signup.mode", "invite_only").await;

        let signup = signed_up_session_cookie(
            &router,
            "newcomer@example.com",
            "correct horse battery staple",
            &[("invite", &token)],
        )
        .await;
        assert_eq!(
            signup.status(),
            StatusCode::FOUND,
            "a valid invite token must let a real signup through"
        );
        assert_eq!(
            signup.headers().get("location").unwrap(),
            "/dashboard/login"
        );

        // The real point: it cannot be reused for a second account.
        let second = signed_up_session_cookie(
            &router,
            "another@example.com",
            "correct horse battery staple",
            &[("invite", &token)],
        )
        .await;
        assert_eq!(
            second.status(),
            StatusCode::OK,
            "a rejected signup re-renders the form, not a redirect"
        );
        let html = body_text(second).await;
        assert!(
            html.contains("already been used"),
            "expected a clear already-used error, got: {html}"
        );
    }

    #[tokio::test]
    async fn deleting_a_request_soft_deletes_it_and_shows_it_struck_through_once_then_never_again()
    {
        let router = build_router(test_state());
        let cookie = admin_session_cookie(&router).await;
        router
            .clone()
            .oneshot(form_request(
                "POST",
                "/request-invite",
                &[("email", "a@example.com"), ("message", "m")],
            ))
            .await
            .unwrap();

        let admin_view = admin_invites_get(&router, &cookie).await;
        let html = body_text(admin_view).await;
        assert!(html.contains("a@example.com"));

        let real_id = extract_first_row_delete_id(&html);

        let delete = router
            .clone()
            .oneshot(authed_form_request(
                "POST",
                &format!("/dashboard/admin/invites/{real_id}/delete?page=1"),
                &cookie,
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(delete.status(), StatusCode::FOUND);
        let location = delete
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(location.contains(&format!("deleted={real_id}")));

        let after_delete = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(&location)
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(after_delete).await;
        assert!(
            html.contains(r#"class="row-deleted""#),
            "expected the one-time struck-through row, got: {html}"
        );
        assert!(
            html.contains("a@example.com"),
            "the struck-through row itself should still show the deleted request's data"
        );

        // A later, plain reload must not still show it.
        let reload = admin_invites_get(&router, &cookie).await;
        let html = body_text(reload).await;
        assert!(
            !html.contains(r#"class="row-deleted""#),
            "the struck-through addendum must only ever show once, right after the delete"
        );
        assert!(!html.contains("a@example.com"));
    }

    #[tokio::test]
    async fn delete_all_clears_every_pending_request_and_shows_a_count_banner() {
        let router = build_router(test_state());
        let cookie = admin_session_cookie(&router).await;
        for i in 0..3 {
            router
                .clone()
                .oneshot(form_request(
                    "POST",
                    "/request-invite",
                    &[("email", &format!("user{i}@example.com")), ("message", "m")],
                ))
                .await
                .unwrap();
        }

        let delete_all = router
            .clone()
            .oneshot(authed_form_request(
                "POST",
                "/dashboard/admin/invites/delete-all",
                &cookie,
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(delete_all.status(), StatusCode::FOUND);
        let location = delete_all
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        let after = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(&location)
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(after).await;
        assert!(
            html.contains("3 requests deleted."),
            "expected a count banner, got: {html}"
        );
        assert!(html.contains("No pending invite requests"));
    }

    #[tokio::test]
    async fn pagination_shows_the_right_page_and_clamps_after_deleting_the_last_row_on_the_last_page(
    ) {
        let router = build_router(test_state());
        let cookie = admin_session_cookie(&router).await;
        // PAGE_SIZE is 10 - 11 requests makes a real second page of exactly 1 row.
        for i in 0..11 {
            router
                .clone()
                .oneshot(form_request(
                    "POST",
                    "/request-invite",
                    &[
                        ("email", &format!("user{i:02}@example.com")),
                        ("message", "m"),
                    ],
                ))
                .await
                .unwrap();
        }

        let page2 = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/dashboard/admin/invites?page=2")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(page2).await;
        assert!(
            html.contains("Page 2 of 2"),
            "expected a real second page, got: {html}"
        );

        let last_row_id = extract_first_row_delete_id(&html);

        let delete = router
            .clone()
            .oneshot(authed_form_request(
                "POST",
                &format!("/dashboard/admin/invites/{last_row_id}/delete?page=2"),
                &cookie,
                &[],
            ))
            .await
            .unwrap();
        let location = delete
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(
            location.starts_with("/dashboard/admin/invites?page=2"),
            "the redirect itself still targets the page it was deleted from"
        );

        let after = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(&location)
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(after).await;
        assert!(
            html.contains("Page 1 of 1"),
            "page 2 no longer exists once its only row is gone - must clamp back to page 1"
        );
    }

    /// With fixi, every button on the invites page answers with the page's
    /// section only (structured_logging.md part 6).
    #[tokio::test]
    async fn invites_buttons_answer_fixi_with_the_section() {
        let router = build_router(test_state());
        let cookie = admin_session_cookie(&router).await;
        let mut create =
            authed_form_request("POST", "/dashboard/admin/invites/create-link", &cookie, &[]);
        create
            .headers_mut()
            .insert("FX-Request", "true".parse().unwrap());
        let html = body_text(router.clone().oneshot(create).await.unwrap()).await;
        assert!(html.starts_with(r#"<section id="invites">"#), "{html}");
        assert!(
            html.contains("Share this link") && html.contains("/dashboard/signup?invite="),
            "{html}"
        );

        let mut clear =
            authed_form_request("POST", "/dashboard/admin/invites/delete-all", &cookie, &[]);
        clear
            .headers_mut()
            .insert("FX-Request", "true".parse().unwrap());
        let response = router.clone().oneshot(clear).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains("0 requests deleted.") && !html.contains("<html"),
            "{html}"
        );
    }
}
