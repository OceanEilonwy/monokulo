//! What the Bell has to do with any point of sale: which one, and which of
//! its environments (an [`Integration`]), the ids a registration and a
//! webhook carry, the payment a webhook asks it to ring, and why a till's
//! token couldn't prove its location.
//!
//! Each point of sale is an adapter of its own ([`crate::square`],
//! [`crate::clover`]) behind a `match` on [`PointOfSale`], not a trait
//! object (`docs/decisions/bell.md`, decision 43): the two check their webhooks in
//! different kinds of way, and the compiler lists every place a third must
//! touch. A Bell runs an adapter for each integration that is on, several
//! environments of one point of sale at once (decision 75).

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use reqwest::StatusCode;
use serde::ser::SerializeMap as _;
use serde::{Deserialize, Serialize, Serializer};
use zeroize::Zeroizing;

use crate::settings::ServiceUrl;

/// A point of sale the Bell can serve. Its wire name, the one spelling
/// everywhere: a registration's `point_of_sale`, `/v1/info`'s keys, a
/// ding's `wake`, the problem slugs (`clover-refused`), the webhook paths
/// (`/v1/clover/…/webhook`) and the settings tables (`[bell.clover.…]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PointOfSale {
    Square,
    Clover,
}

impl PointOfSale {
    /// Every point of sale, in the order `/v1/info` lists them.
    pub const ALL: [PointOfSale; 2] = [PointOfSale::Square, PointOfSale::Clover];

    /// The wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            PointOfSale::Square => "square",
            PointOfSale::Clover => "clover",
        }
    }

    /// The name in words, for problems and log lines.
    pub const fn title(self) -> &'static str {
        match self {
            PointOfSale::Square => "Square",
            PointOfSale::Clover => "Clover",
        }
    }

    /// What a ding for one of its payments says, before it is encrypted:
    /// which point of sale to ask (decision 53).
    pub const fn wake(self) -> &'static [u8] {
        match self {
            PointOfSale::Square => br#"{"wake":"square"}"#,
            PointOfSale::Clover => br#"{"wake":"clover"}"#,
        }
    }
}

impl fmt::Display for PointOfSale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which Square an application is in: production, or the sandbox for
/// testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SquareEnvironment {
    Production,
    Sandbox,
}

impl SquareEnvironment {
    /// Every one, in the order the Bell lists them.
    pub const ALL: [SquareEnvironment; 2] =
        [SquareEnvironment::Production, SquareEnvironment::Sandbox];

    /// The wire name: a registration's `environment`, and the last part
    /// of its settings table (`[bell.square.sandbox]`).
    pub const fn as_str(self) -> &'static str {
        match self {
            SquareEnvironment::Production => "production",
            SquareEnvironment::Sandbox => "sandbox",
        }
    }

    /// The environment's API.
    pub fn api_url(self) -> ServiceUrl {
        // Both parse; the tests load each environment.
        live_settings::parsed_default(match self {
            SquareEnvironment::Production => "https://connect.squareup.com",
            SquareEnvironment::Sandbox => "https://connect.squareupsandbox.com",
        })
    }
}

/// Which Clover an app is in: a region's, or the sandbox. Each is a
/// developer dashboard and an API of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CloverEnvironment {
    NorthAmerica,
    Europe,
    LatinAmerica,
    Sandbox,
}

impl CloverEnvironment {
    /// Every one, in the order the Bell lists them.
    pub const ALL: [CloverEnvironment; 4] = [
        CloverEnvironment::NorthAmerica,
        CloverEnvironment::Europe,
        CloverEnvironment::LatinAmerica,
        CloverEnvironment::Sandbox,
    ];

