//! The key fields of a form that may send a store's keys to the engine's
//! SEV-SNP key storage, which takes them only encrypted to it
//! (`http::key_entry`).
//!
//! The view key and spend key fields are the same as for any store. With
//! JavaScript, `key-custody.js` encrypts what is typed in them before the
//! form is sent and clears them; without it, the merchant encrypts them with
//! `key-custody-cli` and pastes the result into the `encrypted_keys` field.

use maud::{html, Markup};

/// One download of key-custody-cli.
pub struct CliDownload {
    pub label: String,
    pub url: String,
    pub checksum_url: String,
}

/// A bundle to encrypt against, and how to do it without JavaScript.
pub struct SnpReady {
    /// The bundle, for the page's script.
    pub bundle_json: String,
    /// Where `key-custody-cli` (or the merchant, saving it) gets it.
    pub bundle_path: String,
    /// Whether the command names a saved file (this instance has no public
    /// address to fetch the bundle from) rather than the address.
    pub bundle_is_file: bool,
    /// The ID key digest to trust, when it isn't the official one.
    pub trust_id_key: Option<String>,
    /// The ID key digest to trust, for the page's script, when it isn't the
    /// official one built into the checker.
    pub id_key_digest: Option<String>,
    pub min_guest_svn: u32,
    /// The lowest firmware trusted, for the page's script.
    pub min_tcb: String,
    /// The command to run.
    pub command: String,
    /// This monokulo's version: the key-custody-cli release that matches it.
    pub version: String,
    /// Its downloads; empty for a build that isn't a release.
    pub downloads: Vec<CliDownload>,
    pub source_url: String,
    /// The command that checks a download was built by this project's
    /// release workflow (GitHub build provenance), when it is on GitHub.
    pub verify_command: Option<String>,
}

pub enum SnpKeyEntry {
    Ready(Box<SnpReady>),
    /// Why encrypted key entry can't be offered right now.
    Unavailable(String),
    /// This account has opened too many key entry forms; shown as an alert.
    Limited(String),
}

/// The view key and spend key fields. `required` unless the keys may go
/// encrypted instead (the server checks which it got).
pub fn key_fields(
    view_key_hex: &str,
    spend_pubkey_hex: &str,
    snp: Option<&SnpKeyEntry>,
    view_help: Markup,
    spend_help: Markup,
) -> Markup {
    let required = snp.is_none();
    html! {
        label {
            "View key (hex)"
            input type="password" name="view_key_hex" value=(view_key_hex) required[required] pattern="[0-9a-fA-F]{64}" autocomplete="off" placeholder="64 hex characters" data-key-custody="view";
            span class="field-help" { (view_help) }
        }
        label {
            "Spend public key (hex)"
            input type="text" name="spend_pubkey_hex" value=(spend_pubkey_hex) required[required] pattern="[0-9a-fA-F]{64}" autocomplete="off" placeholder="64 hex characters" data-key-custody="spend";
            span class="field-help" { (spend_help) }
        }
    }
}

/// The encrypted key entry: what it protects, and the key-custody-cli steps
/// for a browser without JavaScript. `backend_field` names the form's
/// backend choice, if it has one (the script encrypts only when it says
/// `snp`).
pub fn snp_section(entry: &SnpKeyEntry, backend_field: Option<&str>) -> Markup {
    match entry {
        SnpKeyEntry::Unavailable(why) => html! {
            p class="warning" role="status" { (why) }
        },
        SnpKeyEntry::Limited(why) => html! {
            div class="error" role="alert" { (why) }
        },
        SnpKeyEntry::Ready(ready) => ready_section(ready, backend_field),
    }
}

