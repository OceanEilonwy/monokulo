//! Every runtime-configurable monokulo setting the admin settings page
//! exposes (`http/admin_settings.rs`), resolved via the same
//! `env > database > default` precedence (`shared::settings`) the scanner's
//! own equivalent module (`scanner::settings`) uses. Deliberately mirrors
//! that module's shape - one `ScalarSetting` per knob (a stable `key`, the
//! environment variable that overrides it, and a code default), declared
//! once via the `scalar_settings!` macro so the admin HTTP handler and every
//! boot-time reader agree on exactly the same key/env-var/default triple.
//!
//! **`MONOKULO_ENCRYPTION_KEY` is deliberately not here.** Every other
//! setting below can be changed at any time with no lasting consequence
//! beyond "the new value takes effect on the next read" - this one can't:
//! it's the AES-256-GCM key every `store_connections.tenant_secret_token_encrypted`
//! row was encrypted with (`crate::crypto`), so rotating it live (or even
//! exposing its current value on a settings page) would either corrupt
//! every already-encrypted secret token or leak the key that protects them.
//! It stays exactly what it always was: a required environment variable,
//! read once at boot (`main.rs::encryption_key_from_env`), with no database
//! fallback and no admin-page field.

use shared::settings::{resolve_parsed, resolve_raw, SettingSource};

use crate::db::Db;

/// One scalar setting's identity - see `scanner::settings::ScalarSetting`'s
/// own doc comment for why this is a plain data triple rather than an enum
/// or a trait.
pub struct ScalarSetting {
    pub key: &'static str,
    pub env_var: &'static str,
    pub default: &'static str,
}

macro_rules! scalar_settings {
    ($($name:ident => { key: $key:expr, env: $env:expr, default: $default:expr }),+ $(,)?) => {
        $(pub const $name: ScalarSetting = ScalarSetting { key: $key, env_var: $env, default: $default };)+
        /// Every monokulo setting the admin settings page shows and can save.
        pub const ALL_SCALAR: &[ScalarSetting] = &[$($name),+];
    };
}

scalar_settings! {
    SIGNUP_MODE => { key: "signup.mode", env: "MONOKULO_SIGNUP_MODE", default: "invite_only" },
    ENGINE_URL => { key: "engine.url", env: "MONOKULO_ENGINE_URL", default: "http://127.0.0.1:8080" },
    SCANNER_ADMIN_TOKEN => { key: "engine.admin_token", env: "MONOKULO_SCANNER_ADMIN_TOKEN", default: "" },
    EXCHANGE_RATE_COINGECKO_ENABLED => { key: "exchange_rate.coingecko_enabled", env: "MONOKULO_EXCHANGE_RATE_COINGECKO_ENABLED", default: "true" },
    EXCHANGE_RATE_COINGECKO_BASE_URL => { key: "exchange_rate.coingecko_base_url", env: "MONOKULO_EXCHANGE_RATE_COINGECKO_BASE_URL", default: "https://api.coingecko.com" },
    EXCHANGE_RATE_CACHE_SECONDS => { key: "exchange_rate.cache_seconds", env: "MONOKULO_EXCHANGE_RATE_CACHE_SECONDS", default: "30" },
    RESCAN_DEFAULT_LOOKBACK_DAYS => { key: "rescan.default_lookback_days", env: "MONOKULO_RESCAN_DEFAULT_LOOKBACK_DAYS", default: "7" },
    RESCAN_MAX_LOOKBACK_DAYS => { key: "rescan.max_lookback_days", env: "MONOKULO_RESCAN_MAX_LOOKBACK_DAYS", default: "90" },
    HTTP_CACHE_MAX_MB => { key: "http_cache.max_mb", env: "MONOKULO_HTTP_CACHE_MAX_MB", default: "16" },
    RATE_LIMIT_PER_IP_PER_MIN => { key: "rate_limit.per_ip_per_min", env: "MONOKULO_RATE_LIMIT_PER_IP_PER_MIN", default: "20" },
}

