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
//! `.onion` addresses have no DNS, so they can't be added at all.
//!
//! This module only records what's verified; nothing restricts embedding on
//! it yet.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::db::{SharedDb, StoreDomainRow};

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
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

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
    let domain = domain.strip_prefix("*.").unwrap_or(&domain).trim_end_matches('.').to_string();
    if domain.is_empty() {
        return Err("Enter a domain, like shop.example.");
    }
    if domain.contains(':') || domain.contains('@') {
        return Err("Enter just the domain, without a port or login, like shop.example.");
    }
    if !domain.is_ascii() {
        return Err("Enter the domain in its ASCII form (the xn-- version) - international characters can't be checked.");
    }
    if domain.ends_with(".onion") {
        return Err("Onion addresses can't be verified: they have no DNS records to check.");
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
            && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    };
    if !domain.split('.').all(valid_label) {
        return Err("That isn't a valid domain name. Use letters, digits, hyphens and dots, like shop.example.");
    }
    Ok(domain)
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
            CheckOutcome::Missing => Some(format!(
                "No TXT record at {} with the value {} was found.",
                record_name(domain),
                record_value(token)
            )),
            CheckOutcome::LookupFailed(error) => Some(format!("The DNS lookup failed: {error}")),
        }
    }
}

/// The TXT values published at a name. `Ok(vec![])` means the name has no
/// TXT records; `Err` means the lookup itself failed. A trait so tests can
/// answer without a network.
pub trait TxtLookup: Send + Sync {
    fn txt_values<'a>(&'a self, name: &'a str) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + Send + 'a>>;
}

/// The real lookup, through the machine's own resolver configuration
/// (`/etc/resolv.conf` on Unix).
pub struct SystemDns {
    resolver: hickory_resolver::TokioResolver,
}

impl SystemDns {
    pub fn new() -> Result<Self, String> {
        let resolver = hickory_resolver::TokioResolver::builder_tokio()
            .and_then(|builder| builder.build())
            .map_err(|e| e.to_string())?;
        Ok(SystemDns { resolver })
    }
}

impl TxtLookup for SystemDns {
    fn txt_values<'a>(&'a self, name: &'a str) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + Send + 'a>> {
        Box::pin(async move {
            // Fully qualified, so the resolver's search domains never apply.
            let lookup = match tokio::time::timeout(LOOKUP_TIMEOUT, self.resolver.txt_lookup(format!("{name}."))).await {
                Err(_) => return Err("timed out".to_string()),
                Ok(Err(e)) if e.is_no_records_found() => return Ok(Vec::new()),
                Ok(Err(e)) => return Err(e.to_string()),
                Ok(Ok(lookup)) => lookup,
            };
            Ok(lookup
                .answers()
                .iter()
                .filter_map(|record| match &record.data {
                    // One TXT record may be split into several strings; they
                    // join into one value.
                    hickory_resolver::proto::rr::RData::TXT(txt) => {
                        Some(txt.txt_data.iter().map(|part| String::from_utf8_lossy(part)).collect::<String>())
                    }
                    _ => None,
                })
                .collect())
        })
    }
}

/// A lookup that always fails - used when the system resolver couldn't be
/// set up at startup, so the dashboard still works and says why checks fail.
pub struct UnavailableDns(pub String);

impl TxtLookup for UnavailableDns {
    fn txt_values<'a>(&'a self, _name: &'a str) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + Send + 'a>> {
        let error = self.0.clone();
        Box::pin(async move { Err(error) })
    }
}

/// Looks up `domain`'s record and compares it with `token`.
pub async fn check(dns: &dyn TxtLookup, domain: &str, token: &str) -> CheckOutcome {
    let expected = record_value(token);
    match dns.txt_values(&record_name(domain)).await {
        Ok(values) if values.iter().any(|value| value.trim() == expected) => CheckOutcome::Found,
        Ok(_) => CheckOutcome::Missing,
        Err(error) => CheckOutcome::LookupFailed(error),
    }
}

/// Checks one domain and records the result on its row.
pub async fn check_and_record(db: &SharedDb, dns: &dyn TxtLookup, row: &StoreDomainRow, now: i64) -> Result<CheckOutcome, crate::db::DbError> {
    let outcome = check(dns, &row.domain, &row.token).await;
    let error = outcome.error_message(&row.domain, &row.token);
    db.lock().unwrap().record_store_domain_check(&row.id, now, error.as_deref())?;
    Ok(outcome)
}

/// Re-checks every verified domain that is due, one at a time.
pub async fn recheck_due(db: &SharedDb, dns: &dyn TxtLookup, now: i64) {
    let due = match db.lock().unwrap().list_store_domains_due_for_recheck(now, RECHECK_EVERY_SECS, FAILING_RECHECK_EVERY_SECS) {
        Ok(due) => due,
        Err(e) => {
            eprintln!("could not list domains to re-check: {e}");
            return;
        }
    };
    for row in due {
        if let Err(e) = check_and_record(db, dns, &row, crate::now_unix()).await {
            eprintln!("could not record the re-check of {}: {e}", row.domain);
        }
    }
}

