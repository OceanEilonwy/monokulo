//! The Account page as a merchant uses it: in-process, through the router.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use tower::ServiceExt;

use crate::http::test_support::{
    body_text, form_post, signed_up_and_logged_in_session_token, urlencoding_encode,
};
use crate::http::{build_router, AppState};

const PASSWORD: &str = "correct horse battery staple";

struct Merchant {
    router: Router,
    session: String,
}

impl Merchant {
    async fn new(email: &str) -> Self {
        let router = build_router(AppState::for_tests());
        let session = signed_up_and_logged_in_session_token(&router, email, PASSWORD).await;
        Merchant { router, session }
    }

    async fn get_with(&self, uri: &str, cookie: Option<&str>) -> (StatusCode, String) {
        let mut request = Request::builder()
            .uri(uri)
            .header("cookie", format!("session={}", self.session));
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        let response = self
            .router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        (response.status(), body_text(response).await)
    }

    async fn get(&self, uri: &str) -> (StatusCode, String) {
        self.get_with(uri, None).await
    }

    /// Posts `fields` as a form; the status, and the redirect's location or
    /// else the page.
    async fn post(&self, uri: &str, fields: &[(&str, &str)]) -> (StatusCode, String) {
        self.post_as(&self.session, uri, fields).await
    }