    /// The wire name, as [`SquareEnvironment::as_str`]'s.
    pub const fn as_str(self) -> &'static str {
        match self {
            CloverEnvironment::NorthAmerica => "north_america",
            CloverEnvironment::Europe => "europe",
            CloverEnvironment::LatinAmerica => "latin_america",
            CloverEnvironment::Sandbox => "sandbox",
        }
    }

    /// The environment's API (the till signs in on its web host, which the
    /// Bell never visits).
    pub fn api_url(self) -> ServiceUrl {
        // Each parses; the tests load each environment.
        live_settings::parsed_default(match self {
            CloverEnvironment::NorthAmerica => "https://api.clover.com",
            CloverEnvironment::Europe => "https://api.eu.clover.com",
            CloverEnvironment::LatinAmerica => "https://api.la.clover.com",
            CloverEnvironment::Sandbox => "https://apisandbox.dev.clover.com",
        })
    }
}

/// An integration: one environment of one point of sale, with an
/// application of its own there. It is what a Bell turns on (its settings
/// table, `[bell.square.sandbox]`), takes webhooks for
/// (`/v1/square/sandbox/webhook`) and keeps registrations apart by: a
/// sandbox merchant is never a production one (decision 75).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Integration {
    Square(SquareEnvironment),
    Clover(CloverEnvironment),
}

impl Integration {
    /// Every integration, in the order the Bell lists them: Square's, then
    /// Clover's, each in its environments' order.
    pub const ALL: [Integration; 6] = [
        Integration::Square(SquareEnvironment::Production),
        Integration::Square(SquareEnvironment::Sandbox),
        Integration::Clover(CloverEnvironment::NorthAmerica),
        Integration::Clover(CloverEnvironment::Europe),
        Integration::Clover(CloverEnvironment::LatinAmerica),
        Integration::Clover(CloverEnvironment::Sandbox),
    ];

    pub const fn point_of_sale(self) -> PointOfSale {
        match self {
            Integration::Square(_) => PointOfSale::Square,
            Integration::Clover(_) => PointOfSale::Clover,
        }
    }

    /// The environment's wire name.
    pub const fn environment(self) -> &'static str {
        match self {
            Integration::Square(environment) => environment.as_str(),
            Integration::Clover(environment) => environment.as_str(),
        }
    }

    /// The integration of `pos` named `environment`, if it has one.
    pub fn of(pos: PointOfSale, environment: &str) -> Option<Integration> {
        Integration::ALL.into_iter().find(|integration| {
            integration.point_of_sale() == pos && integration.environment() == environment
        })
    }

    /// Where its webhooks come, under `bell.public_uri`.
    pub const fn webhook_path(self) -> &'static str {
        match self {
            Integration::Square(SquareEnvironment::Production) => "/v1/square/production/webhook",
            Integration::Square(SquareEnvironment::Sandbox) => "/v1/square/sandbox/webhook",
            Integration::Clover(CloverEnvironment::NorthAmerica) => {
                "/v1/clover/north_america/webhook"
            }
            Integration::Clover(CloverEnvironment::Europe) => "/v1/clover/europe/webhook",
            Integration::Clover(CloverEnvironment::LatinAmerica) => {
                "/v1/clover/latin_america/webhook"
            }
            Integration::Clover(CloverEnvironment::Sandbox) => "/v1/clover/sandbox/webhook",
        }
    }
}

/// The integration in words, for problems and log lines: `Square sandbox`,
/// `Clover north_america`.
impl fmt::Display for Integration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.point_of_sale().title(), self.environment())
    }
}

/// `integrations` in words, each point of sale once with its environments:
/// `Square production and sandbox, and Clover europe`.
pub fn in_words(integrations: impl IntoIterator<Item = Integration>) -> String {
    let integrations: Vec<Integration> = integrations.into_iter().collect();
    let groups: Vec<String> = PointOfSale::ALL
        .into_iter()
        .filter_map(|pos| {
            let environments: Vec<&str> = Integration::ALL
                .into_iter()
                .filter(|integration| {
                    integration.point_of_sale() == pos && integrations.contains(integration)
                })
                .map(Integration::environment)
                .collect();
            (!environments.is_empty()).then(|| format!("{} {}", pos.title(), listed(&environments)))
        })
        .collect();
    groups.join(", and ")
}

