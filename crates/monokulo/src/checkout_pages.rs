//! Which checkout a customer sees, and the custom page's standing.
//!
//! A custom page is a merchant's own words at this Monokulo's address, so it shows
//! only where they own the frame around it: their verified domain has approved
//! this version, by a DNS record or (for an onion shop) a file over Tor, and the
//! browser says it's a frame of that domain's site. Everywhere else the store's
//! layout shows instead, so a problem here never stops anyone paying.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use axum::http::HeaderMap;
use checkout_template::Template;
use domain_proof::{CheckOutcome, DomainState, Proofs, Scheme};
use parking_lot::Mutex;

use crate::db::{ConnectionId, Database, PublishedCheckoutPage, StoreDomainRow};

/// Approving one version of a store's custom page.
pub const TEMPLATE: Scheme = Scheme {
    label: "_monokulo-template",
    value_prefix: "monokulo-template=",
    well_known_path: "/.well-known/monokulo-template",
};

/// The domain that approves a store's custom page, and the only site it shows on:
/// its web domain if it has one that counts, otherwise its onion address. So a
/// store with both shows its layout on Tor. Whoever approves a page must be the one
/// it's shown for, or a stolen dashboard could add a domain of its own, approve a
/// page there, and show it on the shop's onion address, whose owner never saw it.
pub fn approver(domains: &[StoreDomainRow], now: i64) -> Option<&StoreDomainRow> {
    let counting = |row: &&StoreDomainRow| row.state(now).counts();
    domains
        .iter()
        .filter(counting)
        .find(|row| !domain_proof::is_onion(&row.domain))
        .or_else(|| domains.iter().find(counting))
}

/// Whether the browser says this page is a frame of another site. Not to be
/// taken on trust if it doesn't say: unlike turning someone away, the safe answer
/// to not knowing here is the store's ordinary layout. Monokulo's own share page
/// frames the checkout too, but from the same origin.
pub fn framed_by_another_site(headers: &HeaderMap) -> bool {
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    let is_frame = header("sec-fetch-dest").is_some_and(|dest| {
        dest.eq_ignore_ascii_case("iframe") || dest.eq_ignore_ascii_case("frame")
    });
    let another_site = header("sec-fetch-site").is_some_and(|site| {
        site.eq_ignore_ascii_case("cross-site") || site.eq_ignore_ascii_case("same-site")
    });
    is_frame && another_site
}

/// Where a request's page sits, as far as choosing a custom page goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    NotFramed,
    /// A frame of another site, with the origin its `Referer` names, if any.
    OfAnotherSite {
        referrer: Option<String>,
    },
    /// A custom page's frame moving itself on: a refresh, a form, a new version.
    /// To the browser that's same-origin, as a first load from Monokulo's own share
    /// page is too, so it's marked in the URL that only a custom page writes.
    ItsOwnNavigation,
}

/// Which [`Frame`] a request is. `marked` is the URL's `framed=true`.
pub fn frame_of(headers: &HeaderMap, marked: bool) -> Frame {
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    if framed_by_another_site(headers) {
        let referrer = header("referer")
            .and_then(|referer| url::Url::parse(referer).ok())
            .map(|referer| referer.origin().ascii_serialization());
        return Frame::OfAnotherSite { referrer };
    }
    let is_frame = header("sec-fetch-dest").is_some_and(|dest| {
        dest.eq_ignore_ascii_case("iframe") || dest.eq_ignore_ascii_case("frame")
    });
    let own = header("sec-fetch-site").is_some_and(|site| site.eq_ignore_ascii_case("same-origin"));
    if marked && is_frame && own {
        Frame::ItsOwnNavigation
    } else {
        Frame::NotFramed
    }
}

/// Where a published page stands, whatever asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageStanding {
    /// Showing, its proof last seen when checked.
    Working,
    /// Its proof has been missing since `since`: still showing, until its grace
    /// period ends.
    Failing { since: i64 },
    /// Its proof has been gone too long, its domain no longer counts, or it no
    /// longer passes the checks.
    NotShowing,
    /// This Monokulo's operator has turned it off.
    TurnedOff,
}

impl PageStanding {
    pub fn shows(&self) -> bool {
        matches!(self, PageStanding::Working | PageStanding::Failing { .. })
    }
}

