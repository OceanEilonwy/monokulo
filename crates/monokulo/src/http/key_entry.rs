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
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use super::{AppState, Engine};
use crate::engine_client::{KeyBundleAnswer, StoreKeys};
use crate::views::key_entry::{CliDownload, SnpKeyEntry, SnpReady};

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
/// fetch.
#[derive(Default, Clone)]
pub struct KeyBundles(Arc<parking_lot::Mutex<HashMap<String, StoredBundle>>>);

/// A bundle's JSON, and when it was handed out.
type StoredBundle = (Instant, Arc<str>);

impl KeyBundles {
    fn insert(&self, json: String) -> String {
        let id = hex::encode(rand::random::<[u8; 16]>());
        let mut bundles = self.0.lock();
        bundles.retain(|_, (at, _)| at.elapsed() < BUNDLE_TTL);
        if bundles.len() >= MAX_BUNDLES {
            if let Some(oldest) = bundles
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(id, _)| id.clone())
            {
                bundles.remove(&oldest);
            }
        }
        bundles.insert(id.clone(), (Instant::now(), json.into()));
        id
    }

    fn get(&self, id: &str) -> Option<Arc<str>> {
        self.0
            .lock()
            .get(id)
            .filter(|(at, _)| at.elapsed() < BUNDLE_TTL)
            .map(|(_, json)| Arc::clone(json))
    }
}

/// What a form's keys are for.
pub enum Purpose<'a> {
    /// A new store.
    Create,
    /// Moving the store whose secret is `sk`.
    Move(&'a shared::auth::RawToken),
}

/// The encrypted key entry for a form whose keys may go to `snp` (one of
/// `backends`), or `None` when they can't. A bundle that can't be had is
/// shown as why.
pub async fn prepare(
    state: &AppState,
    purpose: Purpose<'_>,
    backends: &[String],
) -> Option<SnpKeyEntry> {
    if !backends.iter().any(|b| takes_keys_encrypted(b)) {
        return None;
    }
    let answer = match purpose {
        Purpose::Create => state.engine.client.create_key_bundle(SNP).await,
        Purpose::Move(sk) => state.engine.client.move_key_bundle(sk, SNP).await,
    };
    Some(match answer {
        Ok(answer) => SnpKeyEntry::Ready(ready_view(state, &answer)),
        Err(e) => {
            tracing::warn!(error = %e, "couldn't get a key custody bundle from the engine");
            SnpKeyEntry::Unavailable(
                "Encrypted key entry for SEV-SNP key storage isn't available right now. Try again in a minute."
                    .to_owned(),
            )
        }
    })
}

/// The backends a new store's keys may go to from a form: its choices, or
/// (no choice offered) the engine's default.
pub fn offered_backends(
    engine: &Engine,
    choices: &[crate::views::connect::CustodyChoice],
) -> Vec<String> {
    if choices.is_empty() {
        super::status_page::known_enabled_custody_backends(engine)
            .into_iter()
            .take(1)
            .collect()
    } else {
        choices.iter().map(|c| c.backend.clone()).collect()
    }
}