/// Runs [`recheck_due`] every few minutes for the life of the process.
pub fn spawn_rechecks(db: SharedDb, dns: Arc<dyn TxtLookup>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(5 * 60));
        loop {
            ticker.tick().await;
            recheck_due(&db, dns.as_ref(), crate::now_unix()).await;
        }
    })
}

#[cfg(test)]
pub mod test_support {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    /// Answers lookups from a table the test fills in; a name set to `Err`
    /// fails its lookup.
    #[derive(Default)]
    pub struct FakeDns {
        pub records: Mutex<HashMap<String, Result<Vec<String>, String>>>,
    }

    impl FakeDns {
        pub fn publish(&self, name: &str, value: &str) {
            self.records.lock().unwrap().insert(name.to_string(), Ok(vec![value.to_string()]));
        }
        pub fn remove(&self, name: &str) {
            self.records.lock().unwrap().remove(name);
        }
        pub fn fail(&self, name: &str, error: &str) {
            self.records.lock().unwrap().insert(name.to_string(), Err(error.to_string()));
        }
    }

    impl TxtLookup for FakeDns {
        fn txt_values<'a>(&'a self, name: &'a str) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + Send + 'a>> {
            let answer = self.records.lock().unwrap().get(name).cloned().unwrap_or(Ok(Vec::new()));
            Box::pin(async move { answer })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::FakeDns;
    use super::*;

    #[test]
    fn normalize_domain_accepts_urls_and_rejects_what_cannot_be_verified() {
        assert_eq!(normalize_domain(" https://Shop.Example/checkout?x=1 "), Ok("shop.example".to_string()));
        assert_eq!(normalize_domain("*.shop.example."), Ok("shop.example".to_string()));
        assert_eq!(normalize_domain("www.shop-outlet.example"), Ok("www.shop-outlet.example".to_string()));
        assert!(normalize_domain("abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrstuv.onion").unwrap_err().contains("Onion"));
        assert!(normalize_domain("192.0.2.1").unwrap_err().contains("IP address"));
        assert!(normalize_domain("shop.example:8443").is_err());
        assert!(normalize_domain("localhost").is_err());
        assert!(normalize_domain("-shop.example").is_err());
        assert!(normalize_domain("shop_example.com").is_err());
        assert!(normalize_domain("bücher.example").is_err());
        assert!(normalize_domain("").is_err());
    }

    #[tokio::test]
    async fn check_finds_the_token_only_at_the_right_name() {
        let dns = FakeDns::default();
        assert_eq!(check(&dns, "shop.example", "abc").await, CheckOutcome::Missing);

        dns.publish("_monokulo.shop.example", "monokulo-verify=wrong");
        assert_eq!(check(&dns, "shop.example", "abc").await, CheckOutcome::Missing);

        dns.publish("_monokulo.shop.example", "monokulo-verify=abc");
        assert_eq!(check(&dns, "shop.example", "abc").await, CheckOutcome::Found);
        assert_eq!(check(&dns, "other.example", "abc").await, CheckOutcome::Missing);

        dns.fail("_monokulo.shop.example", "timed out");
        assert_eq!(check(&dns, "shop.example", "abc").await, CheckOutcome::LookupFailed("timed out".to_string()));
    }

    fn row(verified_at: Option<i64>, failing_since: Option<i64>) -> StoreDomainRow {
        StoreDomainRow {
            id: "d1".to_string(),
            connection_id: "c1".to_string(),
            domain: "shop.example".to_string(),
            token: "abc".to_string(),
            created_at: 0,
            verified_at,
            failing_since,
            last_checked_at: None,
            last_error: None,
        }
    }

    /// Needs the network, so it only runs when asked for
    /// (`cargo test -- --ignored`).
    #[tokio::test]
    #[ignore]
    async fn the_system_resolver_reads_real_txt_records() {
        let dns = SystemDns::new().unwrap();
        let values = dns.txt_values("_dmarc.google.com").await.unwrap();
        assert!(values.iter().any(|v| v.starts_with("v=DMARC1")), "got: {values:?}");
        assert_eq!(dns.txt_values("_monokulo.example.invalid").await, Ok(vec![]));
    }

    #[test]
    fn a_missing_record_keeps_counting_through_the_grace_period_then_lapses() {
        assert_eq!(DomainState::of(&row(None, None), 100), DomainState::Pending);
        assert_eq!(DomainState::of(&row(Some(10), None), 100), DomainState::Verified);
        let failing = DomainState::of(&row(Some(10), Some(100)), 100 + GRACE_SECS - 1);
        assert_eq!(failing, DomainState::Failing { since: 100 });
        assert!(failing.counts());
        let lapsed = DomainState::of(&row(Some(10), Some(100)), 100 + GRACE_SECS);
        assert_eq!(lapsed, DomainState::Lapsed { since: 100 });
        assert!(!lapsed.counts());
        assert!(!DomainState::Pending.counts());
    }
}
