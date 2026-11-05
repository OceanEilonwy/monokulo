//! Verified embed domains - the optional step a merchant takes to prove they
//! own the websites that show their store's checkout.
//!
//! A merchant adds a domain to a store; monokulo gives them a TXT record to
//! publish at `_monokulo.<domain>` holding a token unique to that store and
//! domain; once a lookup finds it, the domain (and every subdomain under it)
//! is verified. DNS is the proof because hacking a website rarely gives
//! control of its domain's DNS, so a hacker can't verify a site they merely
//! broke into.
//!
//! Verified domains are re-checked daily. A domain whose record goes missing
//! is *failing*: it keeps counting as verified for
//! [`domain_proof::GRACE_SECS`], then *lapses* until a check finds the record
//! again. A check with no answer (an outage) says nothing about the record, so
//! it never starts that clock. The store page warns about a failing or lapsed
//! domain from its first check to find it missing, and that warning can't be
//! dismissed.
//!
//! An onion address has no DNS, so a shop on one proves itself instead:
//! it serves the same token at `/.well-known/monokulo-verify`, which
//! monokulo fetches over Tor inside monokulo, never over the web. How either
//! is looked up and compared is [`domain_proof`]'s; this module is which
//! domains a store has, and what they allow.
//!
//! A store can then turn on "Only my verified domains can show this
//! checkout" ([`EmbedPolicy`]): its checkout pages tell browsers only its
//! verified domains may frame them (`frame-ancestors`), CORS answers only
//! those domains, and order creation refuses a browser request from anywhere
//! else. Off (the default), any website can.

use std::time::Duration;

use domain_proof::{
    is_onion, CheckOutcome, Proofs, FAILING_RECHECK_EVERY_SECS, RECHECK_EVERY_SECS, VERIFY,
};

use crate::db::{Database, Db, StoreDomainRow};

/// How many domains one store can have.
pub const MAX_DOMAINS_PER_STORE: usize = 10;

/// The scheme a page on `domain` is served over: onion services are plain
/// `http` (Tor encrypts them itself), everything else must be `https`.
fn page_scheme(domain: &str) -> &'static str {
    if is_onion(domain) {
        "http"
    } else {
        "https"
    }
}

