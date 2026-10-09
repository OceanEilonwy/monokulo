//! Key entry for stores whose keys go to the engine's `snp` backend, which
//! takes them only encrypted to it (`key-custody`'s `transport`).
//!
//! A form that may send keys there gets a bundle from the engine when it is
//! rendered: an attestation report vouching for the backend's key, AMD's
//! certificates and a single-use challenge. Then either:
//! - **with JavaScript**, `static/key-custody.js` runs key custody's own
//!   checker, built as WebAssembly (`key_custody.wasm`, from this crate's
//!   build script), which verifies the bundle and encrypts the keys in the
//!   browser; the keys in the clear are never submitted;
//! - **without it**, the merchant runs `key-custody-cli seal` on their own
//!   computer against the bundle (served at `/key-custody/bundles/{id}`) and
//!   pastes what it prints. The page links the CLI release built with this
//!   monokulo, and its source.
//!
//! Either way monokulo only relays the encrypted keys, and never forwards
//! keys typed in the clear to a backend that takes them only encrypted
//! ([`store_keys`]).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

use super::AppState;
use crate::engine_client::{KeyBundleAnswer, StoreKeys};
use crate::views::key_entry::{CliDownload, SnpKeyEntry, SnpReady};
use shared::ids::UserId;

/// The backend that takes keys only encrypted to it.
pub const SNP: &str = "snp";

/// Whether `backend` takes a store's keys only encrypted to it.
pub fn takes_keys_encrypted(backend: &str) -> bool {
    backend == SNP
}

/// How long a bundle is kept to serve to `key-custody-cli`: as long as the
/// engine accepts its challenge.
const BUNDLE_TTL: Duration = Duration::from_secs(60 * 60);
/// The most bundles kept at once; the oldest go first.
const MAX_BUNDLES: usize = 10_000;

/// Bundles handed out for key entry forms, by id, for `key-custody-cli` to
/// fetch, and how many each account has opened lately
/// (`key_custody.snp_bundles_per_user*`), so one account can't expire
/// everyone else's.
#[derive(Clone)]
pub struct KeyBundles {
    bundles: Arc<parking_lot::Mutex<HashMap<String, StoredBundle>>>,
    opened: Arc<shared::rate_limit::RateLimiter<UserId>>,
}

impl Default for KeyBundles {
    fn default() -> Self {
        KeyBundles {
            bundles: Arc::default(),
            opened: Arc::new(shared::rate_limit::RateLimiter::new(
                crate::settings::SnpBundleLimits::default().per_user_per_min,
            )),
        }
    }
}

/// A bundle: who it was handed to, when, and its JSON.
struct StoredBundle {
    user: UserId,
    at: Instant,
    json: Arc<str>,
}

impl KeyBundles {
    /// Whether `user` may open another key entry form now, at most
    /// `per_minute` a minute; counts this one if so.
    fn may_open(&self, user: &UserId, per_minute: u32) -> bool {
        self.opened.set_limit(per_minute);
        self.opened.check(user.clone(), crate::now_unix())
    }

    /// Keeps `json` for `user`, dropping their oldest past `per_user` (and
    /// the oldest of all past [`MAX_BUNDLES`]); returns its id.
    fn insert(&self, user: &UserId, per_user: usize, json: String) -> String {
        let id = hex::encode(rand::random::<[u8; 16]>());
        let mut bundles = self.bundles.lock();
        bundles.retain(|_, bundle| bundle.at.elapsed() < BUNDLE_TTL);
        let oldest = |bundles: &HashMap<String, StoredBundle>, own: bool| {
            bundles
                .iter()
                .filter(|(_, bundle)| !own || bundle.user == *user)
                .min_by_key(|(_, bundle)| bundle.at)
                .map(|(id, _)| id.clone())
        };
        while bundles.values().filter(|b| b.user == *user).count() >= per_user {
            let Some(id) = oldest(&bundles, true) else {
                break;
            };
            bundles.remove(&id);
        }
        if bundles.len() >= MAX_BUNDLES {
            if let Some(id) = oldest(&bundles, false) {
                bundles.remove(&id);
            }
        }
        bundles.insert(
            id.clone(),
            StoredBundle {
                user: user.clone(),
                at: Instant::now(),
                json: json.into(),
            },
        );
        id
    }

