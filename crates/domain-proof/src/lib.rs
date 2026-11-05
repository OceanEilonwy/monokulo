//! Proving a merchant controls a domain.
//!
//! A web domain proves it with a TXT record: hacking a website rarely gives
//! control of its domain's DNS, so a hacker can't prove a site they merely
//! broke into. An onion address has no DNS, so a shop on one serves a small
//! file instead, which is fetched over Tor and never over the web
//! ([`WellKnownFetch`]). That is weaker than DNS, since whoever can change
//! the shop's files can publish it, but it is the strongest proof an onion
//! shop can give, and only the shop's own key can serve its address.
//!
//! What is being proved is a [`Scheme`]: where the proof lives and what it
//! starts with. Everything else here is the same for each scheme, so no two
//! of them can drift apart, or leak an onion name to DNS. Which domains a
//! store has, and what proving one allows, is the caller's.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// How long a proof that has gone missing keeps counting.
pub const GRACE_SECS: i64 = 3 * 24 * 60 * 60;
/// How often a proof is checked again.
pub const RECHECK_EVERY_SECS: i64 = 24 * 60 * 60;
/// How often a proof is checked again while it's failing, or while the last
/// check couldn't get an answer, so a fix is noticed well within the grace
/// period without the merchant clicking anything.
pub const FAILING_RECHECK_EVERY_SECS: i64 = 60 * 60;
/// The shortest gap between two checks of one domain the merchant can ask for.
pub const MIN_CHECK_GAP_SECS: i64 = 30;
/// The largest proof file read: a few values, one per line.
pub const WELL_KNOWN_MAX_BYTES: usize = 4096;
/// A whole lookup: referrals, and the machine's resolvers if the root
/// servers can't be reached.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(20);

/// What is being proved: where its proof lives, and what the proof's value
/// starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scheme {
    /// The label the TXT record lives under: `<label>.shop.example`. Its
    /// own name, so it doesn't crowd the domain's main TXT records.
    pub label: &'static str,
    /// What the proof's value starts with, followed by the token.
    pub value_prefix: &'static str,
    /// Where an onion shop serves the file.
    pub well_known_path: &'static str,
}

impl Scheme {
    /// Where the TXT record for `domain` lives.
    pub fn record_name(&self, domain: &str) -> String {
        format!("{}.{domain}", self.label)
    }

    /// The proof's value for `token`.
    pub fn value(&self, token: &str) -> String {
        format!("{}{token}", self.value_prefix)
    }

    /// Where an onion shop serves the file.
    pub fn well_known_url(&self, domain: &str) -> String {
        format!("http://{domain}{}", self.well_known_path)
    }
}

/// Proving a store owns a domain.
pub const VERIFY: Scheme = Scheme {
    label: "_monokulo",
    value_prefix: "monokulo-verify=",
    well_known_path: "/.well-known/monokulo-verify",
};

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
    if outbound::is_onion_host(&domain) {
        // The service itself: its subdomains are the same service, and
        // covered anyway.
        return tor_service::onion::v3_host(&domain)
            .map(|host| tor_service::onion::service_of(&host).to_string())
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

/// Whether `domain` is an onion address, proved by its file rather than DNS.
pub fn is_onion(domain: &str) -> bool {
    outbound::is_onion_host(domain)
}

/// Where a proved domain stands at some moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainState {
    /// Never proved yet: waiting for the merchant to publish it.
    Pending,
    /// Proved, and no answer since has lacked the proof.
    Verified,
    /// Proved, but its proof has been missing since `since`; still counts
    /// until `since + GRACE_SECS`.
    Failing { since: i64 },
    /// Its proof has been missing for longer than the grace period, so it
    /// no longer counts until a check finds it again.
    Lapsed { since: i64 },
}

impl DomainState {
    /// Where a domain stands at `now`, from when it was last proved and
    /// since when its proof has been missing.
    pub fn of(verified_at: Option<i64>, failing_since: Option<i64>, now: i64) -> Self {
        match (verified_at, failing_since) {
            (None, _) => DomainState::Pending,
            (Some(_), None) => DomainState::Verified,
            (Some(_), Some(since)) if now - since < GRACE_SECS => DomainState::Failing { since },
            (Some(_), Some(since)) => DomainState::Lapsed { since },
        }
    }

    /// Whether the domain currently counts as proved.
    pub fn counts(self) -> bool {
        matches!(self, DomainState::Verified | DomainState::Failing { .. })
    }
}