pub fn get<T: std::str::FromStr>(db: &Db, setting: &ScalarSetting) -> T {
    let db_value = db.get_setting(setting.key).ok().flatten();
    resolve_parsed(setting.env_var, db_value.as_deref(), setting.default.parse().unwrap_or_else(|_| {
        panic!("{}'s own hardcoded default {:?} does not parse as the type it's requested as - a bug in this module, not a runtime input", setting.key, setting.default)
    }))
}

pub fn get_raw(db: &Db, setting: &ScalarSetting) -> (String, SettingSource) {
    let db_value = db.get_setting(setting.key).ok().flatten();
    resolve_raw(setting.env_var, db_value.as_deref(), setting.default)
}

/// `SIGNUP_MODE`'s two valid values - a real enum rather than every caller
/// matching on the raw string, so a typo'd or otherwise malformed stored
/// value has exactly one place (here) that decides what it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignupMode {
    Public,
    InviteOnly,
}

/// This instance's currently effective signup mode - anything other than
/// the literal `"public"` is treated as `InviteOnly`, the same
/// fail-closed-by-default posture `SIGNUP_MODE`'s own `"invite_only"`
/// default already has (a malformed or unrecognized stored value should
/// never accidentally open public signup).
pub fn signup_mode(db: &Db) -> SignupMode {
    match get::<String>(db, &SIGNUP_MODE).as_str() {
        "public" => SignupMode::Public,
        _ => SignupMode::InviteOnly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scalar_settings_own_default_parses_as_the_type_boot_code_actually_requests_it_as() {
        let db = Db::open_in_memory().unwrap();
        let _: String = get(&db, &SIGNUP_MODE);
        let _: String = get(&db, &ENGINE_URL);
        let _: String = get(&db, &SCANNER_ADMIN_TOKEN);
        let _: bool = get(&db, &EXCHANGE_RATE_COINGECKO_ENABLED);
        let _: String = get(&db, &EXCHANGE_RATE_COINGECKO_BASE_URL);
        let _: u64 = get(&db, &EXCHANGE_RATE_CACHE_SECONDS);
        let _: u32 = get(&db, &RESCAN_DEFAULT_LOOKBACK_DAYS);
        let _: u32 = get(&db, &RESCAN_MAX_LOOKBACK_DAYS);
        let _: u64 = get(&db, &HTTP_CACHE_MAX_MB);
        let _: u32 = get(&db, &RATE_LIMIT_PER_IP_PER_MIN);
    }

    #[test]
    fn a_saved_scalar_setting_is_read_back_over_the_default() {
        let db = Db::open_in_memory().unwrap();
        db.set_setting(RESCAN_DEFAULT_LOOKBACK_DAYS.key, "3").unwrap();
        let value: u32 = get(&db, &RESCAN_DEFAULT_LOOKBACK_DAYS);
        assert_eq!(value, 3);
    }

    #[test]
    fn an_env_var_overrides_a_saved_setting() {
        let db = Db::open_in_memory().unwrap();
        db.set_setting(RESCAN_DEFAULT_LOOKBACK_DAYS.key, "3").unwrap();
        std::env::set_var(RESCAN_DEFAULT_LOOKBACK_DAYS.env_var, "9");
        let value: u32 = get(&db, &RESCAN_DEFAULT_LOOKBACK_DAYS);
        std::env::remove_var(RESCAN_DEFAULT_LOOKBACK_DAYS.env_var);
        assert_eq!(value, 9);
    }

    #[test]
    fn get_raw_reports_which_source_actually_won() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(get_raw(&db, &ENGINE_URL).1, SettingSource::Default);
        db.set_setting(ENGINE_URL.key, "http://scanner.internal:8443").unwrap();
        assert_eq!(get_raw(&db, &ENGINE_URL), ("http://scanner.internal:8443".to_string(), SettingSource::Database));
        std::env::set_var(ENGINE_URL.env_var, "http://env-wins.example");
        assert_eq!(get_raw(&db, &ENGINE_URL).1, SettingSource::Env);
        std::env::remove_var(ENGINE_URL.env_var);
    }
}