fn ready_view(state: &AppState, answer: &KeyBundleAnswer) -> SnpReady {
    let json = answer.bundle.to_string();
    let id = state.engine.key_bundles.insert(json.clone());
    let path = format!("/key-custody/bundles/{id}");
    let public = state.settings.public_url();
    let bundle_arg = match &public {
        Some(base) => format!("{base}{path}"),
        None => "key-custody-bundle.json".to_owned(),
    };
    let mut command = format!("key-custody-cli seal --bundle {bundle_arg}");
    if !answer.trust.official {
        command.push_str(&format!(" --trust-id-key {}", answer.trust.id_key_digest));
    }
    if answer.trust.min_guest_svn > 0 {
        command.push_str(&format!(" --min-guest-svn {}", answer.trust.min_guest_svn));
    }
    if !answer.trust.min_tcb.is_empty() {
        command.push_str(&format!(" --min-tcb {}", answer.trust.min_tcb));
    }
    let links = state.settings.cli_links.load();
    let release = cli_release();
    SnpReady {
        bundle_json: json,
        bundle_path: path,
        bundle_is_file: public.is_none(),
        trust_id_key: (!answer.trust.official).then(|| answer.trust.id_key_digest.clone()),
        id_key_digest: answer.trust.id_key_digest.clone(),
        min_guest_svn: answer.trust.min_guest_svn,
        min_tcb: answer.trust.min_tcb.clone(),
        command,
        version: release.version.to_owned(),
        downloads: release
            .tag
            .map(|_| downloads(&links.download, release.version))
            .unwrap_or_default(),
        source_url: links.source.replace("{ref}", &release.reference()),
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

/// The keys a form sent, in the form `backend` takes: refused, with what to
/// do instead, when they came in the clear for a backend that takes them
/// only encrypted (they are not sent on), or encrypted for one that takes
/// them as they are. `backend` is `None` for the engine's default, which is
/// taken from what is known of it.
pub fn store_keys(
    engine: &Engine,
    backend: Option<&str>,
    view_key_hex: &str,
    spend_pubkey_hex: &str,
    encrypted_keys: Option<&str>,
) -> Result<StoreKeys, String> {
    let encrypted = encrypted_keys
        .map(str::trim)
        .filter(|text| !text.is_empty());
    // No backend named: the engine's default, from its status. A status
    // listing no backends is an engine with a single plain one.
    let status_known = super::status_page::known_status_is_fresh(engine);
    let backend = backend.map(str::to_owned).or_else(|| {
        super::status_page::known_enabled_custody_backends(engine)
            .into_iter()
            .next()
    });
    if backend.is_none() && !status_known && encrypted.is_none() && !view_key_hex.trim().is_empty()
    {
        // The engine's default can't be known: it might take keys only
        // encrypted, so typed ones aren't sent on.
        return Err(
            "The engine isn't answering, so this store's keys were not sent. Try again in a minute."
                .to_owned(),
        );
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
        return Ok(StoreKeys {
            encrypted_keys: Some(encrypted.to_owned()),
            ..StoreKeys::default()
        });
    }
    if encrypted.is_some() && backend.is_some() {
        return Err(
            "Encrypted keys are only for SEV-SNP key storage: enter the keys themselves for this one.".to_owned(),
        );
    }
    Ok(StoreKeys {
        view_key_hex: view_key_hex.trim().to_owned(),
        spend_pubkey_hex: spend_pubkey_hex.trim().to_owned(),
        encrypted_keys: encrypted.map(str::to_owned),
    })
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

pub async fn script(headers: HeaderMap) -> Response {
    super::pay::static_asset(
        &headers,
        "text/javascript; charset=utf-8",
        super::pay::REVALIDATE,
        include_str!("../../static/key-custody.js").as_bytes(),
    )
}

pub async fn module(headers: HeaderMap) -> Response {
    super::pay::static_asset(
        &headers,
        "application/wasm",
        super::pay::REVALIDATE,
        include_bytes!(concat!(env!("OUT_DIR"), "/key_custody.wasm")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn status(
        backends: serde_json::Value,
        default: Option<&str>,
    ) -> crate::engine_client::EngineStatusResponse {
        serde_json::from_value(serde_json::json!({
            "networks": [],
            "poll_interval_secs": 5,
            "generated_at": 0,
            "key_custody": backends,
            "key_custody_default": default,
        }))
        .unwrap()
    }

    /// Typed keys go on only where the engine's default is known to take
    /// them: refused while its status is unknown or its default is snp, sent
    /// when it lists plain first, or no backends at all (a single plain one).
    #[test]
    fn typed_keys_are_sent_only_where_the_default_takes_them() {
        let view = "07".repeat(32);
        let spend = "08".repeat(32);
        let state = AppState::for_tests();
        let engine = &state.engine;
        assert!(store_keys(engine, None, &view, &spend, None)
            .unwrap_err()
            .contains("isn't answering"));

        super::super::status_page::seed_status_for_tests(
            engine,
            status(serde_json::json!([]), None),
        );
        let keys = store_keys(engine, None, &view, &spend, None).unwrap();
        assert_eq!(keys.view_key_hex, view);

        super::super::status_page::seed_status_for_tests(
            engine,
            status(
                serde_json::json!([{ "backend": "plain", "error": null }, { "backend": "snp", "error": null }]),
                Some("snp"),
            ),
        );
        assert!(store_keys(engine, None, &view, &spend, None)
            .unwrap_err()
            .contains("were not sent"));
        assert!(store_keys(engine, Some("plain"), &view, &spend, None).is_ok());
        assert_eq!(
            store_keys(engine, None, "", "", Some("{envelope}"))
                .unwrap()
                .encrypted_keys
                .as_deref(),
            Some("{envelope}")
        );
    }

    #[test]
    fn a_bundle_is_served_while_it_lasts() {
        let bundles = KeyBundles::default();
        let id = bundles.insert("{\"v\":1}".into());
        assert_eq!(bundles.get(&id).as_deref(), Some("{\"v\":1}"));
        assert_eq!(bundles.get("nope"), None);
        assert_eq!(id.len(), 32);
    }
}
