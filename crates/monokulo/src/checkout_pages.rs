//! Which checkout a customer sees, and the custom page's standing.
//!
//! A custom page is a merchant's own words at this Monokulo's address, so it shows
//! only where they own the frame around it: every domain the store has has approved
//! this version, by a DNS record or (on an onion address) a file over Tor, and the
//! browser says it's a frame of one of their sites. Everywhere else the store's
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

/// The domains that must approve a store's custom page: all it has that count. A
/// stolen dashboard adding a domain of its own can't approve a version by itself, as
/// the shop's domain has to as well.
pub fn approvers(domains: &[StoreDomainRow], now: i64) -> Vec<&StoreDomainRow> {
    domains
        .iter()
        .filter(|row| row.state(now).counts())
        .collect()
}

/// The domains `page` shows on: those that count, and that its proof was last found
/// on. A domain coming to count later hasn't approved these words, whoever wrote
/// them: say a thief published while the shop's own domain was gone.
pub fn showing_on<'a>(
    page: &PublishedCheckoutPage,
    domains: &'a [StoreDomainRow],
    now: i64,
) -> Vec<&'a StoreDomainRow> {
    approvers(domains, now)
        .into_iter()
        .filter(|row| page.approved_domains.contains(&row.domain))
        .collect()
}

/// The domains that count and haven't approved `page`, whose next look is soon.
pub fn waiting_on(
    page: &PublishedCheckoutPage,
    domains: &[StoreDomainRow],
    now: i64,
) -> Vec<String> {
    approver_names(domains, now)
        .into_iter()
        .filter(|name| !page.approved_domains.contains(name))
        .collect()
}

/// What a domain's proof holds: the version on the web. An onion address's is hashed
/// with its name, so the two public proofs can't be matched to tie a store's onion
/// address to its web domain.
pub fn proof_token(token: &str, domain: &str) -> String {
    if domain_proof::is_onion(domain) {
        checkout_template::version_token(domain, token, "")
    } else {
        token.to_string()
    }
}