/// What one lookup of a domain's proof found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// The proof is there, holding the right token.
    Found,
    /// An answer, with no proof of this scheme at all.
    Missing,
    /// An answer with this scheme's proof holding another token: an old
    /// one, or a typo. Said apart from [`CheckOutcome::Missing`], since only
    /// the token is wrong, and counts the same as it.
    OtherValue,
    /// No answer (timeout, resolver or onion service unreachable). Says
    /// nothing about the proof, so it must never count against it.
    LookupFailed(String),
}

impl CheckOutcome {
    /// The reason shown to the merchant for a check that didn't find the
    /// proof, or `None` for one that did.
    pub fn error_message(&self, scheme: &Scheme, domain: &str, token: &str) -> Option<String> {
        let expected = scheme.value(token);
        match self {
            CheckOutcome::Found => None,
            CheckOutcome::Missing if is_onion(domain) => Some(format!(
                "The shop answered, but {} doesn't hold {expected}.",
                scheme.well_known_url(domain),
            )),
            CheckOutcome::Missing => Some(format!(
                "No TXT record at {} with the value {expected} was found.",
                scheme.record_name(domain),
            )),
            CheckOutcome::OtherValue if is_onion(domain) => Some(format!(
                "The shop answered, but {} holds a different value. It should hold {expected}.",
                scheme.well_known_url(domain),
            )),
            CheckOutcome::OtherValue => Some(format!(
                "The TXT record at {} holds a different value. It should be {expected}.",
                scheme.record_name(domain),
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

/// The real lookup ([`dns_client`]): from the root servers down to the
/// domain's own name servers, so a record is seen the moment it's published,
/// with no resolver's cache still saying there is no such name.
pub struct LiveDns(dns_client::Resolver);

impl LiveDns {
    pub fn new() -> Self {
        LiveDns(dns_client::Resolver::system())
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

/// An onion shop's proof file, fetched over Tor: `Ok(Some(text))` when the
/// shop served it, `Ok(None)` when it answered without it (not found, a
/// redirect, too big), `Err` when it couldn't be reached. A trait so tests
/// answer without Tor.
pub trait WellKnownFetch: Send + Sync {
    fn fetch<'a>(
        &'a self,
        domain: &'a str,
        path: &'static str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<String>, String>> + Send + 'a>>;
}

/// A fetch that always fails, saying why: for a server without Tor inside
/// it, and tests that don't need one.
pub struct UnavailableTor(pub String);

impl WellKnownFetch for UnavailableTor {
    fn fetch<'a>(
        &'a self,
        _domain: &'a str,
        _path: &'static str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<String>, String>> + Send + 'a>> {
        let error = self.0.clone();
        Box::pin(async move { Err(error) })
    }
}

/// Fetches onion shops' proof files over Tor inside monokulo
/// ([`tor_service::Tor::fetch_over_tor`]), never over the web: only while
/// it runs.
pub struct WellKnownOverTor(pub tor_service::Tor);

impl WellKnownFetch for WellKnownOverTor {
    fn fetch<'a>(
        &'a self,
        domain: &'a str,
        path: &'static str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<String>, String>> + Send + 'a>> {
        Box::pin(self.0.fetch_over_tor(domain, path, WELL_KNOWN_MAX_BYTES))
    }
}

/// How a domain is proved: DNS for a web domain, the file over Tor for an
/// onion one.
#[derive(Clone)]
pub struct Proofs {
    pub dns: Arc<dyn TxtLookup>,
    pub onion: Arc<dyn WellKnownFetch>,
}

/// Looks up `domain`'s proof of `scheme` (its TXT record, or an onion shop's
/// file) and compares it with `token`.
pub async fn check(proofs: &Proofs, scheme: &Scheme, domain: &str, token: &str) -> CheckOutcome {
    let expected = scheme.value(token);
    let found = |values: &mut dyn Iterator<Item = &str>| {
        let mut other = false;
        for value in values {
            let value = value.trim();
            if value == expected {
                return CheckOutcome::Found;
            }
            other |= value.starts_with(scheme.value_prefix);
        }
        if other {
            CheckOutcome::OtherValue
        } else {
            CheckOutcome::Missing
        }
    };
    if is_onion(domain) {
        return match proofs.onion.fetch(domain, scheme.well_known_path).await {
            Ok(Some(text)) => found(&mut text.lines()),
            Ok(None) => CheckOutcome::Missing,
            Err(error) => CheckOutcome::LookupFailed(error),
        };
    }
    match proofs.dns.txt_values(&scheme.record_name(domain)).await {
        Ok(values) => found(&mut values.iter().map(String::as_str)),
        Err(error) => CheckOutcome::LookupFailed(error),
    }
}

/// Fakes that answer from a table, for tests.
#[cfg(feature = "test-support")]
pub mod test_support {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    /// Answers lookups from a table the test fills in; a name set to `Err`
    /// fails its lookup. Every name asked for is kept in `asked`, so a test
    /// can prove what never reached DNS.
    #[derive(Default)]
    pub struct FakeDns {
        pub records: Mutex<HashMap<String, Result<Vec<String>, String>>>,
        pub asked: Mutex<Vec<String>>,
    }

    impl FakeDns {
        pub fn publish(&self, name: &str, value: &str) {
            self.records
                .lock()
                .unwrap()
                .insert(name.to_string(), Ok(vec![value.to_string()]));
        }
        pub fn remove(&self, name: &str) {
            self.records.lock().unwrap().remove(name);
        }
        pub fn fail(&self, name: &str, error: &str) {
            self.records
                .lock()
                .unwrap()
                .insert(name.to_string(), Err(error.to_string()));
        }
    }

    impl TxtLookup for FakeDns {
        fn txt_values<'a>(
            &'a self,
            name: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + Send + 'a>> {
            self.asked.lock().unwrap().push(name.to_string());
            let answer = self
                .records
                .lock()
                .unwrap()
                .get(name)
                .cloned()
                .unwrap_or(Ok(Vec::new()));
            Box::pin(async move { answer })
        }
    }

    /// Answers onion shops' proof files from a table the test fills in; a
    /// shop set to `Err` can't be reached, one not in it answers without the
    /// file. Each shop has one file, whatever the path; `asked` keeps the
    /// paths asked for.
    #[derive(Default)]
    pub struct FakeOnion {
        pub files: Mutex<HashMap<String, Result<Option<String>, String>>>,
        pub asked: Mutex<Vec<&'static str>>,
    }

    impl FakeOnion {
        pub fn serve(&self, domain: &str, text: &str) {
            self.files
                .lock()
                .unwrap()
                .insert(domain.to_string(), Ok(Some(text.to_string())));
        }
        pub fn fail(&self, domain: &str, error: &str) {
            self.files
                .lock()
                .unwrap()
                .insert(domain.to_string(), Err(error.to_string()));
        }
    }

    impl WellKnownFetch for FakeOnion {
        fn fetch<'a>(
            &'a self,
            domain: &'a str,
            path: &'static str,
        ) -> Pin<Box<dyn Future<Output = Result<Option<String>, String>> + Send + 'a>> {
            self.asked.lock().unwrap().push(path);
            let answer = self
                .files
                .lock()
                .unwrap()
                .get(domain)
                .cloned()
                .unwrap_or(Ok(None));
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
    use super::test_support::{proofs, FakeDns, FakeOnion};
    use super::*;

    const ONION: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";

    /// A second scheme, so nothing here can only work for [`VERIFY`].
    const OTHER: Scheme = Scheme {
        label: "_other",
        value_prefix: "other=",
        well_known_path: "/.well-known/other",
    };

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
        let proofs = proofs(dns.clone(), onion.clone());
        // A TXT record means nothing for an onion shop, and is never asked for.
        dns.publish(&VERIFY.record_name(ONION), "monokulo-verify=abc");
        assert_eq!(
            check(&proofs, &VERIFY, ONION, "abc").await,
            CheckOutcome::Missing
        );
        // One store's line among others'.
        onion.serve(ONION, "monokulo-verify=other\r\n  monokulo-verify=abc  \n");
        assert_eq!(
            check(&proofs, &VERIFY, ONION, "abc").await,
            CheckOutcome::Found
        );
        onion.fail(ONION, "the onion service couldn't be found");
        let failed = check(&proofs, &VERIFY, ONION, "abc").await;
        assert_eq!(
            failed.error_message(&VERIFY, ONION, "abc").unwrap(),
            "Fetching the file over Tor failed: the onion service couldn't be found"
        );
        assert!(dns.asked.lock().unwrap().is_empty(), "no onion name in DNS");
    }

    #[tokio::test]
    async fn each_answer_gets_its_own_outcome_for_a_web_domain() {
        let fake = Arc::new(FakeDns::default());
        let dns = proofs(fake.clone(), Arc::default());
        let name = "_monokulo.shop.example";
        let outcome = || check(&dns, &VERIFY, "shop.example", "abc");

        assert_eq!(outcome().await, CheckOutcome::Missing);
        fake.publish(name, "google-site-verification=abc");
        assert_eq!(outcome().await, CheckOutcome::Missing);
        fake.publish(name, "monokulo-verify=wrong");
        assert_eq!(outcome().await, CheckOutcome::OtherValue);
        fake.publish(name, "monokulo-verify=abc");
        assert_eq!(outcome().await, CheckOutcome::Found);
        assert_eq!(
            check(&dns, &VERIFY, "other.example", "abc").await,
            CheckOutcome::Missing
        );
        fake.fail(name, "timed out");
        assert_eq!(
            outcome().await,
            CheckOutcome::LookupFailed("timed out".to_string())
        );
    }

    #[tokio::test]
    async fn the_right_token_among_other_values_is_found() {
        let fake = Arc::new(FakeDns::default());
        fake.records.lock().unwrap().insert(
            "_monokulo.shop.example".to_string(),
            Ok(vec![
                "monokulo-verify=old".to_string(),
                " monokulo-verify=abc ".to_string(),
            ]),
        );
        let dns = proofs(fake, Arc::default());
        assert_eq!(
            check(&dns, &VERIFY, "shop.example", "abc").await,
            CheckOutcome::Found
        );
    }

    #[tokio::test]
    async fn an_onion_shop_with_another_token_is_told_so() {
        let onion = Arc::new(FakeOnion::default());
        let proofs = proofs(Arc::default(), onion.clone());
        onion.serve(ONION, "monokulo-verify=old\n");
        let outcome = check(&proofs, &VERIFY, ONION, "abc").await;
        assert_eq!(outcome, CheckOutcome::OtherValue);
        assert_eq!(
            outcome.error_message(&VERIFY, ONION, "abc").unwrap(),
            format!(
                "The shop answered, but http://{ONION}/.well-known/monokulo-verify holds a different value. It should hold monokulo-verify=abc."
            )
        );
    }

    #[tokio::test]
    async fn a_scheme_looks_in_its_own_places_and_ignores_another_schemes_proof() {
        let (dns, onion) = (Arc::new(FakeDns::default()), Arc::new(FakeOnion::default()));
        let proofs = proofs(dns.clone(), onion.clone());
        dns.publish("_other.shop.example", "monokulo-verify=abc");
        assert_eq!(
            check(&proofs, &OTHER, "shop.example", "abc").await,
            CheckOutcome::Missing,
            "the verify scheme's value proves nothing to another scheme"
        );
        dns.publish("_other.shop.example", "other=abc");
        assert_eq!(
            check(&proofs, &OTHER, "shop.example", "abc").await,
            CheckOutcome::Found
        );
        assert_eq!(
            check(&proofs, &VERIFY, "shop.example", "abc").await,
            CheckOutcome::Missing,
            "nor the other way around: it's at another name"
        );
        check(&proofs, &OTHER, ONION, "abc").await;
        assert_eq!(*onion.asked.lock().unwrap(), ["/.well-known/other"]);
    }

    #[test]
    fn the_reason_names_the_place_and_the_value_expected() {
        let reason = |outcome: CheckOutcome, domain| outcome.error_message(&VERIFY, domain, "abc");
        assert_eq!(reason(CheckOutcome::Found, "shop.example"), None);
        assert_eq!(
            reason(CheckOutcome::Missing, "shop.example").unwrap(),
            "No TXT record at _monokulo.shop.example with the value monokulo-verify=abc was found."
        );
        assert_eq!(
            reason(CheckOutcome::OtherValue, "shop.example").unwrap(),
            "The TXT record at _monokulo.shop.example holds a different value. It should be monokulo-verify=abc."
        );
        assert_eq!(
            reason(
                CheckOutcome::LookupFailed("timed out".into()),
                "shop.example"
            )
            .unwrap(),
            "The DNS lookup failed: timed out"
        );
        assert_eq!(
            reason(CheckOutcome::Missing, ONION).unwrap(),
            format!("The shop answered, but http://{ONION}/.well-known/monokulo-verify doesn't hold monokulo-verify=abc.")
        );
    }

    #[test]
    fn a_missing_proof_keeps_counting_through_the_grace_period_then_lapses() {
        assert_eq!(DomainState::of(None, None, 100), DomainState::Pending);
        assert_eq!(DomainState::of(Some(10), None, 100), DomainState::Verified);
        let failing = DomainState::of(Some(10), Some(100), 100 + GRACE_SECS - 1);
        assert_eq!(failing, DomainState::Failing { since: 100 });
        assert!(failing.counts());
        let lapsed = DomainState::of(Some(10), Some(100), 100 + GRACE_SECS);
        assert_eq!(lapsed, DomainState::Lapsed { since: 100 });
        assert!(!lapsed.counts());
        assert!(!DomainState::Pending.counts());
    }
}
