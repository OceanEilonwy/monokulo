//! What a push carries and the RFC 8030 options sent with it.

use std::fmt;
use std::str::FromStr;

use serde::Serialize;

use crate::encrypt::Encrypted;
pub use crate::encrypt::MAX_PLAINTEXT;
use crate::MAX_BODY;

/// How soon the device should be woken for a push (RFC 8030 §5.3). A push
/// service may hold back anything below `High` while the device saves
/// power; APNs and FCM map `High` to their immediate priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Urgency {
    VeryLow,
    Low,
    #[default]
    Normal,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not an urgency (very-low, low, normal or high)")]
pub struct UrgencyError;

impl Urgency {
    /// The `Urgency` header's value, also how a sender stores it.
    pub const fn header_value(self) -> &'static str {
        match self {
            Urgency::VeryLow => "very-low",
            Urgency::Low => "low",
            Urgency::Normal => "normal",
            Urgency::High => "high",
        }
    }
}

impl fmt::Display for Urgency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.header_value())
    }
}

impl FromStr for Urgency {
    type Err = UrgencyError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "very-low" => Ok(Urgency::VeryLow),
            "low" => Ok(Urgency::Low),
            "normal" => Ok(Urgency::Normal),
            "high" => Ok(Urgency::High),
            _ => Err(UrgencyError),
        }
    }
}

/// How long a push service keeps a push for a device it can't reach yet,
/// in seconds (RFC 8030 §5.2). Zero is allowed: deliver now or never.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ttl(u32);

impl Ttl {
    pub const ZERO: Ttl = Ttl(0);

    pub const fn seconds(seconds: u32) -> Self {
        Ttl(seconds)
    }

    pub const fn as_secs(self) -> u32 {
        self.0
    }
}

impl fmt::Display for Ttl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A push's topic (RFC 8030 §5.4): a push service keeps only the newest
/// undelivered push with the same topic for a subscription, so a later
/// state of something replaces an earlier one the device never saw.
///
/// 1 to 32 characters of the URL and filename safe base64 alphabet,
/// `A-Z a-z 0-9 - _`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Topic(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TopicError {
    #[error("empty")]
    Empty,
    #[error("longer than {} characters", Topic::MAX_LEN)]
    TooLong,
    #[error("has a character outside A-Z, a-z, 0-9, - and _")]
    Character,
}

impl Topic {
    pub const MAX_LEN: usize = 32;

