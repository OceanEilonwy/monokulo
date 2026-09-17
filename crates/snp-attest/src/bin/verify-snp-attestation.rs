//! WBS 2.2.1's own literal deliverable: "an attestation-verification
//! script - its pass/fail on the signature chain and reported patch level
//! is the test."
//!
//! USAGE:
//!   verify-snp-attestation --report <path> --product Milan|Genoa|Turin \
//!       [--min-bootloader-spl N] [--min-tee-spl N] [--min-snp-spl N] \
//!       [--min-microcode-spl N]
//!
//! `--report` is the raw 1184-byte binary `ATTESTATION_REPORT` blob (what
//! `/dev/sev-guest`'s `SNP_GET_REPORT` ioctl - or a provider's equivalent
//! wrapper, e.g. `snpguest report` - actually returns; this tool does not
//! itself fetch a report from the guest, so it works identically whether
//! you're checking the box you're running on or one someone handed you a
//! report file from).
//!
//! Exits 0 only if every check below passes; prints exactly what was and
//! wasn't verified either way, matching this repo's existing operator-facing
//! script convention (`scripts/backup-database.sh`/`restore-database.sh`).
//!
//! WHY THE --min-*-spl FLAGS ARE OPTIONAL, AND WHAT THAT MEANS:
//! This WBS item names one concrete target explicitly: "AMD's July 2025
//! microcode patch confirmed present" - which is AMD-SB-3019 (the
//! "StackWarp" SEV-SNP vulnerability, CVE-2025-29943), fixed by the
//! microcode batch AMD shipped 2025-07-29. That advisory's fix is expressed
//! as a minimum **microcode SPL** (Security Patch Level - a small per-
//! platform integer baked into the reported TCB and into the VCEK's own
//! signed certificate extensions, cryptographically checked above), not a
//! human-readable date - and the SPL number that corresponds to "patched"
//! differs per silicon (Milan vs. Genoa vs. Turin) and isn't something this
//! tool hardcodes as if it were a fixed, timeless fact, for the same reason
//! `restore-database.sh` doesn't hardcode a "safe" row count: it's real data
//! that must come from the actual deployment, not a guess baked in at
//! authorship time. What this tool *does* do unconditionally, with no flag
//! required: cryptographically prove the reported SPL values are genuine
//! (signed by AMD, not merely claimed) and print them, so confirming they
//! meet AMD-SB-3019's bar is a one-line comparison against whatever minimum
//! your specific hardware's own AMD/provider documentation states - pass
//! that number via `--min-microcode-spl` (and the sibling flags, if you want
//! bootloader/TEE/SNP-firmware floors enforced too) once you have it. Until
//! then this tool still does everything programmatically checkable: full
//! chain-of-trust verification plus printing the real, attested values for
//! a human to compare by hand - it only refuses to silently invent the
//! threshold itself.

use snp_attest::report::{self, Product};
use snp_attest::verify;
use std::process::ExitCode;

struct Args {
    report_path: String,
    product: Product,
    min_bootloader_spl: Option<u8>,
    min_tee_spl: Option<u8>,
    min_snp_spl: Option<u8>,
    min_microcode_spl: Option<u8>,
}