    fn get(&self, id: &str) -> Option<Arc<str>> {
        self.bundles
            .lock()
            .get(id)
            .filter(|bundle| bundle.at.elapsed() < BUNDLE_TTL)
            .map(|bundle| Arc::clone(&bundle.json))
    }
}

/// What an account that opened too many key entry forms is told.
pub const TOO_MANY_FORMS: &str = "You've opened SEV-SNP key entry too many times in the last minute. Wait a minute, then reload this page.";

/// What a form's keys are for.
pub enum Purpose<'a> {
    /// A new store.
    Create,
    /// Moving the store whose secret is `sk`.
    Move(&'a shared::auth::RawToken),
}

/// The encrypted key entry for `user`'s form whose keys may go to `snp`
/// (one of `backends`, or every form while monokulo requires it), or `None`
/// when they can't. A bundle that can't be had, or an engine whose trust
/// settings aren't this site's, is shown as unavailable; an account past
/// its limit gets an alert, and nothing is asked of the engine.
pub async fn prepare(
    state: &AppState,
    user: &UserId,
    purpose: Purpose<'_>,
    backends: &[String],
) -> Option<SnpKeyEntry> {
    let policy = state.settings.snp_entry.load();
    if !policy.required && !backends.iter().any(|b| takes_keys_encrypted(b)) {
        return None;
    }
    let unavailable = || {
        SnpKeyEntry::Unavailable(
            "Encrypted key entry for SEV-SNP key storage isn't available right now. Try again in a minute; if it lasts, tell this site's operator."
                .to_owned(),
        )
    };
    let Some(trust) = policy.trust else {
        tracing::warn!("encrypted key entry is off: no engine ID key to trust is set (key_custody.snp_entry_id_key)");
        return Some(unavailable());
    };
    if let Some(why) = snp_unusable(state) {
        tracing::warn!(reason = %why, "SEV-SNP key entry isn't offered");
        return Some(unavailable());
    }
    let limits = *state.settings.snp_bundle_limits.load();
    if !state
        .engine
        .key_bundles
        .may_open(user, limits.per_user_per_min)
    {
        tracing::info!(user = %user, "an account opened too many SEV-SNP key entry forms");
        return Some(SnpKeyEntry::Limited(TOO_MANY_FORMS.to_owned()));
    }
    let answer = match purpose {
        Purpose::Create => state.engine.client.create_key_bundle(SNP).await,
        Purpose::Move(sk) => state.engine.client.move_key_bundle(sk, SNP).await,
    };
    Some(match answer {
        Ok(answer) => SnpKeyEntry::Ready(Box::new(ready_view(
            state,
            user,
            limits.per_user,
            &answer,
            &trust,
            policy.is_official(),
        ))),
        Err(e) => {
            tracing::warn!(error = %e, "couldn't get a key custody bundle from the engine");
            unavailable()
        }
    })
}