/// Where `page` stands at `now`, given its store's domains.
pub fn standing(
    page: &PublishedCheckoutPage,
    domains: &[StoreDomainRow],
    now: i64,
) -> PageStanding {
    if page.turned_off_at.is_some() {
        return PageStanding::TurnedOff;
    }
    if approver(domains, now).is_none() || parsed(page).is_none() {
        return PageStanding::NotShowing;
    }
    match DomainState::of(Some(page.published_at), page.failing_since, now) {
        DomainState::Failing { since } => PageStanding::Failing { since },
        DomainState::Lapsed { .. } => PageStanding::NotShowing,
        DomainState::Pending | DomainState::Verified => PageStanding::Working,
    }
}

/// A custom page chosen for a request.
#[derive(Clone)]
pub struct ChosenPage {
    /// Which version it is, which the page's stream checks stays current.
    pub token: String,
    pub template: Arc<Template>,
    pub css_hash: Option<String>,
    /// Only the store's own verified sites may frame it, however its other
    /// layouts are set: a page in a merchant's words mustn't appear on another.
    pub frame_ancestors: Option<String>,
}

/// How many parsed templates are kept. Parsing is quick, and this is only so a
/// busy page's every view doesn't repeat it.
const PARSED_TEMPLATES: usize = 512;

/// A page's template, parsed once per version. `None` if it or its CSS no longer
/// passes today's safety checks, as a release that closes a hole must be able to
/// cause: that costs a page, not a checkout.
pub fn parsed(page: &PublishedCheckoutPage) -> Option<Arc<Template>> {
    let token = page.token.as_str();
    static PARSED: LazyLock<Mutex<HashMap<String, Arc<Template>>>> = LazyLock::new(Mutex::default);
    if let Some(template) = PARSED.lock().get(token) {
        return Some(template.clone());
    }
    let template = checkout_template::check_published(&page.template).ok()?;
    checkout_template::css::sanitize(&page.css, template.classes()).ok()?;
    let template = Arc::new(template);
    let mut parsed = PARSED.lock();
    if parsed.len() >= PARSED_TEMPLATES {
        parsed.clear();
    }
    parsed.insert(token.to_string(), template.clone());
    Some(template)
}

/// The page to show this request, if any: an explicit `?view=` always wins, and
/// the rest is its [`Frame`] and the page's [`standing`]. A frame of a site the
/// approver doesn't cover gets the layout rather than a page its browser would
/// refuse. Tor Browser sends no `Referer` from an onion shop, so with none, only an
/// onion approver is taken to be the one framing it: the policy still holds it to
/// that, and any other site asking for no `Referer` gets the layout.
pub fn chosen_page(
    page: Option<&PublishedCheckoutPage>,
    domains: &[StoreDomainRow],
    frame: &Frame,
    asked_for_a_layout: bool,
    now: i64,
) -> Option<ChosenPage> {
    if asked_for_a_layout || *frame == Frame::NotFramed {
        return None;
    }
    let page = page?;
    if !standing(page, domains, now).shows() {
        return None;
    }
    let approver = approver(domains, now)?;
    let policy = crate::embed_domains::EmbedPolicy {
        restricted: true,
        domains: vec![approver.clone()],
    };
    let here = match frame {
        Frame::NotFramed => false,
        Frame::ItsOwnNavigation => true,
        Frame::OfAnotherSite {
            referrer: Some(origin),
        } => policy.allows_origin(origin, now),
        Frame::OfAnotherSite { referrer: None } => domain_proof::is_onion(&approver.domain),
    };
    if !here {
        return None;
    }
    Some(ChosenPage {
        token: page.token.clone(),
        template: parsed(page)?,
        css_hash: (!page.written_css.is_empty())
            .then(|| crate::views::checkout_style::stylesheet_hash(&page.written_css)),
        frame_ancestors: policy.frame_ancestors(now),
    })
}

/// The store's page, if it's the version `token` and still showing: what an open
/// page's stream follows, and otherwise tells it to load again.
pub fn page_still_showing(
    db: &crate::db::Db,
    id: &ConnectionId,
    token: &str,
    now: i64,
) -> Result<Option<PublishedCheckoutPage>, crate::db::DbError> {
    let Some(page) = db
        .published_checkout_page(id)?
        .filter(|page| page.token == token)
    else {
        return Ok(None);
    };
    let domains = db.list_store_domains(id)?;
    Ok(standing(&page, &domains, now).shows().then_some(page))
}

