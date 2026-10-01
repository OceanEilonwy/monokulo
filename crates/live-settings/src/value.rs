//! How each kind of value parses, prints and describes itself.
//!
//! Every rule about what a value may look like lives on its type, so the
//! save path, the boot path and the admin page's control all read the same
//! rule and can't disagree.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::Serialize;

/// What kind of control the admin page shows for a setting, and the limits
/// it can check in the browser before submitting. Serialised as
/// `{"type": "integer", "min": 1, "max": 10000}` and so on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SettingKind {
    /// A whole number. `min`/`max` combine the type's own limits with the
    /// setting's `range(..)`, when it has one.
    Integer {
        min: Option<i64>,
        max: Option<i64>,
    },
    Bool,
    /// Exactly one of `choices`.
    Choice {
        choices: Vec<&'static str>,
    },
    /// Any number of `choices`, comma separated.
    ChoiceList {
        choices: Vec<&'static str>,
    },
    /// An absolute `http`/`https` URL.
    Url,
    /// An IP address and port.
    Address,
    /// A filesystem path.
    Path,
    Text,
    /// Never shown back in full.
    Secret,
    /// A JSON document.
    Json,
}

/// A type a setting can hold.
///
/// `parse` is the only way in, so a value that exists has passed its
/// type's rules. `to_stored` is what gets written to the store and must
/// parse back to an equal value; `render` is what people see, and differs
/// from `to_stored` only for [`Secret`].
pub trait SettingValue: Sized + Clone + PartialEq + Send + Sync + 'static {
    /// Parses a raw value from the environment, the store or a form. The
    /// error is shown next to the field, so it should say what to enter.
    fn parse(raw: &str) -> Result<Self, String>;

    /// The value as shown on the admin page and in logs. Masked for
    /// secrets.
    fn render(&self) -> String;

    /// The control the admin page shows for this type.
    fn kind() -> SettingKind;

    /// The value as written to the store. `parse(&v.to_stored()) == Ok(v)`.
    fn to_stored(&self) -> String {
        self.render()
    }

    /// The value as a number, for types a `range(min, max)` can apply to.
    fn as_integer(&self) -> Option<i128> {
        None
    }
}

macro_rules! integer_value {
    ($($t:ty => $min:expr, $max:expr;)*) => {$(
        impl SettingValue for $t {
            fn parse(raw: &str) -> Result<Self, String> {
                raw.trim().parse::<$t>().map_err(|_| whole_number_message($min, $max))
            }

            fn render(&self) -> String {
                self.to_string()
            }

            fn kind() -> SettingKind {
                SettingKind::Integer { min: $min, max: $max }
            }

            fn as_integer(&self) -> Option<i128> {
                Some(*self as i128)
            }
        }
    )*};
}

integer_value! {
    u16 => Some(0), Some(i64::from(u16::MAX));
    u32 => Some(0), Some(i64::from(u32::MAX));
    u64 => Some(0), None;
    usize => Some(0), None;
    i64 => None, None;
}

/// The message for a number that doesn't parse, or falls outside a range.
pub(crate) fn whole_number_message(min: Option<i64>, max: Option<i64>) -> String {
    match (min, max) {
        (Some(min), Some(max)) => format!("Enter a whole number from {min} to {max}."),
        (Some(min), None) => format!("Enter a whole number, {min} or more."),
        (None, Some(max)) => format!("Enter a whole number, {max} or less."),
        (None, None) => "Enter a whole number.".to_string(),
    }
}

impl SettingValue for bool {
    fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.eq_ignore_ascii_case("true") {
            Ok(true)
        } else if raw.eq_ignore_ascii_case("false") {
            Ok(false)
        } else {
            Err("Enter true or false.".to_string())
        }
    }

    fn render(&self) -> String {
        self.to_string()
    }

    fn kind() -> SettingKind {
        SettingKind::Bool
    }
}

/// Free text. Surrounding whitespace is dropped, since it only ever arrives
/// by accident from a form or an environment file.
impl SettingValue for String {
    fn parse(raw: &str) -> Result<Self, String> {
        Ok(raw.trim().to_string())
    }

    fn render(&self) -> String {
        self.clone()
    }

    fn kind() -> SettingKind {
        SettingKind::Text
    }
}

/// A filesystem path. Use `Option<PathBuf>` when "not set" is allowed.
impl SettingValue for PathBuf {
    fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("Enter a path.".to_string());
        }
        Ok(PathBuf::from(raw))
    }

    fn render(&self) -> String {
        self.to_string_lossy().into_owned()
    }

    fn kind() -> SettingKind {
        SettingKind::Path
    }
}

/// An optional value: an empty string means "not set". Several settings
/// default to unset (`public_url`, `key_custody.socket_path`), and this
/// keeps that meaning in the type instead of in every reader.
impl<T: SettingValue> SettingValue for Option<T> {
    fn parse(raw: &str) -> Result<Self, String> {
        if raw.trim().is_empty() {
            Ok(None)
        } else {
            T::parse(raw).map(Some)
        }
    }

    fn render(&self) -> String {
        self.as_ref().map(T::render).unwrap_or_default()
    }

    fn kind() -> SettingKind {
        T::kind()
    }

    fn to_stored(&self) -> String {
        self.as_ref().map(T::to_stored).unwrap_or_default()
    }

    fn as_integer(&self) -> Option<i128> {
        self.as_ref().and_then(T::as_integer)
    }
}