    async fn post_as(
        &self,
        session: &str,
        uri: &str,
        fields: &[(&str, &str)],
    ) -> (StatusCode, String) {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{k}={}", urlencoding_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("cookie", format!("session={session}"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        match response.headers().get("location") {
            Some(location) => (status, location.to_str().unwrap().to_string()),
            None => (status, body_text(response).await),
        }
    }

    /// Logs in through the login form: the new session's token, if the
    /// credentials are right.
    async fn log_in(&self, email: &str, password: &str) -> Option<String> {
        let response = self
            .router
            .clone()
            .oneshot(form_post(
                "/login",
                None,
                &[("email", email), ("password", password)],
            ))
            .await
            .unwrap();
        response
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(|v| v.split(';').next()?.strip_prefix("session="))
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
    }

    /// Whether `session` is still signed in.
    async fn signed_in(&self, session: &str) -> bool {
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/account")
                    .header("cookie", format!("session={session}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        response.status() == StatusCode::OK
    }
}

/// `<html ...>`'s attributes.
fn html_tag(html: &str) -> &str {
    html.split("<html")
        .nth(1)
        .unwrap()
        .split('>')
        .next()
        .unwrap()
}

/// The profile form's fields, as the page sends them with nothing changed.
fn profile(email: &str, theme: &str, timezone: &str) -> Vec<(&'static str, String)> {
    vec![
        ("tab", "profile".to_string()),
        ("email", email.to_string()),
        ("theme", theme.to_string()),
        ("timezone", timezone.to_string()),
        ("confirm_email", String::new()),
    ]
}

fn fields<'a>(pairs: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    pairs.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

#[tokio::test]
async fn a_merchant_picks_a_theme_and_a_time_zone_and_every_page_follows() {
    let m = Merchant::new("prefs@example.com").await;
    let (status, html) = m.get("/account").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("<h1>Account</h1>"), "{html}");
    assert!(!html_tag(&html).contains("data-theme"), "{html}");

    let form = profile("prefs@example.com", "dark", "America/New_York");
    let (status, to) = m.post("/account/profile", &fields(&form)).await;
    assert_eq!(
        (status, to.as_str()),
        (StatusCode::SEE_OTHER, "/account?saved=appearance,time")
    );
    let (_, html) = m.get(&to).await;
    assert!(html_tag(&html).contains(r#"data-theme="dark""#), "{html}");
    assert!(html.contains("Times in America/New_York<"), "{html}");
    assert!(html.contains("<strong>Profile saved</strong>"), "{html}");
    assert_eq!(
        html.matches(r#"data-card-saved data-mk="4">Saved<"#)
            .count(),
        2,
        "{html}"
    );
    assert!(
        html.contains(r#"<input type="radio" name="theme" value="dark" checked>"#),
        "{html}"
    );
    assert!(
        html.contains(r#"<option value="America/New_York" selected>"#),
        "{html}"
    );

    // Saving again with nothing changed says so.
    let (_, to) = m.post("/account/profile", &fields(&form)).await;
    assert_eq!(to, "/account?saved=nothing");
    assert!(m
        .get(&to)
        .await
        .1
        .contains("<strong>Nothing to save</strong>"));
}

/// Automatic follows the browser's zone (the `tz` cookie fx-glue.js sets),
/// else UTC; a zone picked wins over it; one that doesn't exist isn't
/// saved.
#[tokio::test]
async fn times_follow_the_zone_a_merchant_picks_or_else_their_browsers() {
    let m = Merchant::new("zone@example.com").await;
    let perth = Some("tz=Australia%2FPerth");
    assert!(m.get("/").await.1.contains("Times in UTC<"));
    let (_, html) = m.get_with("/account", perth).await;
    assert!(html.contains("Times in Australia/Perth<"), "{html}");
    assert!(
        html.contains("Automatic - this browser: Australia/Perth"),
        "{html}"
    );
    assert!(
        html.contains(r#"<option value="" selected data-label="Automatic""#),
        "{html}"
    );

    let picked = profile("zone@example.com", "system", "America/New_York");
    m.post("/account/profile", &fields(&picked)).await;
    assert!(m
        .get_with("/", perth)
        .await
        .1
        .contains("Times in America/New_York<"));

    let (status, html) = m
        .post(
            "/account/profile",
            &fields(&profile("zone@example.com", "system", "Not/AZone")),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        html.contains(r#"<section id="card-time" class="settings-card is-failed""#),
        "{html}"
    );
    assert!(html.contains("Pick a time zone from the list."), "{html}");
    assert!(m.get("/").await.1.contains("Times in America/New_York<"));

    m.post(
        "/account/profile",
        &fields(&profile("zone@example.com", "system", "")),
    )
    .await;
    assert!(m
        .get_with("/", Some("tz=Asia%2FTokyo"))
        .await
        .1
        .contains("Times in Asia/Tokyo<"));
}

/// A new email is saved only once it's typed again: without JavaScript the
/// save answers with a page asking for it (the dialog does it with). Once
/// saved, the merchant logs in with it, not the old one.
#[tokio::test]
async fn an_email_change_is_confirmed_by_typing_it_again_then_used_to_log_in() {
    let m = Merchant::new("old@example.com").await;
    let mut form = profile("New@Example.com ", "system", "");
    let (status, html) = m.post("/account/profile", &fields(&form)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("<h1>Log in with a new email?</h1>"), "{html}");
    assert!(
        html.contains("From now on you log in with <strong>new@example.com</strong> instead of old@example.com."),
        "{html}"
    );
    assert!(
        m.log_in("old@example.com", PASSWORD).await.is_some(),
        "not saved yet"
    );

    form[4].1 = "new@example.org".to_string();
    let (status, html) = m.post("/account/profile", &fields(&form)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(html.contains("That isn't the same email."), "{html}");

    form[4].1 = "NEW@example.com".to_string();
    let (status, to) = m.post("/account/profile", &fields(&form)).await;
    assert_eq!(
        (status, to.as_str()),
        (StatusCode::SEE_OTHER, "/account?saved=email")
    );
    let (_, html) = m.get(&to).await;
    assert!(
        html.contains("You log in with new@example.com from now on."),
        "{html}"
    );
    assert!(
        html.contains("<strong>new@example.com</strong><small>Merchant</small>"),
        "{html}"
    );
    assert!(m.log_in("old@example.com", PASSWORD).await.is_none());
    assert!(m.log_in("new@example.com", PASSWORD).await.is_some());
}

#[tokio::test]
async fn an_email_another_account_has_or_one_that_isnt_an_address_is_refused() {
    let m = Merchant::new("first@example.com").await;
    signed_up_and_logged_in_session_token(&m.router, "second@example.com", PASSWORD).await;
    let mut form = profile("second@example.com", "dark", "");
    form[4].1 = "second@example.com".to_string();
    let (status, html) = m.post("/account/profile", &fields(&form)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        html.contains("That email is already used by another account."),
        "{html}"
    );
    // What was sent stays in the form, against what is saved.
    assert!(
        html.contains(r#"value="second@example.com" autocomplete="email" required data-email-input="" data-saved="first@example.com">"#),
        "{html}"
    );
    assert!(
        html.contains(r#"<input type="radio" name="theme" value="dark" checked data-saved="off">"#),
        "{html}"
    );
    assert!(
        !html_tag(&html).contains("data-theme"),
        "nothing was saved, the theme included: {html}"
    );

    let (status, html) = m
        .post(
            "/account/profile",
            &fields(&profile("not an address", "system", "")),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(html.contains("That is not an email address."), "{html}");
}

/// Changing the password, on the Profile tab, needs the current one and
/// the new one typed twice, and logs out every other session: someone else
/// signed in as the merchant is out, the merchant stays in.
#[tokio::test]
async fn a_password_change_needs_the_current_one_typed_twice_and_logs_out_every_other_session() {
    let m = Merchant::new("pw@example.com").await;
    let elsewhere = m.log_in("pw@example.com", PASSWORD).await.unwrap();
    let change = |current: &str, new: &str, again: &str| {
        let mut form = profile("pw@example.com", "system", "");
        form.extend([
            ("current_password", current.to_string()),
            ("new_password", new.to_string()),
            ("confirm_password", again.to_string()),
        ]);
        form
    };
    let new = "a brand new passphrase";

    let (status, html) = m
        .post(
            "/account/profile",
            &fields(&change("wrong password!", new, new)),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(html.contains("That isn't your current password."), "{html}");
    assert!(
        html.contains(r#"<section id="card-password" class="settings-card is-failed""#),
        "{html}"
    );
    assert!(
        !html.contains("wrong password!"),
        "a password is never sent back: {html}"
    );

    let (status, html) = m
        .post(
            "/account/profile",
            &fields(&change(PASSWORD, "short", "short")),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(html.contains("at least 8 characters"), "{html}");

    let (status, html) = m
        .post(
            "/account/profile",
            &fields(&change(PASSWORD, new, "a brand new passphrasf")),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        html.contains("The new password and its confirmation don't match."),
        "{html}"
    );
    assert!(m.signed_in(&elsewhere).await, "nothing changed yet");

    let (status, to) = m
        .post("/account/profile", &fields(&change(PASSWORD, new, new)))
        .await;
    assert_eq!(
        (status, to.as_str()),
        (StatusCode::SEE_OTHER, "/account?saved=password")
    );
    assert!(m
        .get(&to)
        .await
        .1
        .contains("<strong>Password changed</strong>"));
    assert!(m.signed_in(&m.session).await, "the merchant stays in");
    assert!(!m.signed_in(&elsewhere).await, "everyone else is out");
    assert!(m.log_in("pw@example.com", PASSWORD).await.is_none());
    assert!(m.log_in("pw@example.com", new).await.is_some());

    // Left empty, the password cards' fields change nothing.
    let (_, to) = m
        .post("/account/profile", &fields(&change("", "", "")))
        .await;
    assert_eq!(to, "/account?saved=nothing");
}

/// A theme and a password saved in one go: both change, and the password's
/// change still logs everyone else out.
#[tokio::test]
async fn a_password_saves_with_the_rest_of_the_profile() {
    let m = Merchant::new("both@example.com").await;
    let elsewhere = m.log_in("both@example.com", PASSWORD).await.unwrap();
    let new = "another fresh passphrase";
    let mut form = profile("both@example.com", "dark", "");
    form.extend([
        ("current_password", PASSWORD.to_string()),
        ("new_password", new.to_string()),
        ("confirm_password", new.to_string()),
    ]);
    let (status, to) = m.post("/account/profile", &fields(&form)).await;
    assert_eq!(
        (status, to.as_str()),
        (StatusCode::SEE_OTHER, "/account?saved=appearance,password")
    );
    let (_, html) = m.get(&to).await;
    assert!(html_tag(&html).contains(r#"data-theme="dark""#), "{html}");
    assert!(html.contains("<strong>Profile saved</strong>"), "{html}");
    assert!(!m.signed_in(&elsewhere).await);
    assert!(m.log_in("both@example.com", new).await.is_some());
}

/// Without JavaScript a new email asks again on a page of its own, which
/// can't carry a password through: one sent with it is refused, and
/// nothing changes.
#[tokio::test]
async fn a_new_email_without_its_confirmation_holds_back_a_password_change() {
    let m = Merchant::new("order@example.com").await;
    let new = "yet another passphrase";
    let mut form = profile("renamed@example.com", "system", "");
    form.extend([
        ("current_password", PASSWORD.to_string()),
        ("new_password", new.to_string()),
        ("confirm_password", new.to_string()),
    ]);
    let (status, html) = m.post("/account/profile", &fields(&form)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        html.contains("Change the email first, then the password."),
        "{html}"
    );
    assert!(m.log_in("order@example.com", PASSWORD).await.is_some());
}

#[tokio::test]
async fn signed_out_the_account_page_sends_a_browser_to_log_in() {
    let router = build_router(AppState::for_tests());
    let response = router
        .oneshot(
            Request::builder()
                .uri("/account?tab=wallets")
                .header("accept", "text/html")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(
        response.headers()["location"],
        "/login?next=%2Faccount%3Ftab%3Dwallets"
    );
}

/// The theme is chosen at sign-up too (System unless another is picked),
/// and the new account has it from its first page.
#[tokio::test]
async fn the_theme_picked_at_sign_up_is_the_new_accounts() {
    let router = build_router(AppState::for_tests());
    let form = body_text(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/signup")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert!(
        form.contains(r#"<input type="radio" name="theme" value="system" checked>"#),
        "{form}"
    );
    assert!(
        form.contains("You can change it any time from the account menu."),
        "{form}"
    );

    let sign_up = |email: &str, theme: &str| {
        Request::builder()
            .method("POST")
            .uri("/signup")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(format!(
                "email={}&password={}&theme={theme}",
                urlencoding_encode(email),
                urlencoding_encode(PASSWORD)
            )))
            .unwrap()
    };
    for (email, theme, attribute) in [
        ("dark@example.com", "dark", Some(r#"data-theme="dark""#)),
        ("light@example.com", "light", Some(r#"data-theme="light""#)),
        ("system@example.com", "system", None),
    ] {
        let response = router.clone().oneshot(sign_up(email, theme)).await.unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let page = body_text(
            router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/account")
                        .header("cookie", cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        match attribute {
            Some(attribute) => assert!(html_tag(&page).contains(attribute), "{theme}: {page}"),
            None => assert!(!html_tag(&page).contains("data-theme"), "{page}"),
        }
    }

    // A refused sign-up keeps the theme picked.
    let refused = body_text(
        router
            .oneshot(sign_up("not an address", "dark"))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        refused.contains(r#"<input type="radio" name="theme" value="dark" checked>"#),
        "{refused}"
    );
}