/// [`approvers`]' names.
pub fn approver_names(domains: &[StoreDomainRow], now: i64) -> Vec<String> {
    approvers(domains, now)
        .into_iter()
        .map(|row| row.domain.clone())
        .collect()
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
    if showing_on(page, domains, now).is_empty() || parsed(page).is_none() {
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
/// the rest is its [`Frame`] and the page's [`standing`]. A frame of a site it doesn't
/// show on gets the layout rather than a page its browser would refuse. Tor Browser
/// sends no `Referer` from an onion shop, so with none, only a request over Tor is
/// taken to be that shop, and any other site asking for no `Referer` gets the layout.
/// `over_tor` also says which of the store's sites the policy names: the one this
/// network reaches, so no response ties its onion address to its web domain.
pub fn chosen_page(
    page: Option<&PublishedCheckoutPage>,
    domains: &[StoreDomainRow],
    frame: &Frame,
    over_tor: bool,
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
    let sites: Vec<StoreDomainRow> = showing_on(page, domains, now)
        .into_iter()
        .filter(|row| domain_proof::is_onion(&row.domain) == over_tor)
        .cloned()
        .collect();
    if sites.is_empty() {
        return None;
    }
    let policy = crate::embed_domains::EmbedPolicy {
        restricted: true,
        domains: sites,
    };
    let here = match frame {
        Frame::NotFramed => false,
        Frame::ItsOwnNavigation => true,
        Frame::OfAnotherSite {
            referrer: Some(origin),
        } => policy.allows_origin(origin, now),
        Frame::OfAnotherSite { referrer: None } => over_tor,
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

/// What looking at one domain's proof found.
pub struct DomainCheck {
    pub domain: String,
    pub outcome: CheckOutcome,
    /// What's wrong, for a person; `None` when it's found.
    pub reason: Option<String>,
}

/// Looks for `token`'s proof on each of `domains`.
pub async fn check_every(proofs: &Proofs, domains: &[String], token: &str) -> Vec<DomainCheck> {
    let mut checks = Vec::with_capacity(domains.len());
    for domain in domains {
        let token = proof_token(token, domain);
        let outcome = domain_proof::check(proofs, &TEMPLATE, domain, &token).await;
        checks.push(DomainCheck {
            domain: domain.clone(),
            reason: outcome.error_message(&TEMPLATE, domain, &token),
            outcome,
        });
    }
    checks
}

/// What a page's proofs came to, together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approved {
    /// On every domain.
    Yes,
    /// A domain answered without it, and says why. That counts whatever the others
    /// said, as no answer from one says nothing about another.
    Missing(String),
    /// None is known to be missing, but one didn't answer.
    NoAnswer(String),
}

/// What `checks` come to.
pub fn approved(checks: &[DomainCheck]) -> Approved {
    let reasons = |wanted: &dyn Fn(&CheckOutcome) -> bool| {
        checks
            .iter()
            .filter(|check| wanted(&check.outcome))
            .filter_map(|check| check.reason.as_deref())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let missing =
        reasons(&|outcome| matches!(outcome, CheckOutcome::Missing | CheckOutcome::OtherValue));
    if !missing.is_empty() {
        return Approved::Missing(missing);
    }
    let no_answer = reasons(&|outcome| matches!(outcome, CheckOutcome::LookupFailed(_)));
    if !no_answer.is_empty() {
        return Approved::NoAnswer(no_answer);
    }
    Approved::Yes
}

/// What a lookup found, recorded on a page as on a verified domain: an answer
/// without a proof starts the grace period, and no answer changes nothing but says
/// so.
pub async fn check_and_record(
    db: &Database,
    proofs: &Proofs,
    id: &ConnectionId,
    domains: &[String],
    token: &str,
    now: i64,
) -> Result<Approved, crate::db::DbError> {
    let approved = approved(&check_every(proofs, domains, token).await);
    let (id, token, domains, result) = (
        id.clone(),
        token.to_string(),
        domains.to_vec(),
        approved.clone(),
    );
    db.write(move |db| match &result {
        Approved::Yes => db.record_checkout_page_found(&id, &token, &domains, now),
        Approved::Missing(reason) => db.record_checkout_page_check(&id, &token, now, true, reason),
        Approved::NoAnswer(reason) => {
            db.record_checkout_page_check(&id, &token, now, false, reason)
        }
    })
    .await?;
    Ok(approved)
}

/// Whether a page is due a look at its proofs: daily, or hourly while something's
/// wrong or a domain hasn't approved it, so a fix or a new domain is noticed well
/// inside the grace period.
pub fn check_due(page: &PublishedCheckoutPage, domains: &[StoreDomainRow], now: i64) -> bool {
    let every = if page.failing_since.is_some()
        || page.last_error.is_some()
        || !waiting_on(page, domains, now).is_empty()
    {
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
    if page.turned_off_at.is_some() || !check_due(page, domains, now) {
        return;
    }
    let domains = approver_names(domains, now);
    if domains.is_empty() {
        return;
    }
    let (id, token) = (page.connection_id.clone(), page.token.clone());
    if !CHECKING.lock().insert(id.clone()) {
        return;
    }
    tokio::spawn(async move {
        let checked = check_and_record(&db, &proofs, &id, &domains, &token, now).await;
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
            approved_domains: vec!["shop.example".to_string(), ONION.to_string()],
        }
    }

    #[test]
    fn every_domain_that_counts_must_approve() {
        let web = domain("shop.example", Some(1), None);
        let onion = domain(ONION, Some(1), None);
        let pending = domain("pending.example", None, None);
        let lapsed = domain(
            "lapsed.example",
            Some(1),
            Some(NOW - domain_proof::GRACE_SECS),
        );
        assert_eq!(
            approver_names(&[onion.clone(), web.clone()], NOW),
            [ONION, "shop.example"]
        );
        assert_eq!(approver_names(std::slice::from_ref(&onion), NOW), [ONION]);
        assert_eq!(
            approver_names(&[pending, lapsed], NOW),
            Vec::<String>::new()
        );
    }

    /// Together, one domain missing its proof outweighs another not answering.
    #[test]
    fn a_missing_proof_counts_whatever_the_other_domain_says() {
        let check = |domain: &str, outcome: CheckOutcome| DomainCheck {
            domain: domain.to_string(),
            reason: outcome.error_message(&TEMPLATE, domain, "t"),
            outcome,
        };
        let found = || check("shop.example", CheckOutcome::Found);
        let missing = || check("shop.example", CheckOutcome::Missing);
        let silent = || check(ONION, CheckOutcome::LookupFailed("timed out".to_string()));
        assert_eq!(
            approved(&[found(), check(ONION, CheckOutcome::Found)]),
            Approved::Yes
        );
        assert!(matches!(
            approved(&[found(), silent()]),
            Approved::NoAnswer(_)
        ));
        let Approved::Missing(reason) = approved(&[missing(), silent()]) else {
            panic!("a missing proof is missing");
        };
        assert!(
            reason.contains("_monokulo-template.shop.example"),
            "{reason}"
        );
        assert!(!reason.contains("Tor"), "{reason}");
    }

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
            chosen_page(page, domains, &frame, false, asked, NOW).is_some()
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
        let chosen =
            chosen_page(Some(&page()), &domains, &shop_frame(), false, false, NOW).unwrap();
        assert_eq!(
            chosen.frame_ancestors.as_deref(),
            Some("frame-ancestors 'self' https://shop.example https://*.shop.example")
        );
    }

    /// A page shows on a store's sites, each on the network that reaches it. Its policy
    /// names only that one, so no response ties an onion address to its web domain.
    /// An onion shop sends no `Referer`, which only a request over Tor is taken to be.
    #[test]
    fn a_page_shows_on_every_site_each_on_its_own_network() {
        let both = [
            domain("shop.example", Some(1), None),
            domain(ONION, Some(1), None),
        ];
        let frame = |referrer: Option<&str>| Frame::OfAnotherSite {
            referrer: referrer.map(str::to_string),
        };
        let on = |frame: &Frame, over_tor| {
            chosen_page(Some(&page()), &both, frame, over_tor, false, NOW)
        };
        let web = on(&frame(Some("https://shop.example")), false).unwrap();
        let web_policy = web.frame_ancestors.unwrap();
        assert!(web_policy.contains("https://shop.example"), "{web_policy}");
        assert!(!web_policy.contains(ONION), "{web_policy}");

        let tor = on(&frame(None), true).unwrap();
        let tor_policy = tor.frame_ancestors.unwrap();
        assert!(
            tor_policy.contains(&format!("http://{ONION}")),
            "{tor_policy}"
        );
        assert!(!tor_policy.contains("shop.example"), "{tor_policy}");
        assert!(on(&frame(Some(&format!("http://{ONION}"))), true).is_some());

        // Anywhere else, the layout, and never a frame its browser would refuse.
        assert!(on(&frame(None), false).is_none(), "no Referer, on the web");
        assert!(on(&frame(Some("https://elsewhere.example")), false).is_none());
        assert!(on(&frame(Some("https://shop.example")), true).is_none());
    }

    /// A page shows only where its proof was found, however a domain came to count. A
    /// thief can publish while the shop's domain is gone; when it's back it must not
    /// show their words, and it's due a look. Finding it there is what adds it.
    #[test]
    fn a_domain_that_comes_to_count_later_doesnt_show_words_it_never_approved() {
        let thief = "thief.example";
        let domains = [
            domain("shop.example", Some(1), None),
            domain(thief, Some(1), None),
        ];
        let theirs = PublishedCheckoutPage {
            approved_domains: vec![thief.to_string()],
            ..page()
        };
        let chosen = |referrer: &str| {
            let frame = Frame::OfAnotherSite {
                referrer: Some(referrer.to_string()),
            };
            chosen_page(Some(&theirs), &domains, &frame, false, false, NOW)
        };
        assert!(chosen("https://shop.example").is_none());
        let policy = chosen(&format!("https://{thief}"))
            .unwrap()
            .frame_ancestors
            .unwrap();
        assert!(!policy.contains("shop.example"), "{policy}");
        assert_eq!(waiting_on(&theirs, &domains, NOW), ["shop.example"]);
        assert!(check_due(
            &PublishedCheckoutPage {
                last_checked_at: Some(NOW - domain_proof::FAILING_RECHECK_EVERY_SECS),
                ..theirs.clone()
            },
            &domains,
            NOW
        ));
        // A page with nowhere left to show isn't showing, as its card says.
        let shop_only = [domain("shop.example", Some(1), None)];
        assert_eq!(standing(&theirs, &shop_only, NOW), PageStanding::NotShowing);
    }

    /// One domain lapsing leaves the other showing, and it's no longer asked.
    #[test]
    fn a_lapsed_or_pending_domain_isnt_asked_and_the_other_stays() {
        let lapsed = domain(ONION, Some(1), Some(NOW - domain_proof::GRACE_SECS));
        let pending = domain(ONION, None, None);
        let web = domain("shop.example", Some(1), None);
        for other in [lapsed, pending] {
            let domains = [web.clone(), other];
            assert_eq!(approver_names(&domains, NOW), ["shop.example"]);
            assert_eq!(standing(&page(), &domains, NOW), PageStanding::Working);
        }
    }

    /// The two proofs of a version can't be matched by anyone reading both.
    #[test]
    fn an_onions_proof_cant_be_matched_to_the_web_domains() {
        assert_eq!(proof_token("version", "shop.example"), "version");
        let onion = proof_token("version", ONION);
        assert_ne!(onion, "version");
        assert_ne!(onion, proof_token("other", ONION), "still per version");
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
            chosen_page(Some(&page()), &domains, &frame, false, false, NOW).is_some()
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
            chosen_page(Some(&page), &domains, &shop_frame(), false, false, NOW).is_some()
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
            &[],
            NOW + domain_proof::RECHECK_EVERY_SECS - 1
        ));
        assert!(check_due(
            &fine,
            &[],
            NOW + domain_proof::RECHECK_EVERY_SECS
        ));
        let failing = PublishedCheckoutPage {
            failing_since: Some(NOW),
            ..fine.clone()
        };
        assert!(check_due(
            &failing,
            &[],
            NOW + domain_proof::FAILING_RECHECK_EVERY_SECS
        ));
        let no_answer = PublishedCheckoutPage {
            last_error: Some("x".to_string()),
            ..fine
        };
        assert!(check_due(
            &no_answer,
            &[],
            NOW + domain_proof::FAILING_RECHECK_EVERY_SECS
        ));
    }
}