/// What a lookup found, recorded on a page as on a verified domain: an answer
/// without the proof starts the grace period, and no answer changes nothing but
/// says so.
pub async fn check_and_record(
    db: &Database,
    proofs: &Proofs,
    id: &ConnectionId,
    domain: &str,
    token: &str,
    now: i64,
) -> Result<CheckOutcome, crate::db::DbError> {
    let outcome = domain_proof::check(proofs, &TEMPLATE, domain, token).await;
    let error = outcome.error_message(&TEMPLATE, domain, token);
    let answered = !matches!(outcome, CheckOutcome::LookupFailed(_));
    let id = id.clone();
    let token = token.to_string();
    db.write(move |db| db.record_checkout_page_check(&id, &token, now, answered, error.as_deref()))
        .await?;
    Ok(outcome)
}

/// Whether a page is due a look at its proof: daily, or hourly while something's
/// wrong, so a fix is noticed well inside the grace period.
pub fn check_due(page: &PublishedCheckoutPage, now: i64) -> bool {
    let every = if page.failing_since.is_some() || page.last_error.is_some() {
        domain_proof::FAILING_RECHECK_EVERY_SECS
    } else {
        domain_proof::RECHECK_EVERY_SECS
    };
    page.last_checked_at.unwrap_or(page.published_at) + every <= now
}

