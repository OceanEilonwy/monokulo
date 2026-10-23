//! The values the API takes, as types (`#[api::scalar]`): each checks
//! itself when it's read, so a body that holds one has a valid one, and
//! describes itself (pattern, lengths, an example) in the schema the forms
//! and the OpenAPI document show.

use std::sync::OnceLock;

use schemars::Schema;
use serde_json::{json, Map, Value};

use crate::api;

/// A scalar's rules, as `#[api::scalar]` writes them.
pub struct ScalarRules {
    pub name: &'static str,
    pub doc: &'static str,
    pub pattern: Option<&'static str>,
    pub example: &'static str,
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    /// Trim surrounding whitespace before checking (and keep it trimmed).
    pub trim: bool,
    pub regex: OnceLock<regex::Regex>,
}

impl ScalarRules {
    /// `value` (trimmed, if the scalar trims), if it follows the rules;
    /// otherwise what's wrong, for `detail`.
    pub fn check(&self, value: &str) -> Result<String, String> {
        let value = if self.trim { value.trim() } else { value };
        let chars = value.chars().count();
        if let Some(min) = self.min_length {
            if chars < min {
                return Err(format!("{} must be at least {min} characters", self.name));
            }
        }
        if let Some(max) = self.max_length {
            if chars > max {
                return Err(format!("{} must be at most {max} characters", self.name));
            }
        }
        if let Some(pattern) = self.pattern {
            let regex = self
                .regex
                .get_or_init(|| regex::Regex::new(pattern).expect("a scalar's pattern compiles"));
            if !regex.is_match(value) {
                return Err(format!(
                    "{} must match {pattern}, like {}",
                    self.name, self.example
                ));
            }
        }
        Ok(value.to_owned())
    }

    /// Its JSON Schema.
    pub fn schema(&self) -> Schema {
        let mut schema = Map::new();
        schema.insert("type".into(), "string".into());
        schema.insert("description".into(), self.doc.into());
        if let Some(pattern) = self.pattern {
            schema.insert("pattern".into(), pattern.into());
        }
        if let Some(min) = self.min_length {
            schema.insert("minLength".into(), json!(min));
        }
        if let Some(max) = self.max_length {
            schema.insert("maxLength".into(), json!(max));
        }
        schema.insert("examples".into(), json!([self.example]));
        Schema::try_from(Value::Object(schema)).expect("a string schema")
    }
}

/// A price: a decimal number in its currency, with at most 12 decimals
/// (XMR's own precision; a fiat currency takes at most 2).
#[api::scalar(
    pattern = r"^[0-9]+(\.[0-9]{1,12})?$",
    example = "12.50",
    max_length = 40,
    trim
)]
pub struct Amount(String);

/// A currency: `XMR`, or a currency this store's exchange rate providers
/// can price (`EUR`, `USD`, ...).
#[api::scalar(pattern = r"^[A-Za-z0-9]{2,10}$", example = "EUR", trim)]
pub struct CurrencyCode(String);

/// The shop's own reference for an order (its order or cart number), shown
/// on the dashboard and in webhooks.
#[api::scalar(example = "1042", min_length = 1, max_length = 120, trim)]
pub struct MerchantRef(String);

/// What an order is for, shown to the payer under the store's name: one
/// line of plain text.
#[api::scalar(
    pattern = r"^[^\x00-\x1F\x7F-\x9F]*$",
    example = "Order #1042: Blue mug + 2 more",
    min_length = 1,
    max_length = 140,
    trim
)]
pub struct Description(String);

/// The caller's key for one purchase: visible ASCII characters. A retry
/// with the same key gets the order the first attempt made, never a
/// second one.
#[api::scalar(
    pattern = r"^[!-~]+$",
    example = "order-1042-total-1250",
    min_length = 1,
    max_length = 100
)]
pub struct IdempotencyKey(String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scalar_checks_what_it_reads() {
        assert_eq!(Amount::parse(" 12.50 ").unwrap().as_str(), "12.50");
        assert!(Amount::parse("12,50").is_err());
        assert!(Amount::parse("0.000000000001").is_ok());
        assert!(MerchantRef::parse(&"x".repeat(121)).is_err());
        assert!(Description::parse("two\nlines").is_err());
        assert!(IdempotencyKey::parse("with space").is_err());
        let refused: Result<Amount, _> = serde_json::from_str("\"twelve\"");
        assert!(refused
            .unwrap_err()
            .to_string()
            .contains("Amount must match"));
    }

    #[test]
    fn a_scalar_describes_its_rules() {
        let schema = schemars::schema_for!(MerchantRef).to_value();
        assert_eq!(schema["maxLength"], 120);
        assert_eq!(schema["examples"][0], "1042");
        assert!(schema["description"]
            .as_str()
            .unwrap()
            .starts_with("The shop's own reference"));
    }
}