fn parse_args() -> Result<Args, String> {
    let mut report_path = None;
    let mut product = None;
    let mut min_bootloader_spl = None;
    let mut min_tee_spl = None;
    let mut min_snp_spl = None;
    let mut min_microcode_spl = None;

    let mut raw = std::env::args().skip(1);
    while let Some(flag) = raw.next() {
        let mut value = || raw.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--report" => report_path = Some(value()?),
            "--product" => {
                let v = value()?;
                product = Some(Product::parse(&v).ok_or_else(|| {
                    format!("unknown --product {v} (expected Milan, Genoa, or Turin)")
                })?);
            }
            "--min-bootloader-spl" => {
                min_bootloader_spl = Some(value()?.parse::<u8>().map_err(|e| e.to_string())?)
            }
            "--min-tee-spl" => {
                min_tee_spl = Some(value()?.parse::<u8>().map_err(|e| e.to_string())?)
            }
            "--min-snp-spl" => {
                min_snp_spl = Some(value()?.parse::<u8>().map_err(|e| e.to_string())?)
            }
            "--min-microcode-spl" => {
                min_microcode_spl = Some(value()?.parse::<u8>().map_err(|e| e.to_string())?)
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(Args {
        report_path: report_path.ok_or("--report <path> is required")?,
        product: product.ok_or("--product Milan|Genoa|Turin is required")?,
        min_bootloader_spl,
        min_tee_spl,
        min_snp_spl,
        min_microcode_spl,
    })
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("usage: verify-snp-attestation --report <path> --product Milan|Genoa|Turin [--min-bootloader-spl N] [--min-tee-spl N] [--min-snp-spl N] [--min-microcode-spl N]");
            return ExitCode::FAILURE;
        }
    };

    let report_bytes = match std::fs::read(&args.report_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: failed to read {}: {e}", args.report_path);
            return ExitCode::FAILURE;
        }
    };

    let parsed = match report::parse(&report_bytes, args.product) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: failed to parse attestation report: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!(
        "==> verifying attestation report against AMD KDS (direct-to-AMD, root pinned in-binary)"
    );
    let client = reqwest::Client::new();
    let outcome = match verify::verify(&client, args.product, &parsed).await {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: attestation verification FAILED: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("    chain ok: report signature verifies against a genuine AMD-issued VCEK, chained to the pinned AMD root");
    println!(
        "    reported_tcb: bootloader={} tee={} snp={} microcode={}{}",
        outcome.reported_tcb.bootloader,
        outcome.reported_tcb.tee,
        outcome.reported_tcb.snp,
        outcome.reported_tcb.microcode,
        outcome
            .reported_tcb
            .fmc
            .map(|f| format!(" fmc={f}"))
            .unwrap_or_default()
    );
    println!(
        "    current_tcb:  bootloader={} tee={} snp={} microcode={}{}",
        outcome.current_tcb.bootloader,
        outcome.current_tcb.tee,
        outcome.current_tcb.snp,
        outcome.current_tcb.microcode,
        outcome
            .current_tcb
            .fmc
            .map(|f| format!(" fmc={f}"))
            .unwrap_or_default()
    );

    let mut threshold_failures = Vec::new();
    let mut any_threshold_checked = false;
    let checks: [(&str, Option<u8>, u8); 4] = [
        (
            "bootloader",
            args.min_bootloader_spl,
            outcome.reported_tcb.bootloader,
        ),
        ("tee", args.min_tee_spl, outcome.reported_tcb.tee),
        ("snp", args.min_snp_spl, outcome.reported_tcb.snp),
        (
            "microcode",
            args.min_microcode_spl,
            outcome.reported_tcb.microcode,
        ),
    ];
    for (name, min, actual) in checks {
        if let Some(min) = min {
            any_threshold_checked = true;
            if actual < min {
                threshold_failures.push(format!(
                    "{name} SPL {actual} is below the required minimum {min}"
                ));
            }
        }
    }

    if !any_threshold_checked {
        println!("    NOTE: no --min-*-spl flags were given, so no minimum-patch-level threshold was enforced.");
        println!("          the values above are cryptographically genuine (AMD-signed) - compare microcode={} against", outcome.reported_tcb.microcode);
        println!("          the minimum SPL your hardware vendor documents for AMD-SB-3019 (CVE-2025-29943, fixed by AMD's");
        println!("          2025-07-29 microcode release) to confirm it by hand, or re-run with --min-microcode-spl once known.");
    } else if !threshold_failures.is_empty() {
        eprintln!("error: reported TCB is below the required minimum patch level:");
        for f in &threshold_failures {
            eprintln!("    {f}");
        }
        return ExitCode::FAILURE;
    } else {
        println!("    all supplied --min-*-spl thresholds satisfied.");
    }

    println!("==> PASS: report is genuinely SEV-SNP-attested by AMD.");
    ExitCode::SUCCESS
}