/// Looks at a page's proof in the background if it's due. A view never waits for
/// DNS or Tor, and a store never has two lookups going, whatever its traffic.
pub fn recheck_if_due(
    db: Database,
    proofs: Proofs,
    page: &PublishedCheckoutPage,
    domains: &[StoreDomainRow],
    now: i64,
) {
    static CHECKING: LazyLock<Mutex<HashSet<ConnectionId>>> = LazyLock::new(Mutex::default);
    if page.turned_off_at.is_some() || !check_due(page, now) {
        return;
    }
    let Some(domain) = approver(domains, now).map(|row| row.domain.clone()) else {
        return;
    };
    let (id, token) = (page.connection_id.clone(), page.token.clone());
    if !CHECKING.lock().insert(id.clone()) {
        return;
    }
    tokio::spawn(async move {
        let checked = check_and_record(&db, &proofs, &id, &domain, &token, now).await;
        if let Err(e) = checked {
            tracing::warn!(store.id = %id, error = %e, "could not record a custom page's check");
        }
        CHECKING.lock().remove(&id);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn domain(name: &str, verified: Option<i64>, failing: Option<i64>) -> StoreDomainRow {
        StoreDomainRow {
            id: name.to_string(),
            connection_id: ConnectionId::new("s".to_string()),
            domain: name.to_string(),
            token: "t".to_string(),
            created_at: 0,
            verified_at: verified,
            failing_since: failing,
            last_checked_at: None,
            last_error: None,
            replaces_id: None,
        }
    }

    const ONION: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
    const NOW: i64 = 10_000_000;
    const SOURCE: &str = "{{status}}{{qr}}<p>{{amount}}{{fiat}}</p>{{address}}";

    fn shop_frame() -> Frame {
        Frame::OfAnotherSite {
            referrer: Some("https://shop.example".to_string()),
        }
    }

    fn page() -> PublishedCheckoutPage {
        PublishedCheckoutPage {
            connection_id: ConnectionId::new("s".to_string()),
            template: SOURCE.to_string(),
            css: String::new(),
            written_css: String::new(),
            token: "version-one".to_string(),
            published_at: NOW - 100,
            last_checked_at: Some(NOW - 100),
            failing_since: None,
            last_error: None,
            turned_off_at: None,
            turned_off_reason: None,
        }
    }

    #[test]
    fn the_web_domain_approves_if_there_is_one_otherwise_the_onion_address() {
        let web = domain("shop.example", Some(1), None);
        let onion = domain(ONION, Some(1), None);
        let name = |rows: &[StoreDomainRow]| approver(rows, NOW).map(|row| row.domain.clone());
        assert_eq!(
            name(&[onion.clone(), web.clone()]).as_deref(),
            Some("shop.example")
        );
        assert_eq!(name(std::slice::from_ref(&onion)).as_deref(), Some(ONION));
        assert_eq!(name(&[domain("pending.example", None, None)]), None);
        // A lapsed web domain hands over to the onion address that still counts.
        let lapsed = domain(
            "shop.example",
            Some(1),
            Some(NOW - domain_proof::GRACE_SECS),
        );
        assert_eq!(name(&[lapsed.clone(), onion]).as_deref(), Some(ONION));
        assert_eq!(name(&[lapsed]), None);
    }

    /// Every way a request can come decides the same way: only a frame that
    /// says it's another site's.
    #[test]
    fn only_a_frame_of_another_site_counts() {
        let headers = |dest: Option<&str>, site: Option<&str>| {
            let mut headers = HeaderMap::new();
            if let Some(dest) = dest {
                headers.insert("sec-fetch-dest", dest.parse().unwrap());
            }
            if let Some(site) = site {
                headers.insert("sec-fetch-site", site.parse().unwrap());
            }
            headers
        };
        for (dest, site, framed) in [
            (Some("iframe"), Some("cross-site"), true),
            (Some("IFRAME"), Some("same-site"), true),
            (Some("frame"), Some("cross-site"), true),
            // Monokulo's own share page.
            (Some("iframe"), Some("same-origin"), false),
            (Some("document"), Some("cross-site"), false),
            (Some("document"), Some("none"), false),
            // Says nothing: not known to be a frame.
            (None, None, false),
            (Some("iframe"), None, false),
            (None, Some("cross-site"), false),
        ] {
            assert_eq!(
                framed_by_another_site(&headers(dest, site)),
                framed,
                "{dest:?} {site:?}"
            );
        }
    }

    #[test]
    fn a_page_stands_by_its_proof_its_domain_and_its_operator() {
        let domains = [domain("shop.example", Some(1), None)];
        assert_eq!(standing(&page(), &domains, NOW), PageStanding::Working);
        let failing = PublishedCheckoutPage {
            failing_since: Some(NOW - 60),
            ..page()
        };
        assert_eq!(
            standing(&failing, &domains, NOW),
            PageStanding::Failing { since: NOW - 60 }
        );
        let gone = PublishedCheckoutPage {
            failing_since: Some(NOW - domain_proof::GRACE_SECS),
            ..page()
        };
        assert_eq!(standing(&gone, &domains, NOW), PageStanding::NotShowing);
        assert_eq!(standing(&page(), &[], NOW), PageStanding::NotShowing);
        let off = PublishedCheckoutPage {
            turned_off_at: Some(NOW),
            ..page()
        };
        assert_eq!(standing(&off, &domains, NOW), PageStanding::TurnedOff);
    }

    /// The whole decision: each of these alone means the store's own layout.
    #[test]
    fn a_page_shows_only_when_everything_agrees() {
        let domains = [domain("shop.example", Some(1), None)];
        let shown = |page: Option<&PublishedCheckoutPage>,
                     domains: &[StoreDomainRow],
                     framed: bool,
                     asked| {
            let frame = if framed {
                shop_frame()
            } else {
                Frame::NotFramed
            };
            chosen_page(page, domains, &frame, asked, NOW).is_some()
        };
        assert!(shown(Some(&page()), &domains, true, false));
        assert!(!shown(None, &domains, true, false), "no page");
        assert!(!shown(Some(&page()), &domains, false, false), "not framed");
        assert!(
            !shown(Some(&page()), &domains, true, true),
            "an embed asked for a layout"
        );
        assert!(!shown(Some(&page()), &[], true, false), "no domain");
        let off = PublishedCheckoutPage {
            turned_off_at: Some(NOW),
            ..page()
        };
        assert!(!shown(Some(&off), &domains, true, false), "turned off");
        let broken = PublishedCheckoutPage {
            token: "broken".to_string(),
            template: "<img>".to_string(),
            ..page()
        };
        assert!(
            !shown(Some(&broken), &domains, true, false),
            "no longer reads"
        );
    }

    /// Only its own verified sites may frame it, even for an unrestricted store.
    #[test]
    fn a_chosen_page_can_only_be_framed_by_its_stores_sites() {
        let domains = [domain("shop.example", Some(1), None)];
        let chosen = chosen_page(Some(&page()), &domains, &shop_frame(), false, NOW).unwrap();
        assert_eq!(
            chosen.frame_ancestors.as_deref(),
            Some("frame-ancestors 'self' https://shop.example https://*.shop.example")
        );
    }

    /// A stolen dashboard can add a domain of its own and approve a page on it. That
    /// mustn't reach the shop's onion address, whose owner never saw the page: its
    /// customers get the layout, and the policy doesn't list it.
    #[test]
    fn a_page_shows_only_on_the_site_that_approved_it() {
        let domains = [
            domain(ONION, Some(1), None),
            domain("added-by-thief.example", Some(1), None),
        ];
        let onion_frame = |referrer: Option<&str>| Frame::OfAnotherSite {
            referrer: referrer.map(str::to_string),
        };
        let on = |frame: &Frame| chosen_page(Some(&page()), &domains, frame, false, NOW);
        assert!(
            on(&onion_frame(None)).is_none(),
            "the onion shop, no referrer"
        );
        assert!(
            on(&onion_frame(Some(&format!("http://{ONION}")))).is_none(),
            "the onion shop, naming itself"
        );
        let chosen = on(&onion_frame(Some("https://added-by-thief.example"))).unwrap();
        assert!(!chosen.frame_ancestors.unwrap().contains(ONION));

        let onion_only = [domain(ONION, Some(1), None)];
        let chosen =
            chosen_page(Some(&page()), &onion_only, &onion_frame(None), false, NOW).unwrap();
        assert!(chosen.frame_ancestors.unwrap().contains(ONION));
    }

    /// A site the store hasn't verified can still take a payment there, so it gets
    /// the layout, not a frame its browser would refuse.
    #[test]
    fn a_site_that_cant_frame_the_page_gets_the_layout() {
        let domains = [domain("shop.example", Some(1), None)];
        let shown = |referrer: Option<&str>| {
            let frame = Frame::OfAnotherSite {
                referrer: referrer.map(str::to_string),
            };
            chosen_page(Some(&page()), &domains, &frame, false, NOW).is_some()
        };
        assert!(shown(Some("https://shop.example")));
        assert!(shown(Some("https://www.shop.example")));
        assert!(!shown(Some("https://elsewhere.example")));
        assert!(!shown(Some("http://shop.example")), "not over https");
        assert!(!shown(None), "says nothing");
    }

    /// A frame moving itself on says same-origin, as Monokulo's own share page does
    /// on first load, so only the page's own mark keeps it on the page.
    #[test]
    fn a_frame_stays_on_the_page_only_by_its_own_mark() {
        let with = |dest: &str, site: &str, referer: Option<&str>| {
            let mut headers = HeaderMap::new();
            headers.insert("sec-fetch-dest", dest.parse().unwrap());
            headers.insert("sec-fetch-site", site.parse().unwrap());
            if let Some(referer) = referer {
                headers.insert("referer", referer.parse().unwrap());
            }
            headers
        };
        assert_eq!(
            frame_of(&with("iframe", "same-origin", None), true),
            Frame::ItsOwnNavigation
        );
        assert_eq!(
            frame_of(&with("iframe", "same-origin", None), false),
            Frame::NotFramed
        );
        assert_eq!(
            frame_of(&with("document", "same-origin", None), true),
            Frame::NotFramed,
            "opened on its own, whatever the link says"
        );
        assert_eq!(
            frame_of(
                &with(
                    "iframe",
                    "cross-site",
                    Some("https://elsewhere.example/cart?a=1")
                ),
                true
            ),
            Frame::OfAnotherSite {
                referrer: Some("https://elsewhere.example".to_string())
            },
            "a mark from another site proves nothing"
        );
    }

    /// A page already passing can fail a later release's checks: it then doesn't
    /// show, and neither an old template nor old CSS gets by.
    #[test]
    fn a_page_that_no_longer_passes_the_checks_doesnt_show() {
        let domains = [domain("shop.example", Some(1), None)];
        let shown = |page: PublishedCheckoutPage| {
            chosen_page(Some(&page), &domains, &shop_frame(), false, NOW).is_some()
        };
        assert!(!shown(PublishedCheckoutPage {
            token: "no-address".to_string(),
            template: "{{status}}{{qr}}<p>{{amount}}{{fiat}}</p>".to_string(),
            ..page()
        }));
        assert!(!shown(PublishedCheckoutPage {
            token: "bad-css".to_string(),
            css: "@import url(https://evil.example/a.css);".to_string(),
            ..page()
        }));
    }

    #[test]
    fn a_check_is_due_daily_or_hourly_when_something_is_wrong() {
        let fine = PublishedCheckoutPage {
            last_checked_at: Some(NOW),
            ..page()
        };
        assert!(!check_due(
            &fine,
            NOW + domain_proof::RECHECK_EVERY_SECS - 1
        ));
        assert!(check_due(&fine, NOW + domain_proof::RECHECK_EVERY_SECS));
        let failing = PublishedCheckoutPage {
            failing_since: Some(NOW),
            ..fine.clone()
        };
        assert!(check_due(
            &failing,
            NOW + domain_proof::FAILING_RECHECK_EVERY_SECS
        ));
        let no_answer = PublishedCheckoutPage {
            last_error: Some("x".to_string()),
            ..fine
        };
        assert!(check_due(
            &no_answer,
            NOW + domain_proof::FAILING_RECHECK_EVERY_SECS
        ));
    }
}