/// `a`, `a and b`, `a, b and c`.
fn listed(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Values by integration, as JSON nests them: `{"square": {"sandbox": …}}`,
/// in [`Integration::ALL`]'s order whatever order they come in (a map
/// would sort them), a point of sale with none left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByIntegration<V>(pub Vec<(Integration, V)>);

impl<V: Serialize> Serialize for ByIntegration<V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        /// One point of sale's environments, in order.
        struct Environments<'a, V>(Vec<&'a (Integration, V)>);

        impl<V: Serialize> Serialize for Environments<'_, V> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut map = serializer.serialize_map(Some(self.0.len()))?;
                for (integration, value) in &self.0 {
                    map.serialize_entry(integration.environment(), value)?;
                }
                map.end()
            }
        }

        let mut map = serializer.serialize_map(None)?;
        for pos in PointOfSale::ALL {
            let environments: Vec<&(Integration, V)> = Integration::ALL
                .iter()
                .filter(|integration| integration.point_of_sale() == pos)
                .filter_map(|integration| self.0.iter().find(|(i, _)| i == integration))
                .collect();
            if !environments.is_empty() {
                map.serialize_entry(pos.as_str(), &Environments(environments))?;
            }
        }
        map.end()
    }
}

/// Not a point of sale's wire name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a point of sale (square or clover)")]
pub struct UnknownPointOfSale;

impl FromStr for PointOfSale {
    type Err = UnknownPointOfSale;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        PointOfSale::ALL
            .into_iter()
            .find(|pos| pos.as_str() == text)
            .ok_or(UnknownPointOfSale)
    }
}