fn ready_section(ready: &SnpReady, backend_field: Option<&str>) -> Markup {
    html! {
        div class="box" data-key-custody-bundle=(ready.bundle_json) data-key-custody-id-key=[ready.id_key_digest.as_deref()] data-key-custody-min-svn=(ready.min_guest_svn) data-key-custody-min-tcb=(ready.min_tcb) data-key-custody-backend-field=[backend_field] {
            h3 { "SEV-SNP key storage: your keys are encrypted for the engine" }
            p class="hint" {
                "With SEV-SNP key storage, your keys are encrypted so that only the engine, running in an AMD "
                "SEV-SNP confidential machine, can read them. This server only passes them on. In this browser, "
                "the page checks the engine's attestation and encrypts the keys you typed above when you submit."
            }
            p class="notice" role="status" data-key-custody-status hidden {}
            @if let Some(digest) = &ready.trust_id_key {
                p class="warning" {
                    "This instance's engine image is not signed by the official monokulo key. It is trusted "
                    "here by the digest " code { (digest) } ". Before you rely on it, confirm that digest "
                    "with this instance's operator another way, not from this page."
                }
            }
            details class="help-disclosure" {
                summary { "Without JavaScript, or for the strongest guarantee: use key-custody-cli" }
                p class="hint" {
                    "key-custody-cli runs on your own computer and checks the engine against AMD's root "
                    "certificate and the engine signing key built into it, so it doesn't depend on this site."
                }
                ol class="steps" {
                    li {
                        @if ready.downloads.is_empty() {
                            "This monokulo (" (ready.version) ") isn't a release build, so there's no download for it: "
                            "build key-custody-cli from its source: "
                            a href=(ready.source_url) rel="noopener" target="_blank" { "key-custody-cli source" } "."
                        } @else {
                            "Download key-custody-cli " (ready.version) " (the version that matches this site) for your computer:"
                            ul {
                                @for download in &ready.downloads {
                                    li {
                                        a href=(download.url) rel="noopener" { (download.label) }
                                        " (" a href=(download.checksum_url) rel="noopener" { "SHA-256" } ")"
                                    }
                                }
                            }
                            @if let Some(verify) = &ready.verify_command {
                                "Check the file was built by this project's release, not just served next to its checksum: "
                                code { (verify) } ". "
                            }
                            "Or read and build it from its "
                            a href=(ready.source_url) rel="noopener" target="_blank" { "source" } "."
                        }
                    }
                    @if ready.bundle_is_file {
                        li {
                            "Save " a href=(ready.bundle_path) download="key-custody-bundle.json" { "this form's bundle" }
                            " as key-custody-bundle.json, in the folder you run the command from."
                        }
                    }
                    li {
                        "Run this, and type your view key and spend public key when it asks:"
                        pre class="dns-record" { code { (ready.command) } }
                    }
                    li {
                        label {
                            "Paste what it prints here"
                            textarea name="encrypted_keys" rows="4" autocomplete="off" spellcheck="false" {}
                            span class="field-help" {
                                "Leave the key fields above empty when you use this. The bundle is for this form "
                                "only: if the page is reloaded, run the command again."
                            }
                        }
                    }
                }
            }
            script src="/static/key-custody.js" defer {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(trust_id_key: Option<&str>, downloads: Vec<CliDownload>) -> SnpReady {
        SnpReady {
            bundle_json: "{\"v\":1}".into(),
            bundle_path: "/key-custody/bundles/abc".into(),
            bundle_is_file: false,
            trust_id_key: trust_id_key.map(str::to_owned),
            id_key_digest: Some("ab".repeat(48)),
            min_guest_svn: 0,
            min_tcb: String::new(),
            command:
                "key-custody-cli seal --bundle https://pay.example.com/key-custody/bundles/abc"
                    .into(),
            version: "1.2.3".into(),
            downloads,
            source_url: "https://example.com/tree/v1.2.3/crates/key-custody-cli".into(),
            verify_command: Some("gh attestation verify <file> --repo o/r".into()),
        }
    }

    #[test]
    fn a_release_links_its_own_cli_and_the_command_to_run() {
        let html = snp_section(
            &SnpKeyEntry::Ready(Box::new(ready(
                None,
                vec![CliDownload {
                    label: "Linux (x86-64)".into(),
                    url: "https://example.com/key-custody-cli-1.2.3-x86_64-unknown-linux-gnu.tar.gz".into(),
                    checksum_url: "https://example.com/key-custody-cli-1.2.3-x86_64-unknown-linux-gnu.tar.gz.sha256".into(),
                }],
            ))),
            Some("key_custody_backend"),
        )
        .into_string();
        assert!(html.contains("key-custody-cli 1.2.3"), "{html}");
        assert!(
            html.contains("x86_64-unknown-linux-gnu.tar.gz.sha256"),
            "{html}"
        );
        assert!(
            html.contains(
                "key-custody-cli seal --bundle https://pay.example.com/key-custody/bundles/abc"
            ),
            "{html}"
        );
        assert!(html.contains(r#"name="encrypted_keys""#), "{html}");
        assert!(
            html.contains(r#"data-key-custody-backend-field="key_custody_backend""#),
            "{html}"
        );
        assert!(html.contains("/static/key-custody.js"), "{html}");
        assert!(!html.contains("not signed by the official"), "{html}");
    }

    #[test]
    fn a_build_that_is_not_a_release_points_at_the_source_and_a_custom_key_is_named() {
        let html = snp_section(
            &SnpKeyEntry::Ready(Box::new(ready(Some("cdcd"), vec![]))),
            None,
        )
        .into_string();
        assert!(html.contains("isn't a release build"), "{html}");
        assert!(
            html.contains("tree/v1.2.3/crates/key-custody-cli"),
            "{html}"
        );
        assert!(
            html.contains("not signed by the official monokulo key"),
            "{html}"
        );
        assert!(html.contains("cdcd"), "{html}");
    }

    #[test]
    fn keys_are_required_only_when_they_must_be_typed() {
        let plain = key_fields("", "", None, html! {}, html! {}).into_string();
        assert_eq!(plain.matches(" required").count(), 2, "{plain}");
        let entry = SnpKeyEntry::Unavailable("later".into());
        let either = key_fields("", "", Some(&entry), html! {}, html! {}).into_string();
        assert!(!either.contains(" required"), "{either}");
    }
}
