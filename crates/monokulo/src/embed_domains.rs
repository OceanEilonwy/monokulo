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
//! is *failing*: it keeps counting as verified for [`GRACE_SECS`], then
//! *lapses* until a check finds the record again. The store page warns about
//! a failing or lapsed domain from its first failed check, and that warning
//! can't be dismissed.
//!
//! An onion address has no DNS, so a shop on one proves itself instead:
//! it serves the same token at `/.well-known/monokulo-verify`, which
//! monokulo fetches over Tor inside monokulo, never over the web
//! ([`WellKnownFetch`]). That is weaker than DNS - whoever can change the
//! shop's files can publish it - but it is the strongest proof an onion
//! shop can give, and only the shop's own key can serve its address.
//!
//! A store can then turn on "Only my verified domains can show this
//! checkout" ([`EmbedPolicy`]): its checkout pages tell browsers only its
//! verified domains may frame them (`frame-ancestors`), CORS answers only
//! those domains, and order creation refuses a browser request from anywhere
//! else. Off (the default), any website can.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::db::{Database, Db, StoreDomainRow};

/// The label the TXT record lives under: `_monokulo.shop.example`. Its own
/// name, so it doesn't crowd the domain's main TXT records.
pub const RECORD_LABEL: &str = "_monokulo";
/// What the record's value starts with, followed by the token.
pub const RECORD_VALUE_PREFIX: &str = "monokulo-verify=";
/// How long a verified domain whose record has gone missing keeps counting
/// as verified.
pub const GRACE_SECS: i64 = 3 * 24 * 60 * 60;
/// How often a verified domain is re-checked.
pub const RECHECK_EVERY_SECS: i64 = 24 * 60 * 60;
/// How often a failing domain is re-checked, so a fixed record is noticed
/// well within the grace period without the merchant clicking anything.
pub const FAILING_RECHECK_EVERY_SECS: i64 = 60 * 60;
/// The shortest gap between two checks of one domain the merchant can ask for.
pub const MIN_CHECK_GAP_SECS: i64 = 30;
/// How many domains one store can have.
pub const MAX_DOMAINS_PER_STORE: usize = 10;
/// A whole lookup: referrals, and the machine's resolvers if the root
/// servers can't be reached.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(20);

/// Turns what a merchant typed into a bare, lowercase domain name, or says
/// why it can't be one. Accepts a pasted URL (`https://Shop.Example/path`)
/// and drops a leading `*.`, since every domain covers its subdomains anyway.
pub fn normalize_domain(input: &str) -> Result<String, &'static str> {
    let mut domain = input.trim().to_ascii_lowercase();
    for scheme in ["https://", "http://"] {
        if let Some(rest) = domain.strip_prefix(scheme) {
            domain = rest.to_string();
        }
    }
    if let Some(end) = domain.find(['/', '?', '#']) {
        domain.truncate(end);
    }
    let domain = domain
        .strip_prefix("*.")
        .unwrap_or(&domain)
        .trim_end_matches('.')
        .to_string();
    if domain.is_empty() {
        return Err("Enter a domain, like shop.example.");
    }
    if domain.contains(':') || domain.contains('@') {
        return Err("Enter just the domain, without a port or login, like shop.example.");
    }
    if !domain.is_ascii() {
        return Err("Enter the domain in its ASCII form (the xn-- version) - international characters can't be checked.");
    }
    if crate::onion::is_onion_host(&domain) {
        // The service itself: its subdomains are the same service, and
        // covered anyway.
        return crate::onion::v3_host(&domain)
            .map(|host| crate::onion::service_of(&host).to_string())
            .map_err(|_| "That isn't a valid onion address: v3 addresses have 56 letters and digits before .onion. Check it for a typo.");
    }
    if domain.parse::<std::net::Ipv4Addr>().is_ok() {
        return Err("Enter a domain name, not an IP address.");
    }
    if domain.len() > 253 || !domain.contains('.') {
        return Err("Enter a full domain, like shop.example.");
    }
    let valid_label = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    };
    if !domain.split('.').all(valid_label) {
        return Err("That isn't a valid domain name. Use letters, digits, hyphens and dots, like shop.example.");
    }
    Ok(domain)
}