/// A fresh random token for a new domain.
pub fn new_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Whether verifying `domain` covers `host`: the domain itself, or any
/// subdomain of it. `evilshop.example` is not covered by `shop.example`.
pub fn covers(domain: &str, host: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

/// A store's embed policy, looked up per request by public key.
pub struct EmbedPolicy {
    /// "Only my verified domains can show this checkout".
    pub restricted: bool,
    pub domains: Vec<StoreDomainRow>,
}

impl EmbedPolicy {
    /// The domains that currently count as verified.
    pub fn counting_domains(&self, now: i64) -> impl Iterator<Item = &str> {
        self.domains
            .iter()
            .filter(move |row| row.state(now).counts())
            .map(|row| row.domain.as_str())
    }

    /// Whether a browser page on `origin` (an `Origin` header value) may use
    /// this store's checkout: always when unrestricted, otherwise only an
    /// `https://` page (`http://` for an onion shop) on a counting domain
    /// or one of its subdomains.
    pub fn allows_origin(&self, origin: &str, now: i64) -> bool {
        if !self.restricted {
            return true;
        }
        let Some((scheme, authority)) = origin.split_once("://") else {
            return false;
        };
        let host = authority
            .split(':')
            .next()
            .unwrap_or("")
            .trim_end_matches('.')
            .to_ascii_lowercase();
        self.counting_domains(now)
            .any(|domain| covers(domain, &host) && scheme == page_scheme(domain))
    }

    /// The `Content-Security-Policy` a restricted store's pages are sent
    /// with: only monokulo itself and the counting domains (with their
    /// subdomains) may frame them. `None` when unrestricted.
    pub fn frame_ancestors(&self, now: i64) -> Option<String> {
        if !self.restricted {
            return None;
        }
        let mut policy = "frame-ancestors 'self'".to_string();
        for domain in self.counting_domains(now) {
            let scheme = page_scheme(domain);
            policy.push_str(&format!(" {scheme}://{domain} {scheme}://*.{domain}"));
        }
        Some(policy)
    }
}

/// The policy of the store with this public key, `None` for an unknown
/// key, or `Err` when the database didn't answer. The error is the
/// caller's to fail closed on: a store that restricted its checkout to
/// its verified domains must not accept orders from anywhere while the
/// policy can't be read.
pub async fn policy_for_public_key(
    db: &Database,
    public_key: &str,
) -> Result<Option<EmbedPolicy>, crate::db::DbError> {
    let key = public_key.to_string();
    db.read(move |db| db.embed_policy_for_public_key(&key))
        .await
        .map(|policy| {
            policy.map(|(restricted, domains)| EmbedPolicy {
                restricted,
                domains,
            })
        })
        .map_err(|e| {
            tracing::error!(public_key = %public_key, error = %e, "could not read the embed policy");
            e
        })
}

/// The domain of a store's site (a host, `shop.example:8443`) or of a URL,
/// when it's one that can be verified.
pub fn domain_of_site(site: &str) -> Option<String> {
    let url = if site.contains("://") {
        site.to_owned()
    } else {
        format!("https://{site}")
    };
    let host = url::Url::parse(&url).ok()?.host_str()?.to_string();
    domain_proof::normalize_domain(&host).ok()
}

/// A store's site (`shop.example`), when one of `domains` that counts as
/// verified at `now` covers it: the shop the checkout may tell the payer
/// they're paying, since its owner proved they run it. `None` for a store
/// without a site, or one whose site isn't verified.
pub fn verified_site(
    site: Option<&crate::stores::Site>,
    domains: &[StoreDomainRow],
    now: i64,
) -> Option<crate::stores::Site> {
    let site = site?;
    let host = domain_of_site(site.as_str())?;
    domains
        .iter()
        .any(|row| row.state(now).counts() && covers(&row.domain, &host))
        .then(|| site.clone())
}

/// What a merchant is told when removing a domain is refused by
/// [`would_leave_restricted_store_without_domain`].
pub const LAST_DOMAIN_MESSAGE: &str = "This is your last verified domain. Turn off \"Only my verified domains can show this checkout\" first - otherwise no website could show your checkout.";

/// Whether removing `domain_id` would leave a store that restricts its
/// checkout to its verified domains with none that counts, so no website
/// could show its checkout. Every path that removes a store's domain (the
/// Domains card, changing or removing the website) asks this inside the
/// write that removes it.
pub fn would_leave_restricted_store_without_domain(
    db: &Db,
    connection_id: &crate::db::ConnectionId,
    domain_id: &str,
    now: i64,
) -> Result<bool, crate::db::DbError> {
    if !db.embed_restricted(connection_id)? {
        return Ok(false);
    }
    let domains = db.list_store_domains(connection_id)?;
    let mut counting = domains.iter().filter(|domain| domain.state(now).counts());
    Ok(matches!(
        (counting.next(), counting.next()),
        (Some(only), None) if only.id == domain_id
    ))
}

/// Adds the site's domain to a store as a domain waiting for DNS, so the
/// merchant only has to publish the record. Skipped quietly when the site
/// has no verifiable domain, or the store already has it or is full.
pub fn suggest_site_domain(
    db: &Db,
    connection_id: &crate::db::ConnectionId,
    site: Option<&crate::stores::Site>,
    now: i64,
) {
    if let Some(domain) = site.and_then(|site| domain_of_site(site.as_str())) {
        suggest(db, connection_id, &domain, now);
    }
}

fn suggest(db: &Db, connection_id: &crate::db::ConnectionId, domain: &str, now: i64) {
    if let Err(e) = db.suggest_store_domain(connection_id, domain, now, MAX_DOMAINS_PER_STORE) {
        tracing::warn!(store.id = %connection_id, domain = %domain, error = %e, "could not add a domain to a store");
    }
}

/// Copies each existing store's own site domain into its domains, waiting
/// for DNS: the code step [`crate::db::STORE_DOMAINS_STEP`], run once, so a
/// domain the merchant removes afterwards stays removed. The stores it
/// copies for are those made before migration 0020
/// (`store_connections.domains_imported`); every store made since has its
/// site's domain added when it is made. Local to monokulo: the engine holds
/// no embedding policy, so nothing is read from it. Returns whether it ran.
pub fn import_existing_domains(db: &Db) -> Result<bool, crate::db::DbError> {
    db.run_backfill_step(crate::db::STORE_DOMAINS_STEP, |db| {
        for store in db.list_store_connections_awaiting_domain_import()? {
            suggest_site_domain(db, &store.id, store.site.as_ref(), crate::now_unix());
            db.mark_store_domains_imported(&store.id)?;
        }
        Ok(())
    })
}

/// Checks one domain and records the result on its row.
pub async fn check_and_record(
    db: &Database,
    proofs: &Proofs,
    row: &StoreDomainRow,
    now: i64,
) -> Result<CheckOutcome, crate::db::DbError> {
    let outcome = domain_proof::check(proofs, &VERIFY, &row.domain, &row.token).await;
    let error = outcome.error_message(&VERIFY, &row.domain, &row.token);
    let id = row.id.clone();
    // No answer says nothing about the proof, so an outage can't start the
    // failing clock, and in time lapse a domain whose record is still there.
    let answered = !matches!(outcome, CheckOutcome::LookupFailed(_));
    db.write(move |db| {
        if answered {
            db.record_store_domain_check(&id, now, error.as_deref())
        } else {
            db.record_store_domain_unanswered(&id, now, error.as_deref().unwrap_or_default())
        }
    })
    .await?;
    Ok(outcome)
}

/// Re-checks every verified domain that is due, one at a time.
pub async fn recheck_due(db: &Database, proofs: &Proofs, now: i64) {
    let due = match db
        .read(move |db| {
            db.list_store_domains_due_for_recheck(
                now,
                RECHECK_EVERY_SECS,
                FAILING_RECHECK_EVERY_SECS,
            )
        })
        .await
    {
        Ok(due) => due,
        Err(e) => {
            tracing::error!(error = %e, "could not list domains to re-check");
            return;
        }
    };
    for row in due {
        if let Err(e) = check_and_record(db, proofs, &row, now).await {
            tracing::warn!(domain = %row.domain, error = %e, "could not record a domain re-check");
        }
    }
}

/// Runs [`recheck_due`] every few minutes for the life of the process.
pub fn spawn_rechecks(db: Database, proofs: Proofs) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(5 * 60));
        loop {
            ticker.tick().await;
            // Each pass on a task of its own: a panic in one is logged and
            // the next pass still runs, rather than ending the re-checks
            // for the life of the process.
            let (db, proofs) = (db.clone(), proofs.clone());
            let pass = tokio::spawn(async move {
                recheck_due(&db, &proofs, crate::now_unix()).await;
            });
            if let Err(e) = pass.await {
                tracing::error!(error = %e, "a domain re-check pass failed");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_domain_covers_itself_and_its_subdomains_only() {
        assert!(covers("shop.example", "shop.example"));
        assert!(covers("shop.example", "www.shop.example"));
        assert!(covers("shop.example", "a.b.shop.example"));
        assert!(!covers("shop.example", "evilshop.example"));
        assert!(!covers("shop.example", "shop.example.evil.example"));
        assert!(!covers("www.shop.example", "shop.example"));
    }

    #[test]
    fn a_restricted_policy_allows_only_https_pages_on_counting_domains() {
        let now = 1_000_000;
        let domain =
            |name: &str, verified_at: Option<i64>, failing_since: Option<i64>| StoreDomainRow {
                domain: name.to_string(),
                verified_at,
                failing_since,
                ..row(None, None)
            };
        let mut policy = EmbedPolicy {
            restricted: false,
            domains: vec![
                domain("shop.example", Some(1), None),
                domain("pending.example", None, None),
                domain("failing.example", Some(1), Some(now - 60)),
                domain(
                    "lapsed.example",
                    Some(1),
                    Some(now - domain_proof::GRACE_SECS),
                ),
            ],
        };
        assert!(
            policy.allows_origin("https://anything.example", now),
            "unrestricted allows any site"
        );
        assert_eq!(policy.frame_ancestors(now), None);

        policy.restricted = true;
        assert!(policy.allows_origin("https://shop.example", now));
        assert!(policy.allows_origin("https://www.shop.example:8443", now));
        assert!(
            policy.allows_origin("https://failing.example", now),
            "still inside its grace period"
        );
        assert!(
            !policy.allows_origin("http://shop.example", now),
            "only https pages count"
        );
        assert!(!policy.allows_origin("https://evilshop.example", now));
        assert!(!policy.allows_origin("https://pending.example", now));
        assert!(!policy.allows_origin("https://lapsed.example", now));
        assert!(!policy.allows_origin("null", now));
        assert_eq!(
            policy.frame_ancestors(now).unwrap(),
            "frame-ancestors 'self' https://shop.example https://*.shop.example https://failing.example https://*.failing.example"
        );
    }

    /// The checkout names the store's site only while a verified domain
    /// covers it: not while it waits for DNS, nor once it has lapsed.
    #[test]
    fn a_site_is_named_only_while_a_verified_domain_covers_it() {
        let now = 1_000_000;
        // `row` is a domain for shop.example.
        let verified = [row(Some(1), None)];
        let site = |s: &str| crate::stores::Site::parse(s).ok();
        let named = |s: &str, domains: &[StoreDomainRow]| {
            verified_site(site(s).as_ref(), domains, now).map(|site| site.to_string())
        };
        assert_eq!(
            named("shop.example", &verified).as_deref(),
            Some("shop.example")
        );
        assert_eq!(
            named("www.shop.example:8443", &verified).as_deref(),
            Some("www.shop.example:8443")
        );
        assert_eq!(named("evilshop.example", &verified), None);
        assert_eq!(verified_site(None, &verified, now), None);
        let pending = [row(None, None)];
        assert_eq!(named("shop.example", &pending), None);
        let lapsed = [row(Some(1), Some(now - domain_proof::GRACE_SECS))];
        assert_eq!(named("shop.example", &lapsed), None);
    }

    #[test]
    fn a_site_url_suggests_its_domain_when_it_can_be_verified() {
        assert_eq!(
            domain_of_site("https://Shop.Example/wp"),
            Some("shop.example".to_string())
        );
        assert_eq!(
            domain_of_site("shop.example:8443"),
            Some("shop.example".to_string())
        );
        assert_eq!(domain_of_site("http://abcdefghijklmnop.onion"), None);
        assert_eq!(domain_of_site("http://192.0.2.1:8080"), None);
        assert_eq!(domain_of_site("not a url"), None);
    }

    const ONION: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";

    #[test]
    fn a_restricted_store_allows_its_onion_shop_over_http_only() {
        let policy = EmbedPolicy {
            restricted: true,
            domains: vec![
                StoreDomainRow {
                    domain: ONION.to_string(),
                    ..row(Some(1), None)
                },
                row(Some(1), None),
            ],
        };
        assert!(policy.allows_origin(&format!("http://{ONION}"), 10));
        assert!(policy.allows_origin(&format!("http://shop.{ONION}"), 10));
        assert!(!policy.allows_origin(&format!("https://{ONION}"), 10));
        assert!(policy.allows_origin("https://shop.example", 10));
        assert!(
            !policy.allows_origin("http://shop.example", 10),
            "the web needs https"
        );
        let ancestors = policy.frame_ancestors(10).unwrap();
        assert!(
            ancestors.contains(&format!(" http://{ONION} http://*.{ONION}")),
            "{ancestors}"
        );
        assert!(
            ancestors.contains(" https://shop.example https://*.shop.example"),
            "{ancestors}"
        );
    }

    fn row(verified_at: Option<i64>, failing_since: Option<i64>) -> StoreDomainRow {
        StoreDomainRow {
            id: "d1".to_string(),
            connection_id: engine_protocol::ids::ConnectionId::new("c1".to_string()),
            domain: "shop.example".to_string(),
            token: "abc".to_string(),
            created_at: 0,
            verified_at,
            failing_since,
            last_checked_at: None,
            last_error: None,
        }
    }
}
