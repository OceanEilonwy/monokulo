//! `key-custody-cli`: see the library's docs (`src/lib.rs`).

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use zeroize::Zeroizing;

#[derive(Parser)]
#[command(
    name = "key-custody-cli",
    version,
    about = "Encrypts a store's watch-only keys for a monokulo engine's SEV-SNP key storage, after checking the engine is genuine."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Checks the engine's bundle, asks for the keys, and prints them
    /// encrypted for that engine, to paste into the key entry form.
    Seal {
        /// The bundle: the address the key entry form shows, or a saved
        /// key-custody-bundle.json.
        #[arg(long)]
        bundle: String,
        /// Trust engine images signed by this ID key (96 hex characters)
        /// instead of the official one. Only for an instance that runs its
        /// own engine build: get the digest from its operator, not from the
        /// site.
        #[arg(long)]
        trust_id_key: Option<String>,
        /// Refuse engine images below this security version.
        #[arg(long, default_value_t = 0)]
        min_guest_svn: u32,
    },
}

fn main() -> ExitCode {
    let Cli { command } = Cli::parse();
    let Command::Seal {
        bundle,
        trust_id_key,
        min_guest_svn,
    } = command;
    match seal(&bundle, trust_id_key.as_deref(), min_guest_svn) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("key-custody-cli: {e}");
            ExitCode::FAILURE
        }
    }
}

fn seal(source: &str, trust_id_key: Option<&str>, min_guest_svn: u32) -> Result<(), String> {
    let (policy, custom) = key_custody_cli::trust_policy(trust_id_key, min_guest_svn)?;
    if custom {
        eprintln!(
            "WARNING: trusting engine images signed by the ID key {} given with --trust-id-key, not the official \
             monokulo key. Only do this if the instance's operator gave you that digest directly.",
            hex::encode(policy.id_key_digest)
        );
    }
    let bundle = key_custody_cli::load_bundle(source, fetch)?;
    let verified = key_custody_cli::check(&bundle, &policy, now())?;
    eprintln!("{}", key_custody_cli::describe(&verified, &policy, custom));

    let (view, spend) = read_keys()?;
    let keys = key_custody_cli::parse_keys(&view, &spend)?;
    let sealed = key_custody_cli::seal(&verified, &keys)?;
    eprintln!("Paste the line below into the key entry form's encrypted keys field:");
    println!("{sealed}");
    Ok(())
}

/// The view key (hidden as it's typed) and the spend public key. From a
/// pipe, one per line.
fn read_keys() -> Result<(Zeroizing<String>, Zeroizing<String>), String> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let mut lines = stdin.lock().lines();
        let mut next = |what: &str| -> Result<Zeroizing<String>, String> {
            lines
                .next()
                .transpose()
                .map_err(|e| e.to_string())?
                .map(Zeroizing::new)
                .ok_or_else(|| format!("expected the {what} on standard input"))
        };
        return Ok((next("private view key")?, next("public spend key")?));
    }
    let view = Zeroizing::new(
        rpassword::prompt_password("Private view key (hex, hidden): ")
            .map_err(|e| e.to_string())?,
    );
    eprint!("Public spend key (hex): ");
    std::io::stderr().flush().map_err(|e| e.to_string())?;
    let mut spend = Zeroizing::new(String::new());
    stdin
        .lock()
        .read_line(&mut spend)
        .map_err(|e| e.to_string())?;
    Ok((view, spend))
}

fn fetch(url: &str) -> Result<String, String> {
    let response = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?
        .get(url)
        .send()
        .map_err(|e| format!("fetching {url}: {e}"))?;
    let status = response.status();
    let body = response.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("fetching {url}: {status}: {}", body.trim()));
    }
    Ok(body)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