/// Where an onion shop serves its verification file.
pub const WELL_KNOWN_PATH: &str = "/.well-known/monokulo-verify";

/// The largest verification file read: a few tokens, one per line.
pub const WELL_KNOWN_MAX_BYTES: usize = 4096;

/// Whether `domain` is an onion address, proved by its file rather than DNS.
pub fn is_onion(domain: &str) -> bool {
    crate::onion::is_onion_host(domain)
}

/// The scheme a page on `domain` is served over: onion services are plain
/// `http` (Tor encrypts them itself), everything else must be `https`.
fn page_scheme(domain: &str) -> &'static str {
    if is_onion(domain) {
        "http"
    } else {
        "https"
    }
}

/// Where an onion shop's verification file is.
pub fn well_known_url(domain: &str) -> String {
    format!("http://{domain}{WELL_KNOWN_PATH}")
}

/// Where the TXT record for `domain` lives.
pub fn record_name(domain: &str) -> String {
    format!("{RECORD_LABEL}.{domain}")
}

/// The TXT record's value for `token`.
pub fn record_value(token: &str) -> String {
    format!("{RECORD_VALUE_PREFIX}{token}")
}

/// A fresh random token for a new domain.
pub fn new_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Where a domain stands, derived from its row at `now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainState {
    /// Never verified yet: waiting for the merchant to publish the record.
    Pending,
    /// Verified, and its latest check found the record.
    Verified,
    /// Verified, but its record has been missing since `since`; still
    /// counts as verified until `since + GRACE_SECS`.
    Failing { since: i64 },
    /// Its record has been missing for longer than the grace period, so it
    /// no longer counts as verified until a check finds it again.
    Lapsed { since: i64 },
}

impl DomainState {
    pub fn of(row: &StoreDomainRow, now: i64) -> Self {
        match (row.verified_at, row.failing_since) {
            (None, _) => DomainState::Pending,
            (Some(_), None) => DomainState::Verified,
            (Some(_), Some(since)) if now - since < GRACE_SECS => DomainState::Failing { since },
            (Some(_), Some(since)) => DomainState::Lapsed { since },
        }
    }

    /// Whether the domain currently counts as verified.
    pub fn counts(self) -> bool {
        matches!(self, DomainState::Verified | DomainState::Failing { .. })
    }
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
            .filter(move |row| DomainState::of(row, now).counts())
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
    normalize_domain(&host).ok()
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
        .any(|row| DomainState::of(row, now).counts() && covers(&row.domain, &host))
        .then(|| site.clone())
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
/// for DNS - once per store (`store_connections.domains_imported`), so a
/// domain the merchant removes afterwards stays removed. Local to monokulo:
/// the engine holds no embedding policy, so nothing is read from it.
pub fn import_existing_domains(db: &Db) {
    let stores = match db.list_store_connections_awaiting_domain_import() {
        Ok(stores) => stores,
        Err(e) => {
            tracing::error!(error = %e, "could not list stores to import domains for");
            return;
        }
    };
    for store in stores {
        suggest_site_domain(db, &store.id, store.site.as_ref(), crate::now_unix());
        if let Err(e) = db.mark_store_domains_imported(&store.id) {
            tracing::warn!(store.id = %store.id, error = %e, "could not mark the store's domains imported");
        }
    }
}

/// What one lookup of a domain's record found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// A TXT record at the right name holds the right token.
    Found,
    /// The name has no TXT record holding this token (either no record at
    /// all, or only other values).
    Missing,
    /// The lookup itself failed (timeout, resolver unreachable).
    LookupFailed(String),
}

impl CheckOutcome {
    /// The reason shown to the merchant for a check that didn't find the
    /// record, or `None` for one that did.
    pub fn error_message(&self, domain: &str, token: &str) -> Option<String> {
        match self {
            CheckOutcome::Found => None,
            CheckOutcome::Missing if is_onion(domain) => Some(format!(
                "The shop answered, but {} doesn't hold {}.",
                well_known_url(domain),
                record_value(token)
            )),
            CheckOutcome::Missing => Some(format!(
                "No TXT record at {} with the value {} was found.",
                record_name(domain),
                record_value(token)
            )),
            CheckOutcome::LookupFailed(error) if is_onion(domain) => {
                Some(format!("Fetching the file over Tor failed: {error}"))
            }
            CheckOutcome::LookupFailed(error) => Some(format!("The DNS lookup failed: {error}")),
        }
    }
}

/// The TXT values published at a name. `Ok(vec![])` means the name has no
/// TXT records; `Err` means the lookup itself failed. A trait so tests can
/// answer without a network.
pub trait TxtLookup: Send + Sync {
    fn txt_values<'a>(
        &'a self,
        name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + Send + 'a>>;
}

/// The real lookup ([`shared::dns`]): from the root servers down to the
/// domain's own name servers, so a record is seen the moment it's published,
/// with no resolver's cache still saying there is no such name.
pub struct LiveDns(shared::dns::Resolver);

impl LiveDns {
    pub fn new() -> Self {
        LiveDns(shared::dns::Resolver::system())
    }
}

impl Default for LiveDns {
    fn default() -> Self {
        LiveDns::new()
    }
}

impl TxtLookup for LiveDns {
    fn txt_values<'a>(
        &'a self,
        name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + Send + 'a>> {
        Box::pin(async move {
            match tokio::time::timeout(LOOKUP_TIMEOUT, self.0.txt(name)).await {
                Err(_) => Err("timed out".to_string()),
                Ok(result) => result.map_err(|e| e.to_string()),
            }
        })
    }
}

/// A lookup that always fails, saying why: for a server run without the
/// network (the test state, the browser fixture, the e2e harness).
pub struct UnavailableDns(pub String);

impl TxtLookup for UnavailableDns {
    fn txt_values<'a>(
        &'a self,
        _name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + Send + 'a>> {
        let error = self.0.clone();
        Box::pin(async move { Err(error) })
    }
}

/// An onion shop's verification file, fetched over Tor: `Ok(Some(text))`
/// when the shop served it, `Ok(None)` when it answered without it (not
/// found, a redirect, too big), `Err` when it couldn't be reached. A trait
/// so tests answer without Tor.
pub trait WellKnownFetch: Send + Sync {
    fn fetch<'a>(
        &'a self,
        domain: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<String>, String>> + Send + 'a>>;
}

/// A fetch that always fails, saying why: for a server without Tor inside
/// it, and tests that don't need one.
pub struct UnavailableTor(pub String);

impl WellKnownFetch for UnavailableTor {
    fn fetch<'a>(
        &'a self,
        _domain: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<String>, String>> + Send + 'a>> {
        let error = self.0.clone();
        Box::pin(async move { Err(error) })
    }
}

/// How a domain is proved: DNS for a web domain, the file over Tor for an
/// onion one.
#[derive(Clone)]
pub struct Proofs {
    pub dns: Arc<dyn TxtLookup>,
    pub onion: Arc<dyn WellKnownFetch>,
}

/// Looks up `domain`'s proof (its TXT record, or an onion shop's file)
/// and compares it with `token`.
pub async fn check(proofs: &Proofs, domain: &str, token: &str) -> CheckOutcome {
    let expected = record_value(token);
    if is_onion(domain) {
        return match proofs.onion.fetch(domain).await {
            Ok(Some(text)) if text.lines().any(|line| line.trim() == expected) => {
                CheckOutcome::Found
            }
            Ok(_) => CheckOutcome::Missing,
            Err(error) => CheckOutcome::LookupFailed(error),
        };
    }
    match proofs.dns.txt_values(&record_name(domain)).await {
        Ok(values) if values.iter().any(|value| value.trim() == expected) => CheckOutcome::Found,
        Ok(_) => CheckOutcome::Missing,
        Err(error) => CheckOutcome::LookupFailed(error),
    }
}

/// Checks one domain and records the result on its row.
pub async fn check_and_record(
    db: &Database,
    proofs: &Proofs,
    row: &StoreDomainRow,
    now: i64,
) -> Result<CheckOutcome, crate::db::DbError> {
    let outcome = check(proofs, &row.domain, &row.token).await;
    let error = outcome.error_message(&row.domain, &row.token);
    let id = row.id.clone();
    db.write(move |db| db.record_store_domain_check(&id, now, error.as_deref()))
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
pub mod test_support {
    use parking_lot::Mutex;
    use std::collections::HashMap;

    use super::*;

    /// Answers lookups from a table the test fills in; a name set to `Err`
    /// fails its lookup.
    #[derive(Default)]
    pub struct FakeDns {
        pub records: Mutex<HashMap<String, Result<Vec<String>, String>>>,
    }

    impl FakeDns {
        pub fn publish(&self, name: &str, value: &str) {
            self.records
                .lock()
                .insert(name.to_string(), Ok(vec![value.to_string()]));
        }
        pub fn remove(&self, name: &str) {
            self.records.lock().remove(name);
        }
        pub fn fail(&self, name: &str, error: &str) {
            self.records
                .lock()
                .insert(name.to_string(), Err(error.to_string()));
        }
    }

    impl TxtLookup for FakeDns {
        fn txt_values<'a>(
            &'a self,
            name: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + Send + 'a>> {
            let answer = self
                .records
                .lock()
                .get(name)
                .cloned()
                .unwrap_or(Ok(Vec::new()));
            Box::pin(async move { answer })
        }
    }

    /// Answers onion shops' verification files from a table the test
    /// fills in; a shop set to `Err` can't be reached, one not in it
    /// answers without the file.
    #[derive(Default)]
    pub struct FakeOnion {
        pub files: Mutex<HashMap<String, Result<Option<String>, String>>>,
    }

    impl FakeOnion {
        pub fn serve(&self, domain: &str, text: &str) {
            self.files
                .lock()
                .insert(domain.to_string(), Ok(Some(text.to_string())));
        }
        pub fn fail(&self, domain: &str, error: &str) {
            self.files
                .lock()
                .insert(domain.to_string(), Err(error.to_string()));
        }
    }

    impl WellKnownFetch for FakeOnion {
        fn fetch<'a>(
            &'a self,
            domain: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Option<String>, String>> + Send + 'a>> {
            let answer = self.files.lock().get(domain).cloned().unwrap_or(Ok(None));
            Box::pin(async move { answer })
        }
    }

    /// Proofs answered by `dns` and `onion`.
    pub fn proofs(dns: Arc<FakeDns>, onion: Arc<FakeOnion>) -> Proofs {
        Proofs { dns, onion }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{FakeDns, FakeOnion};
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
                domain("lapsed.example", Some(1), Some(now - GRACE_SECS)),
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
        let lapsed = [row(Some(1), Some(now - GRACE_SECS))];
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

    #[test]
    fn normalize_domain_accepts_urls_and_rejects_what_cannot_be_verified() {
        assert_eq!(
            normalize_domain(" https://Shop.Example/checkout?x=1 "),
            Ok("shop.example".to_string())
        );
        assert_eq!(
            normalize_domain("*.shop.example."),
            Ok("shop.example".to_string())
        );
        assert_eq!(
            normalize_domain("www.shop-outlet.example"),
            Ok("www.shop-outlet.example".to_string())
        );
        assert!(
            normalize_domain("abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrstuv.onion")
                .unwrap_err()
                .contains("isn't a valid onion address")
        );
        assert!(normalize_domain("192.0.2.1")
            .unwrap_err()
            .contains("IP address"));
        assert!(normalize_domain("shop.example:8443").is_err());
        assert!(normalize_domain("localhost").is_err());
        assert!(normalize_domain("-shop.example").is_err());
        assert!(normalize_domain("shop_example.com").is_err());
        assert!(normalize_domain("bücher.example").is_err());
        assert!(normalize_domain("").is_err());
    }

    const ONION: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";

    #[test]
    fn an_onion_shop_is_added_as_its_service_and_a_typo_is_caught() {
        assert_eq!(
            normalize_domain(&format!("http://{ONION}/shop")).unwrap(),
            ONION
        );
        assert_eq!(normalize_domain(&format!("www.{ONION}")).unwrap(), ONION);
        let typo = ONION.replacen("2gzy", "2gzz", 1);
        assert!(normalize_domain(&typo).unwrap_err().contains("typo"));
    }

    #[tokio::test]
    async fn an_onion_shop_is_proved_by_its_file_over_tor_never_by_dns() {
        let (dns, onion) = (Arc::new(FakeDns::default()), Arc::new(FakeOnion::default()));
        let proofs = test_support::proofs(dns.clone(), onion.clone());
        // A TXT record means nothing for an onion shop.
        dns.publish(&record_name(ONION), "monokulo-verify=abc");
        assert_eq!(check(&proofs, ONION, "abc").await, CheckOutcome::Missing);
        // One store's line among others'.
        onion.serve(ONION, "monokulo-verify=other\r\n  monokulo-verify=abc  \n");
        assert_eq!(check(&proofs, ONION, "abc").await, CheckOutcome::Found);
        onion.fail(ONION, "the onion service couldn't be found");
        let failed = check(&proofs, ONION, "abc").await;
        assert_eq!(
            failed.error_message(ONION, "abc").unwrap(),
            "Fetching the file over Tor failed: the onion service couldn't be found"
        );
        assert!(CheckOutcome::Missing
            .error_message(ONION, "abc")
            .unwrap()
            .contains("/.well-known/monokulo-verify"));
    }

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

    #[tokio::test]
    async fn check_finds_the_token_only_at_the_right_name() {
        let fake = Arc::new(FakeDns::default());
        let dns = test_support::proofs(fake.clone(), Arc::default());
        assert_eq!(
            check(&dns, "shop.example", "abc").await,
            CheckOutcome::Missing
        );

        fake.publish("_monokulo.shop.example", "monokulo-verify=wrong");
        assert_eq!(
            check(&dns, "shop.example", "abc").await,
            CheckOutcome::Missing
        );

        fake.publish("_monokulo.shop.example", "monokulo-verify=abc");
        assert_eq!(
            check(&dns, "shop.example", "abc").await,
            CheckOutcome::Found
        );
        assert_eq!(
            check(&dns, "other.example", "abc").await,
            CheckOutcome::Missing
        );

        fake.fail("_monokulo.shop.example", "timed out");
        assert_eq!(
            check(&dns, "shop.example", "abc").await,
            CheckOutcome::LookupFailed("timed out".to_string())
        );
    }

    fn row(verified_at: Option<i64>, failing_since: Option<i64>) -> StoreDomainRow {
        StoreDomainRow {
            id: "d1".to_string(),
            connection_id: shared::ids::ConnectionId::new("c1".to_string()),
            domain: "shop.example".to_string(),
            token: "abc".to_string(),
            created_at: 0,
            verified_at,
            failing_since,
            last_checked_at: None,
            last_error: None,
        }
    }

    #[test]
    fn a_missing_record_keeps_counting_through_the_grace_period_then_lapses() {
        assert_eq!(DomainState::of(&row(None, None), 100), DomainState::Pending);
        assert_eq!(
            DomainState::of(&row(Some(10), None), 100),
            DomainState::Verified
        );
        let failing = DomainState::of(&row(Some(10), Some(100)), 100 + GRACE_SECS - 1);
        assert_eq!(failing, DomainState::Failing { since: 100 });
        assert!(failing.counts());
        let lapsed = DomainState::of(&row(Some(10), Some(100)), 100 + GRACE_SECS);
        assert_eq!(lapsed, DomainState::Lapsed { since: 100 });
        assert!(!lapsed.counts());
        assert!(!DomainState::Pending.counts());
    }
}