/// A value that must never be shown back in full or logged: an admin token,
/// for instance. `render` and `Debug` mask it; only [`Secret::expose`] and
/// the stored form carry the real value.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// The real value, for the one place that has to use it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// The value is scrubbed from memory when the secret is dropped.
impl Drop for Secret {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

/// What the admin page shows for a secret that is set. Always the same
/// length, so it doesn't give away the real one.
pub const MASK: &str = "********";

impl SettingValue for Secret {
    fn parse(raw: &str) -> Result<Self, String> {
        Ok(Secret(raw.trim().to_string()))
    }

    fn render(&self) -> String {
        if self.0.is_empty() {
            String::new()
        } else {
            MASK.to_string()
        }
    }

    fn kind() -> SettingKind {
        SettingKind::Secret
    }

    fn to_stored(&self) -> String {
        self.0.clone()
    }
}

/// An address and port to listen on, like `127.0.0.1:8443`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindAddr(pub SocketAddr);

impl SettingValue for BindAddr {
    fn parse(raw: &str) -> Result<Self, String> {
        raw.trim().parse().map(BindAddr).map_err(|_| {
            "Enter an IP address and port, like 127.0.0.1:8443 or [::1]:8443.".to_string()
        })
    }

    fn render(&self) -> String {
        self.0.to_string()
    }

    fn kind() -> SettingKind {
        SettingKind::Address
    }
}

/// An absolute `http` or `https` URL with a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpUrl(url::Url);

impl HttpUrl {
    pub fn url(&self) -> &url::Url {
        &self.0
    }

    /// The URL without the `/` that `url` adds to a bare host, so callers
    /// can append paths like `/api/v1` without doubling the slash.
    pub fn as_str(&self) -> &str {
        let s = self.0.as_str();
        if self.0.path() == "/" && self.0.query().is_none() && self.0.fragment().is_none() {
            s.strip_suffix('/').unwrap_or(s)
        } else {
            s
        }
    }
}

impl SettingValue for HttpUrl {
    fn parse(raw: &str) -> Result<Self, String> {
        let problem = || {
            "Enter a full web address starting with http:// or https://, like https://example.com."
                .to_string()
        };
        let parsed = url::Url::parse(raw.trim()).map_err(|_| problem())?;
        let has_host = parsed.host_str().is_some_and(|host| !host.is_empty());
        if !matches!(parsed.scheme(), "http" | "https") || !has_host {
            return Err(problem());
        }
        // A URL setting is shown on the admin page and logged: a user name
        // or password in it would be too. Credentials belong in a secret
        // setting.
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err("Leave the user name and password out of the address; put credentials in their own setting.".to_string());
        }
        Ok(HttpUrl(parsed))
    }

    fn render(&self) -> String {
        self.as_str().to_string()
    }

    fn kind() -> SettingKind {
        SettingKind::Url
    }
}

/// A comma-separated list. Blank items are skipped, so `a, , b` and a
/// trailing comma are both fine; an empty string is an empty list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommaList<T>(pub Vec<T>);

impl<T: SettingValue> SettingValue for CommaList<T> {
    fn parse(raw: &str) -> Result<Self, String> {
        raw.split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(|item| T::parse(item).map_err(|e| format!("{item:?}: {e}")))
            .collect::<Result<Vec<_>, _>>()
            .map(CommaList)
    }

    fn render(&self) -> String {
        self.0.iter().map(T::render).collect::<Vec<_>>().join(", ")
    }

    fn kind() -> SettingKind {
        match T::kind() {
            SettingKind::Choice { choices } => SettingKind::ChoiceList { choices },
            _ => SettingKind::Text,
        }
    }

    fn to_stored(&self) -> String {
        self.0
            .iter()
            .map(T::to_stored)
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// A structured value stored as JSON, such as one network's Monero nodes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Json<T>(pub T);

impl<T> SettingValue for Json<T>
where
    T: Serialize + DeserializeOwned + Clone + PartialEq + Send + Sync + 'static,
{
    fn parse(raw: &str) -> Result<Self, String> {
        let value: T = serde_json::from_str(raw)
            .map_err(|e| format!("This isn't valid for this setting: {e}."))?;
        // Checked here, so `render` can't fail later on a value that exists.
        serde_json::to_string(&value).map_err(|e| format!("This value can't be saved: {e}."))?;
        Ok(Json(value))
    }

    fn render(&self) -> String {
        // `parse` already proved this value serialises.
        serde_json::to_string(&self.0).unwrap_or_default()
    }

    fn kind() -> SettingKind {
        SettingKind::Json
    }
}

/// Declares a choice setting's value type: a plain enum whose variants map to
/// fixed strings, shown on the admin page as a select.
///
/// ```
/// live_settings::choice_value! {
///     pub enum Mode { Public = "public", InviteOnly = "invite_only" }
/// }
/// use live_settings::SettingValue;
/// assert_eq!(Mode::parse("public"), Ok(Mode::Public));
/// assert!(Mode::parse("nope").is_err());
/// ```
#[macro_export]
macro_rules! choice_value {
    ($(#[$attr:meta])* $vis:vis enum $name:ident { $($variant:ident = $text:literal),+ $(,)? }) => {
        $(#[$attr])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        $vis enum $name { $($variant),+ }

        impl $name {
            pub const CHOICES: &'static [&'static str] = &[$($text),+];
            pub fn as_str(&self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
        }

        impl $crate::SettingValue for $name {
            fn parse(raw: &str) -> ::core::result::Result<Self, ::std::string::String> {
                match raw.trim() {
                    $($text => Ok(Self::$variant),)+
                    other => Err(format!("Choose one of: {} (got {:?}).", Self::CHOICES.join(", "), other)),
                }
            }
            fn render(&self) -> ::std::string::String {
                self.as_str().to_string()
            }
            fn kind() -> $crate::SettingKind {
                $crate::SettingKind::Choice { choices: Self::CHOICES.to_vec() }
            }
        }
    };
}