/// A point of sale's ids as the Bell keeps them: 1 to 64 letters, digits,
/// `-` and `_`, so one goes into a URL path as it is. Square's and Clover's
/// (13 uppercase letters and digits) both fit.
fn is_id(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

macro_rules! pos_id {
    ($(#[$attr:meta])* $name:ident, $what:literal) => {
        $(#[$attr])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
        #[serde(try_from = "String")]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = String;

            fn try_from(text: String) -> Result<Self, Self::Error> {
                if is_id(&text) {
                    Ok($name(text))
                } else {
                    Err(concat!("not a ", $what, " ID (1 to 64 letters, digits, - and _)").to_owned())
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

pos_id!(
    /// A merchant (seller account): Square's, or Clover's.
    MerchantId,
    "merchant"
);
pos_id!(
    /// One of a merchant's locations. A Clover merchant is its own one
    /// location (decision 48), so for Clover this is the merchant's id.
    LocationId,
    "location"
);

impl From<MerchantId> for LocationId {
    /// A Clover merchant as its own location.
    fn from(merchant: MerchantId) -> Self {
        LocationId(merchant.0)
    }
}

impl From<LocationId> for MerchantId {
    /// The merchant a Clover registration names as its location.
    fn from(location: LocationId) -> Self {
        MerchantId(location.0)
    }
}

/// A payment: what tells one payment's webhooks from another's. Compared,
/// never shown: it has no `Display`, so no log line names it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "String")]
pub struct PaymentId(String);

impl TryFrom<String> for PaymentId {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        if is_id(&text) {
            Ok(PaymentId(text))
        } else {
            Err("not a payment ID (1 to 64 letters, digits, - and _)".to_owned())
        }
    }
}

/// A merchant's access token, as a till hands it over to register. Used
/// for one request and dropped, wiped: `Debug` doesn't show it and nothing
/// stores or logs it.
#[derive(Deserialize)]
#[serde(try_from = "String")]
pub struct AccessToken(Zeroizing<String>);

impl AccessToken {
    /// The longest token taken; Square's are about 64 characters, Clover's
    /// fewer.
    const MAX_LEN: usize = 1024;

    /// The token, for the one request it goes into.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AccessToken {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let text = Zeroizing::new(text);
        // Visible ASCII, so it goes into a header as it is.
        if (1..=Self::MAX_LEN).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_graphic()) {
            Ok(AccessToken(text))
        } else {
            Err(format!(
                "not an access token (1 to {} visible ASCII characters)",
                Self::MAX_LEN
            ))
        }
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken(..)")
    }
}

/// One payment to ring: what a webhook of any point of sale asks for, zero
/// or more times.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ring {
    pub integration: Integration,
    pub payment_id: PaymentId,
    pub merchant_id: MerchantId,
    pub location_id: LocationId,
}

/// Why a till's token couldn't prove its location to its point of sale.
#[derive(Debug, thiserror::Error)]
pub enum ProofError {
    /// The point of sale refused the token, or the location isn't the
    /// token's merchant's: any `4xx` but `429`.
    #[error("the point of sale refused the token for that location ({0})")]
    Refused(StatusCode),
    /// The point of sale answered for another location than the one asked:
    /// the id was an alias (Square's `main`), or not the token's merchant's
    /// own (Clover). Asking again gives the same answer.
    #[error("the point of sale answered for another location")]
    NotById,
    /// The point of sale didn't answer, or answered that it couldn't
    /// (`429`, `5xx`), or answered with something that isn't what was asked.
    #[error("the point of sale couldn't be asked: {0}")]
    Unavailable(String),
}

impl ProofError {
    /// What an answer's status means, when it isn't `200`: decision 4's
    /// rule, for every point of sale.
    pub fn from_status(status: StatusCode, pos: PointOfSale) -> ProofError {
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            ProofError::Unavailable(format!("{} answered {status}", pos.title()))
        } else {
            ProofError::Refused(status)
        }
    }
}

/// A webhook body, or a point of sale's answer, that doesn't read as one.
/// The error says where it stopped, never what was there.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a body that doesn't read ({category} at line {line}, column {column})")]
pub struct BodyError {
    category: &'static str,
    line: usize,
    column: usize,
}

impl From<serde_json::Error> for BodyError {
    fn from(error: serde_json::Error) -> Self {
        // `serde_json::Error`'s message can quote the value it refused.
        BodyError {
            category: match error.classify() {
                serde_json::error::Category::Io => "unreadable",
                serde_json::error::Category::Syntax => "not JSON",
                serde_json::error::Category::Data => "not the shape expected",
                serde_json::error::Category::Eof => "cut short",
            },
            line: error.line(),
            column: error.column(),
        }
    }
}

/// A client for a point of sale's API: redirects off, so a token goes to
/// the API it was configured for and nowhere else; the Bell's `User-Agent`
/// (Clover requires one); waiting `timeout` for each answer. Fails only if
/// TLS can't be set up.
pub fn api_client(timeout: Duration) -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(crate::USER_AGENT)
        .timeout(timeout)
        .build()
}

/// A failed request, without its URL.
pub fn request_error(error: reqwest::Error) -> String {
    let error = error.without_url();
    if error.is_timeout() {
        "timed out".to_owned()
    } else if error.is_connect() {
        "could not connect".to_owned()
    } else {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_of_sale_is_its_wire_name_and_nothing_else() {
        for pos in PointOfSale::ALL {
            assert_eq!(pos.as_str().parse(), Ok(pos));
            assert_eq!(pos.to_string(), pos.as_str());
            let json = serde_json::to_string(&pos).unwrap();
            assert_eq!(json, format!("\"{}\"", pos.as_str()));
            assert_eq!(serde_json::from_str::<PointOfSale>(&json).unwrap(), pos);
            assert_eq!(
                pos.wake(),
                format!("{{\"wake\":\"{}\"}}", pos.as_str()).as_bytes()
            );
        }
        for bad in ["", "Square", "CLOVER", "square ", "toast"] {
            assert_eq!(bad.parse::<PointOfSale>(), Err(UnknownPointOfSale), "{bad}");
            assert!(serde_json::from_str::<PointOfSale>(&format!("\"{bad}\"")).is_err());
        }
    }

    /// Each integration is its point of sale's and one of its environments,
    /// named once, its webhooks at a path of its own.
    #[test]
    fn an_integration_is_a_point_of_sale_and_an_environment() {
        for integration in Integration::ALL {
            let (pos, environment) = (integration.point_of_sale(), integration.environment());
            assert_eq!(Integration::of(pos, environment), Some(integration));
            assert_eq!(
                integration.webhook_path(),
                format!("/v1/{pos}/{environment}/webhook")
            );
        }
        assert_eq!(
            Integration::ALL
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            [
                "Square production",
                "Square sandbox",
                "Clover north_america",
                "Clover europe",
                "Clover latin_america",
                "Clover sandbox",
            ]
        );
        // A name is its own point of sale's alone, as written.
        assert_eq!(Integration::of(PointOfSale::Square, "europe"), None);
        assert_eq!(Integration::of(PointOfSale::Clover, "production"), None);
        assert_eq!(Integration::of(PointOfSale::Square, "Production"), None);
    }

    #[test]
    fn integrations_are_said_a_point_of_sale_at_a_time() {
        assert_eq!(
            in_words([
                Integration::Clover(CloverEnvironment::Europe),
                Integration::Square(SquareEnvironment::Sandbox),
                Integration::Square(SquareEnvironment::Production),
            ]),
            "Square production and sandbox, and Clover europe"
        );
        assert_eq!(
            in_words([
                Integration::Clover(CloverEnvironment::Sandbox),
                Integration::Clover(CloverEnvironment::NorthAmerica),
                Integration::Clover(CloverEnvironment::LatinAmerica),
            ]),
            "Clover north_america, latin_america and sandbox"
        );
        assert_eq!(in_words([]), "");
    }

    /// Nested by point of sale, in the fixed order whatever order the
    /// values come in, a point of sale with none left out.
    #[test]
    fn values_by_integration_nest_in_a_fixed_order() {
        let values = ByIntegration(vec![
            (Integration::Clover(CloverEnvironment::Sandbox), 4),
            (Integration::Square(SquareEnvironment::Sandbox), 2),
            (Integration::Clover(CloverEnvironment::NorthAmerica), 3),
        ]);
        assert_eq!(
            serde_json::to_string(&values).unwrap(),
            r#"{"square":{"sandbox":2},"clover":{"north_america":3,"sandbox":4}}"#
        );
        let clover = ByIntegration(vec![(Integration::Clover(CloverEnvironment::Europe), 1)]);
        assert_eq!(
            serde_json::to_string(&clover).unwrap(),
            r#"{"clover":{"europe":1}}"#
        );
        assert_eq!(
            serde_json::to_string(&ByIntegration::<u8>(Vec::new())).unwrap(),
            "{}"
        );
    }

    #[test]
    fn ids_and_tokens_take_only_what_fits_a_url_and_a_header() {
        assert!(LocationId::try_from("L8GWD5R9QB376".to_owned()).is_ok());
        for bad in ["", "a/b", "a b", "..%2f", &"L".repeat(65)] {
            assert!(LocationId::try_from(bad.to_owned()).is_err(), "{bad}");
        }
        assert!(AccessToken::try_from("EAAAl-token_123".to_owned()).is_ok());
        for bad in ["", "a b", "a\nb", &"t".repeat(1025)] {
            assert!(AccessToken::try_from(bad.to_owned()).is_err(), "{bad:?}");
        }
        let token = AccessToken::try_from("EAAAsecret".to_owned()).unwrap();
        assert_eq!(format!("{token:?}"), "AccessToken(..)");
    }

    #[test]
    fn a_status_is_a_refusal_unless_the_point_of_sale_couldnt_answer() {
        for code in [400, 401, 403, 404, 409, 307] {
            let status = StatusCode::from_u16(code).unwrap();
            assert!(
                matches!(ProofError::from_status(status, PointOfSale::Clover), ProofError::Refused(s) if s == status),
                "{code}"
            );
        }
        for code in [429, 500, 502, 503] {
            let status = StatusCode::from_u16(code).unwrap();
            assert!(
                matches!(
                    ProofError::from_status(status, PointOfSale::Clover),
                    ProofError::Unavailable(_)
                ),
                "{code}"
            );
        }
    }
}