/// Why the `snp` backend can't take keys from this site's forms right now,
/// or `None` when it can: monokulo's own policy is set, the engine's status
/// is known, and the engine trusts exactly the images monokulo does.
pub fn snp_unusable(state: &AppState) -> Option<String> {
    let policy = state.settings.snp_entry.load();
    let Some(ours) = policy.trust else {
        return Some(
            "this site has no engine ID key to trust (key_custody.snp_entry_id_key)".to_owned(),
        );
    };
    match super::status_page::known_snp_trust(&state.engine) {
        None => Some("the engine's status isn't known".to_owned()),
        Some(None) => Some("the engine reports no SEV-SNP backend set up".to_owned()),
        Some(Some(engine)) => {
            let differences = trust_differences(&ours, &engine);
            (!differences.is_empty()).then(|| {
                differences
                    .into_iter()
                    .map(|(_, difference)| difference)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
        }
    }
}

/// How the engine's trust settings differ from monokulo's own: monokulo's
/// setting and a phrase naming both, for each; empty when they agree.
pub fn trust_differences(
    ours: &key_custody::transport::TrustPolicy,
    engine: &crate::engine_client::SnpTrustStatus,
) -> Vec<(&'static str, String)> {
    use crate::settings::{
        KEY_CUSTODY_SNP_ENTRY_ID_KEY, KEY_CUSTODY_SNP_ENTRY_MIN_GUEST_SVN,
        KEY_CUSTODY_SNP_ENTRY_MIN_TCB,
    };
    let mut differences = Vec::new();
    if !engine
        .id_key_digest
        .trim()
        .eq_ignore_ascii_case(&hex::encode(ours.id_key_digest))
    {
        differences.push((
            KEY_CUSTODY_SNP_ENTRY_ID_KEY.key,
            format!(
                "the ID key: key_custody.snp_entry_id_key here is {}, the engine's key_custody.snp_trusted_id_key is {}",
                hex::encode(ours.id_key_digest),
                engine.id_key_digest.trim()
            ),
        ));
    }
    if engine.min_guest_svn != ours.min_guest_svn {
        differences.push((
            KEY_CUSTODY_SNP_ENTRY_MIN_GUEST_SVN.key,
            format!(
                "the minimum security version: key_custody.snp_entry_min_guest_svn here is {}, the engine's key_custody.snp_min_guest_svn is {}",
                ours.min_guest_svn, engine.min_guest_svn
            ),
        ));
    }
    let engine_tcb = key_custody::transport::TcbFloor::parse(&engine.min_tcb);
    if engine_tcb.as_ref().ok() != Some(&ours.min_tcb) {
        let show = |text: String| {
            if text.is_empty() {
                "empty".to_owned()
            } else {
                text
            }
        };
        differences.push((
            KEY_CUSTODY_SNP_ENTRY_MIN_TCB.key,
            format!(
                "the firmware floor: key_custody.snp_entry_min_tcb here is {}, the engine's key_custody.snp_min_tcb is {}",
                show(ours.min_tcb.to_text()),
                show(engine.min_tcb.trim().to_owned())
            ),
        ));
    }
    differences
}

/// The key custody backends a store may use from this site's forms: the
/// engine's, the default first, less `snp` while [`snp_unusable`], and
/// only `snp` while monokulo requires it.
pub fn usable_custody_backends(state: &AppState) -> Vec<String> {
    let required = state.settings.snp_entry.load().required;
    let snp_ok = snp_unusable(state).is_none();
    super::status_page::known_enabled_custody_backends(&state.engine)
        .into_iter()
        .filter(|backend| {
            if takes_keys_encrypted(backend) {
                snp_ok
            } else {
                !required
            }
        })
        .collect()
}

/// The backends a new store's keys may go to from a form: its choices, or
/// (no choice offered) the first usable one.
pub fn offered_backends(
    state: &AppState,
    choices: &[crate::views::connect::CustodyChoice],
) -> Vec<String> {
    if choices.is_empty() {
        usable_custody_backends(state).into_iter().take(1).collect()
    } else {
        choices.iter().map(|c| c.backend.clone()).collect()
    }
}

/// `value` as one shell word: unchanged when it holds only characters no
/// shell treats specially, single-quoted otherwise.
fn shell_word(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./:=,%@+".contains(&b));
    if plain {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn ready_view(
    state: &AppState,
    user: &UserId,
    per_user: usize,
    answer: &KeyBundleAnswer,
    trust: &key_custody::transport::TrustPolicy,
    official: bool,
) -> SnpReady {
    let json = answer.bundle.to_string();
    let id = state
        .engine
        .key_bundles
        .insert(user, per_user, json.clone());
    let path = format!("/key-custody/bundles/{id}");
    let public = state.settings.public_url();
    let bundle_arg = match &public {
        Some(base) => format!("{base}{path}"),
        None => "key-custody-bundle.json".to_owned(),
    };
    let digest = hex::encode(trust.id_key_digest);
    let min_tcb = trust.min_tcb.to_text();
    let mut command = format!("key-custody-cli seal --bundle {}", shell_word(&bundle_arg));
    if !official {
        command.push_str(&format!(" --trust-id-key {}", shell_word(&digest)));
    }
    if trust.min_guest_svn > 0 {
        command.push_str(&format!(" --min-guest-svn {}", trust.min_guest_svn));
    }
    if !min_tcb.is_empty() {
        command.push_str(&format!(" --min-tcb {}", shell_word(&min_tcb)));
    }
    let links = state.settings.cli_links.load();
    let release = cli_release();
    SnpReady {
        bundle_json: json,
        bundle_path: path,
        bundle_is_file: public.is_none(),
        trust_id_key: (!official).then(|| digest.clone()),
        // The official key is the one built into the browser's checker: it
        // isn't told which key to trust then.
        id_key_digest: (!official).then(|| digest.clone()),
        min_guest_svn: trust.min_guest_svn,
        min_tcb,
        command,
        version: release.version.to_owned(),
        downloads: release
            .tag
            .map(|_| downloads(&links.download, release.version))
            .unwrap_or_default(),
        source_url: links.source.replace("{ref}", &release.reference()),
        verify_command: env!("CARGO_PKG_REPOSITORY")
            .strip_prefix("https://github.com/")
            .map(|repo| format!("gh attestation verify <the file> --repo {repo}")),
    }
}

/// Which build this is, for the key-custody-cli links.
pub struct CliRelease {
    pub version: &'static str,
    /// The release tag (`v{version}`) when this build is a release.
    pub tag: Option<&'static str>,
    /// The commit this build is from, when known.
    pub commit: Option<&'static str>,
}

impl CliRelease {
    /// What the source link points at: the release tag, else the commit,
    /// else the main branch.
    pub fn reference(&self) -> String {
        self.tag.or(self.commit).unwrap_or("main").to_owned()
    }
}

pub fn cli_release() -> CliRelease {
    CliRelease {
        version: env!("CARGO_PKG_VERSION"),
        tag: option_env!("MONOKULO_RELEASE_TAG").filter(|tag| !tag.is_empty()),
        commit: option_env!("MONOKULO_GIT_COMMIT").filter(|commit| !commit.is_empty()),
    }
}

/// The computers key-custody-cli is released for: what to call each, its
/// Rust target, and its archive's extension.
pub const CLI_TARGETS: [(&str, &str, &str); 5] = [
    ("Linux (x86-64)", "x86_64-unknown-linux-gnu", "tar.gz"),
    ("Linux (ARM64)", "aarch64-unknown-linux-gnu", "tar.gz"),
    ("macOS (Apple silicon)", "aarch64-apple-darwin", "tar.gz"),
    ("macOS (Intel)", "x86_64-apple-darwin", "tar.gz"),
    ("Windows (x86-64)", "x86_64-pc-windows-msvc", "zip"),
];

/// The release archive name for `target`, as CI publishes it.
pub fn cli_file(version: &str, target: &str, extension: &str) -> String {
    format!("key-custody-cli-{version}-{target}.{extension}")
}

fn downloads(template: &str, version: &str) -> Vec<CliDownload> {
    CLI_TARGETS
        .iter()
        .map(|(label, target, extension)| {
            let url = template
                .replace("{version}", version)
                .replace("{file}", &cli_file(version, target, extension));
            CliDownload {
                label: (*label).to_owned(),
                checksum_url: format!("{url}.sha256"),
                url,
            }
        })
        .collect()
}

/// The backend a form's keys go to and the keys, in the form it takes.
/// Refused, with what to do instead, when they came in the clear for a
/// backend that takes them only encrypted (they are not sent on), or
/// encrypted for one that takes them as they are, or for a backend this
/// site doesn't offer now ([`usable_custody_backends`]). `backend` is
/// `None` for the default: the first usable backend, named so the engine
/// doesn't pick one this site wouldn't (`None` stays for an engine with a
/// single plain backend, or one whose status isn't known).
pub fn store_keys(
    state: &AppState,
    backend: Option<&str>,
    view_key_hex: &str,
    spend_pubkey_hex: &str,
    encrypted_keys: Option<&str>,
) -> Result<(Option<String>, StoreKeys), String> {
    let engine = &state.engine;
    let required = state.settings.snp_entry.load().required;
    let encrypted = encrypted_keys
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let status_known = super::status_page::known_status_is_fresh(engine);
    let not_answering =
        "The engine isn't answering, so this store's keys were not sent. Try again in a minute.";
    let backend = match backend {
        Some(backend) => Some(backend.to_owned()),
        None if !status_known => {
            // The engine's default can't be known: it might take keys only
            // encrypted, so typed ones aren't sent on.
            if required || (encrypted.is_none() && !view_key_hex.trim().is_empty()) {
                return Err(not_answering.to_owned());
            }
            None
        }
        // A status listing no backends is an engine with a single plain one.
        None if super::status_page::known_enabled_custody_backends(engine).is_empty() => None,
        None => Some(usable_custody_backends(state).into_iter().next().ok_or(
            "No key storage can take this store's keys right now, so they were not sent. Try again later; if it lasts, tell this site's operator.",
        )?),
    };
    match backend.as_deref() {
        Some(backend) if takes_keys_encrypted(backend) => {
            if let Some(why) = snp_unusable(state) {
                tracing::warn!(reason = %why, "keys for SEV-SNP key storage refused");
                return Err(
                    "SEV-SNP key storage isn't available right now, so the keys were not sent. Try again later; if it lasts, tell this site's operator."
                        .to_owned(),
                );
            }
        }
        _ if required => {
            return Err(
                "This site keeps stores' keys only in SEV-SNP key storage, and the keys were not sent. Choose SEV-SNP key storage."
                    .to_owned(),
            );
        }
        _ => {}
    }
    let encrypted_only = backend.as_deref().is_some_and(takes_keys_encrypted);
    if encrypted_only {
        if !view_key_hex.trim().is_empty() {
            return Err(
                "SEV-SNP key storage takes keys only encrypted for the engine, and the keys you typed were not sent. \
                 Turn JavaScript on so this page can encrypt them, or encrypt them with key-custody-cli and paste what it prints."
                    .to_owned(),
            );
        }
        let Some(encrypted) = encrypted else {
            return Err(
                "Paste the encrypted keys key-custody-cli printed, or turn JavaScript on so this page can encrypt them."
                    .to_owned(),
            );
        };
        return Ok((
            backend,
            StoreKeys {
                encrypted_keys: Some(encrypted.to_owned()),
                ..StoreKeys::default()
            },
        ));
    }
    if encrypted.is_some() && backend.is_some() {
        return Err(
            "Encrypted keys are only for SEV-SNP key storage: enter the keys themselves for this one.".to_owned(),
        );
    }
    Ok((
        backend,
        StoreKeys {
            view_key_hex: view_key_hex.trim().to_owned(),
            spend_pubkey_hex: spend_pubkey_hex.trim().to_owned(),
            encrypted_keys: encrypted.map(str::to_owned),
        },
    ))
}

/// `GET /key-custody/bundles/{id}`: a bundle handed out with a key entry
/// form, for `key-custody-cli`. Public, like the challenge it carries is
/// worthless without the keys it is for; ids are unguessable.
pub async fn bundle(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.engine.key_bundles.get(&id) {
        Some(json) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"key-custody-bundle.json\"",
                ),
                (header::CACHE_CONTROL, "no-store"),
            ],
            json.to_string(),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            "This bundle has expired or never existed. Load the key entry form again for a new one.",
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_words_are_quoted_unless_plainly_safe() {
        assert_eq!(
            shell_word("https://pay.example.com/key-custody/bundles/ab"),
            "https://pay.example.com/key-custody/bundles/ab"
        );
        assert_eq!(shell_word("1,2,3,4"), "1,2,3,4");
        assert_eq!(shell_word("a b"), "'a b'");
        assert_eq!(shell_word("it's"), "'it'\\''s'");
        assert_eq!(shell_word("$(x)"), "'$(x)'");
        assert_eq!(shell_word(""), "''");
    }

    #[test]
    fn download_links_name_this_version_and_each_computer() {
        let links = downloads(
            "https://example.com/releases/download/v{version}/{file}",
            "1.2.3",
        );
        assert_eq!(links.len(), CLI_TARGETS.len());
        assert_eq!(
            links[0].url,
            "https://example.com/releases/download/v1.2.3/key-custody-cli-1.2.3-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert!(links[4]
            .url
            .ends_with("key-custody-cli-1.2.3-x86_64-pc-windows-msvc.zip"));
        assert_eq!(links[0].checksum_url, format!("{}.sha256", links[0].url));
    }

    #[test]
    fn the_source_link_follows_the_release_then_the_commit() {
        let release = |tag, commit| CliRelease {
            version: "1.2.3",
            tag,
            commit,
        };
        assert_eq!(release(Some("v1.2.3"), Some("abc")).reference(), "v1.2.3");
        assert_eq!(release(None, Some("abc")).reference(), "abc");
        assert_eq!(release(None, None).reference(), "main");
    }

    use crate::settings::{MonokuloSettings, PerRequest, SnpEntryPolicy};
    use key_custody::transport::{TcbFloor, TrustPolicy};

    fn status(
        backends: serde_json::Value,
        default: Option<&str>,
        snp_trust: Option<serde_json::Value>,
    ) -> crate::engine_client::EngineStatusResponse {
        serde_json::from_value(serde_json::json!({
            "networks": [],
            "poll_interval_secs": 5,
            "generated_at": 0,
            "key_custody": backends,
            "key_custody_default": default,
            "key_custody_snp_trust": snp_trust,
        }))
        .unwrap()
    }

    fn both_backends() -> serde_json::Value {
        serde_json::json!([{ "backend": "plain", "error": null }, { "backend": "snp", "error": null }])
    }

    fn our_trust() -> TrustPolicy {
        TrustPolicy {
            id_key_digest: [0xAB; 48],
            min_guest_svn: 2,
            min_tcb: TcbFloor::parse("1,2,3,4").unwrap(),
        }
    }

    fn engine_trust(digest: &str, min_guest_svn: u32, min_tcb: &str) -> serde_json::Value {
        serde_json::json!({
            "id_key_digest": digest,
            "min_guest_svn": min_guest_svn,
            "min_tcb": min_tcb,
        })
    }

    fn matching_engine_trust() -> serde_json::Value {
        engine_trust(&"ab".repeat(48), 2, "1,2,3,4")
    }

    /// A test state whose key entry policy is `trust`, required or not.
    fn state_with_policy(trust: Option<TrustPolicy>, required: bool) -> AppState {
        let mut state = AppState::for_tests();
        state.settings = MonokuloSettings::fixed_with_snp_entry(
            PerRequest {
                signup_mode: crate::settings::SignupMode::Public,
                public_url: String::new(),
            },
            SnpEntryPolicy { trust, required },
        );
        state
    }

    /// Typed keys go on only where the first usable backend is known to take
    /// them: refused while the engine's status is unknown or its default is
    /// snp, sent when it lists plain first, or no backends at all (a single
    /// plain one).
    #[test]
    fn typed_keys_are_sent_only_where_the_default_takes_them() {
        let view = "07".repeat(32);
        let spend = "08".repeat(32);
        let state = state_with_policy(Some(our_trust()), false);
        assert!(store_keys(&state, None, &view, &spend, None)
            .unwrap_err()
            .contains("isn't answering"));

        super::super::status_page::seed_status_for_tests(
            &state.engine,
            status(serde_json::json!([]), None, None),
        );
        let (backend, keys) = store_keys(&state, None, &view, &spend, None).unwrap();
        assert_eq!((backend, keys.view_key_hex), (None, view.clone()));

        super::super::status_page::seed_status_for_tests(
            &state.engine,
            status(both_backends(), Some("snp"), Some(matching_engine_trust())),
        );
        assert!(store_keys(&state, None, &view, &spend, None)
            .unwrap_err()
            .contains("were not sent"));
        assert!(store_keys(&state, Some("plain"), &view, &spend, None).is_ok());
        let (backend, keys) = store_keys(&state, None, "", "", Some("{envelope}")).unwrap();
        assert_eq!(
            backend.as_deref(),
            Some("snp"),
            "named, not left to the engine"
        );
        assert_eq!(keys.encrypted_keys.as_deref(), Some("{envelope}"));
    }

    /// The engine trusting other images than this site's policy takes snp
    /// off the forms: it isn't a choice, keys for it are refused, and a store
    /// whose engine default is snp goes to plain, named, instead.
    #[test]
    fn snp_is_not_offered_while_the_engine_trusts_other_images() {
        let view = "07".repeat(32);
        let spend = "08".repeat(32);
        let state = state_with_policy(Some(our_trust()), false);
        for (engine, differs_in) in [
            (engine_trust(&"cd".repeat(48), 2, "1,2,3,4"), "the ID key"),
            (
                engine_trust(&"ab".repeat(48), 1, "1,2,3,4"),
                "the minimum security version",
            ),
            (engine_trust(&"ab".repeat(48), 2, ""), "the firmware floor"),
        ] {
            super::super::status_page::seed_status_for_tests(
                &state.engine,
                status(both_backends(), Some("snp"), Some(engine.clone())),
            );
            let why = snp_unusable(&state).unwrap();
            assert!(why.contains(differs_in), "{why}");
            assert_eq!(usable_custody_backends(&state), vec!["plain".to_owned()]);
            assert!(super::super::status_page::custody_choice_views(&state, None).is_empty());
            assert!(store_keys(&state, Some("snp"), "", "", Some("{envelope}"))
                .unwrap_err()
                .contains("isn't available"));
            let (backend, _) = store_keys(&state, None, &view, &spend, None).unwrap();
            assert_eq!(backend.as_deref(), Some("plain"));
        }

        super::super::status_page::seed_status_for_tests(
            &state.engine,
            status(both_backends(), Some("snp"), Some(matching_engine_trust())),
        );
        assert_eq!(snp_unusable(&state), None);
        assert_eq!(
            usable_custody_backends(&state),
            vec!["snp".to_owned(), "plain".to_owned()]
        );
    }

    /// With encrypted entry required, keys in the clear never go anywhere,
    /// even to an engine that says it has only plain; snp, when usable, is
    /// the only choice.
    #[test]
    fn required_encrypted_entry_never_sends_typed_keys() {
        let view = "07".repeat(32);
        let spend = "08".repeat(32);
        let state = state_with_policy(Some(our_trust()), true);
        assert!(
            store_keys(&state, None, "", "", Some("{envelope}")).is_err(),
            "status unknown"
        );
        super::super::status_page::seed_status_for_tests(
            &state.engine,
            status(serde_json::json!([]), None, None),
        );
        assert!(store_keys(&state, None, &view, &spend, None)
            .unwrap_err()
            .contains("only in SEV-SNP"));
        super::super::status_page::seed_status_for_tests(
            &state.engine,
            status(
                both_backends(),
                Some("plain"),
                Some(matching_engine_trust()),
            ),
        );
        assert_eq!(usable_custody_backends(&state), vec!["snp".to_owned()]);
        assert!(store_keys(&state, Some("plain"), &view, &spend, None)
            .unwrap_err()
            .contains("only in SEV-SNP"));
        let (backend, _) = store_keys(&state, None, "", "", Some("{envelope}")).unwrap();
        assert_eq!(backend.as_deref(), Some("snp"));
    }

    /// The status page's alert: none while the two agree or snp isn't used,
    /// what differs while they don't, and a required backend the engine
    /// lacks.
    #[test]
    fn the_status_alert_names_what_differs() {
        use super::super::status_page::snp_policy_alert;
        let policy = SnpEntryPolicy {
            trust: Some(our_trust()),
            required: false,
        };
        let only_plain = status(
            serde_json::json!([{ "backend": "plain", "error": null }]),
            None,
            None,
        );
        assert_eq!(snp_policy_alert(&policy, &only_plain), None);
        let agreeing = status(both_backends(), None, Some(matching_engine_trust()));
        assert_eq!(snp_policy_alert(&policy, &agreeing), None);
        let other_key = status(
            both_backends(),
            None,
            Some(engine_trust(&"cd".repeat(48), 2, "1,2,3,4")),
        );
        let alert = snp_policy_alert(&policy, &other_key).unwrap();
        assert!(alert.contains("key_custody.snp_entry_id_key"), "{alert}");
        assert!(alert.contains(&"cd".repeat(48)), "{alert}");
        let required = SnpEntryPolicy {
            required: true,
            ..policy
        };
        assert!(snp_policy_alert(&required, &only_plain)
            .unwrap()
            .contains("no SEV-SNP backend"));
    }

    #[test]
    fn a_bundle_is_served_while_it_lasts() {
        let bundles = KeyBundles::default();
        let id = bundles.insert(&UserId::new("u1"), 20, "{\"v\":1}".into());
        assert_eq!(bundles.get(&id).as_deref(), Some("{\"v\":1}"));
        assert_eq!(bundles.get("nope"), None);
        assert_eq!(id.len(), 32);
    }

    /// One account opening form after form expires only its own oldest
    /// forms, never another account's.
    #[test]
    fn an_account_past_its_open_forms_loses_only_its_own_oldest() {
        let bundles = KeyBundles::default();
        let (flooder, other) = (UserId::new("flooder"), UserId::new("other"));
        let theirs = bundles.insert(&other, 2, "theirs".into());
        let first = bundles.insert(&flooder, 2, "1".into());
        for _ in 0..50 {
            bundles.insert(&flooder, 2, "more".into());
        }
        assert_eq!(bundles.get(&first), None, "the flooder's oldest went");
        assert_eq!(bundles.get(&theirs).as_deref(), Some("theirs"));
        let kept = bundles
            .bundles
            .lock()
            .values()
            .filter(|b| b.user == flooder)
            .count();
        assert_eq!(kept, 2);
    }

    /// Past the per-minute limit, the form is an alert and the engine isn't
    /// asked for a challenge; another account is unaffected.
    #[cfg(feature = "snp")]
    #[tokio::test]
    async fn an_account_opening_too_many_forms_gets_an_alert() {
        let engine = engine_test_support::TestEngineConfig::new()
            .with_snp_backend()
            .spawn()
            .await;
        let base = crate::settings::MonokuloSettings::fixed_with_snp_entry(
            crate::settings::PerRequest {
                signup_mode: crate::settings::SignupMode::Public,
                public_url: String::new(),
            },
            crate::settings::SnpEntryPolicy {
                trust: Some(engine_test_support::snp_test_trust()),
                required: false,
            },
        );
        let settings = Arc::new(crate::settings::MonokuloSettings {
            registry: None,
            server: base.server.clone(),
            per_request: base.per_request.clone(),
            cli_links: base.cli_links.clone(),
            snp_entry: base.snp_entry.clone(),
            snp_bundle_limits: live_settings::Live::new(crate::settings::SnpBundleLimits {
                per_user: 20,
                per_user_per_min: 2,
            }),
        });
        let state = AppState {
            engine: crate::http::Engine::new(crate::http::EngineClient::embedded_for_tests(
                engine.router(),
            )),
            settings,
            ..AppState::for_tests()
        };
        crate::http::status_page::get_status_cached(&state.engine)
            .await
            .unwrap();
        let backends = vec![SNP.to_owned()];
        let (flooder, other) = (UserId::new("flooder"), UserId::new("other"));
        for _ in 0..2 {
            assert!(matches!(
                prepare(&state, &flooder, Purpose::Create, &backends).await,
                Some(SnpKeyEntry::Ready(_))
            ));
        }
        let limited = prepare(&state, &flooder, Purpose::Create, &backends).await;
        assert!(
            matches!(&limited, Some(SnpKeyEntry::Limited(m)) if m == TOO_MANY_FORMS),
            "past the limit"
        );
        let html =
            crate::views::key_entry::snp_section(limited.as_ref().unwrap(), None).into_string();
        assert!(
            html.contains(r#"<div class="error" role="alert">"#),
            "{html}"
        );
        assert!(html.contains("Wait a minute"), "{html}");
        assert!(matches!(
            prepare(&state, &other, Purpose::Create, &backends).await,
            Some(SnpKeyEntry::Ready(_))
        ));
    }
}