    pub fn parse(text: &str) -> Result<Self, TopicError> {
        if text.is_empty() {
            return Err(TopicError::Empty);
        }
        if text.len() > Self::MAX_LEN {
            return Err(TopicError::TooLong);
        }
        if !text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(TopicError::Character);
        }
        Ok(Topic(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Topic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Topic {
    type Err = TopicError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Topic::parse(text)
    }
}

/// A push's body sent as JSON in the clear (`Content-Type:
/// application/json`), for a relay that has to read it: an iOS Live
/// Activity's state goes to APNs as JSON, which the device can't decrypt
/// first. At most [`MAX_BODY`] bytes.
///
/// `Debug` prints only its length: the JSON is the push's content (a
/// sale's amount, its address when the device asked for it), which a
/// relay never logs.
#[derive(Clone, PartialEq, Eq)]
pub struct PlainJson(Vec<u8>);

impl fmt::Debug for PlainJson {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PlainJson({} bytes)", self.0.len())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PlainJsonError {
    #[error("{0} bytes of JSON, more than a push may carry ({MAX_BODY})")]
    TooLarge(usize),
    #[error("not JSON: {0}")]
    Json(#[from] serde_json::Error),
}

impl PlainJson {
    /// `value` written as JSON.
    pub fn new(value: &impl Serialize) -> Result<Self, PlainJsonError> {
        let bytes = serde_json::to_vec(value)?;
        if bytes.len() > MAX_BODY {
            return Err(PlainJsonError::TooLarge(bytes.len()));
        }
        Ok(PlainJson(bytes))
    }

    /// A body as a relay receives it: refused unless it is JSON and fits.
    pub fn from_slice(body: &[u8]) -> Result<Self, PlainJsonError> {
        if body.len() > MAX_BODY {
            return Err(PlainJsonError::TooLarge(body.len()));
        }
        serde_json::from_slice::<serde::de::IgnoredAny>(body)?;
        Ok(PlainJson(body.to_vec()))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// A push's body: encrypted to the device (every push to a Web Push
/// subscription), or JSON a relay reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Encrypted(Encrypted),
    PlainJson(PlainJson),
}

impl Message {
    pub fn body(&self) -> &[u8] {
        match self {
            Message::Encrypted(encrypted) => encrypted.as_bytes(),
            Message::PlainJson(json) => json.as_bytes(),
        }
    }
}

/// What RFC 8030 lets a sender ask of the push service about one push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub urgency: Urgency,
    pub ttl: Ttl,
    pub topic: Option<Topic>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urgency_reads_and_writes_its_four_header_values() {
        for urgency in [
            Urgency::VeryLow,
            Urgency::Low,
            Urgency::Normal,
            Urgency::High,
        ] {
            assert_eq!(urgency.header_value().parse::<Urgency>(), Ok(urgency));
        }
        assert_eq!("High".parse::<Urgency>(), Err(UrgencyError));
        assert_eq!("".parse::<Urgency>(), Err(UrgencyError));
        assert_eq!(Urgency::default(), Urgency::Normal);
    }

    #[test]
    fn a_ttl_of_zero_is_a_ttl() {
        assert_eq!(Ttl::seconds(0), Ttl::ZERO);
        assert_eq!(Ttl::ZERO.to_string(), "0");
        assert_eq!(Ttl::seconds(u32::MAX).as_secs(), u32::MAX);
    }

    #[test]
    fn a_topic_is_1_to_32_url_safe_characters() {
        let longest = "aZ09-_".repeat(6)[..32].to_owned();
        assert_eq!(Topic::parse(&longest).unwrap().as_str(), longest);
        assert_eq!(Topic::parse("a").unwrap().as_str(), "a");
        assert_eq!(
            Topic::parse(&format!("{longest}a")),
            Err(TopicError::TooLong)
        );
        assert_eq!(Topic::parse(""), Err(TopicError::Empty));
        for text in ["a+b", "a/b", "a=b", "a b", "a.b", "é"] {
            assert_eq!(Topic::parse(text), Err(TopicError::Character), "{text}");
        }
    }

    #[test]
    fn plain_json_fits_in_4096_bytes_or_is_refused() {
        // `"…"`: two quotes around the string.
        let fits = "a".repeat(MAX_BODY - 2);
        assert_eq!(PlainJson::new(&fits).unwrap().as_bytes().len(), MAX_BODY);
        let over = "a".repeat(MAX_BODY - 1);
        assert!(matches!(
            PlainJson::new(&over),
            Err(PlainJsonError::TooLarge(4097))
        ));

        let body = serde_json::to_vec(&fits).unwrap();
        assert_eq!(
            PlainJson::from_slice(&body).unwrap().as_bytes(),
            body.as_slice()
        );
        let body = serde_json::to_vec(&over).unwrap();
        assert!(matches!(
            PlainJson::from_slice(&body),
            Err(PlainJsonError::TooLarge(4097))
        ));
        assert!(matches!(
            PlainJson::from_slice(b"{\"phase\":"),
            Err(PlainJsonError::Json(_))
        ));
    }

    /// The body is the push's content, which no log line gets to print.
    #[test]
    fn plain_json_debug_prints_only_its_length() {
        let json = PlainJson::new(&serde_json::json!({"amount": "12.50"})).unwrap();
        let printed = format!("{json:?}");
        assert_eq!(printed, "PlainJson(18 bytes)");
        assert_eq!(
            format!("{:?}", Message::PlainJson(json)),
            "PlainJson(PlainJson(18 bytes))"
        );
    }
}
