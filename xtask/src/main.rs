mod coverage;
mod exploration;
mod logo;
mod mutations;
mod quality;
mod rounds;
mod snp;
mod stress;
mod summary;
mod validate;

use std::{
    env, io,
    path::{Path, PathBuf},
    process::ExitCode,
};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn help() {
    println!("Usage: cargo xtask coverage <rust|browser|woocommerce|stagenet|all|report|summary|validate|open>\n       cargo xtask stress <ci|full|scale|open> [driver]\n       cargo xtask stress rounds\n       cargo xtask snp-id-key [--from-env]\n       cargo xtask snp-id-block ...\n       cargo xtask logo\n       cargo xtask engine <fuzz|properties|scale|mutations> [...]\n       cargo xtask test-summary TITLE LABEL=JUNIT...\n       cargo xtask pages <build|fetch> [...]\n       cargo xtask serve DIR [PORT]\n\n\
        rust          Refresh nightly and cargo-llvm-cov; run workspace tests and collect Rust coverage\n\
        browser       Run deterministic Playwright tests and collect authored browser source coverage\n\
        woocommerce   Run default PHPUnit tests in wp-env and collect plugin coverage\n\
        stagenet      Explicit extended run: paid browser tests and separate instrumented report\n\
        all           Run rust, browser and woocommerce side by side; preserve successful reports if another fails\n\
        report        Combine rust, browser and woocommerce outputs already in target/coverage (as CI's\n\
                      separate jobs leave them) into one index; validate it once all three passed\n\
        summary       Print the coverage components as a GitHub job-summary table (Markdown)\n\
        validate      Check target/coverage: manifests, report links, screenshots, sources, line floors\n\
        open          Open target/coverage/index.html in the default browser\n\
        stress ci     One-CPU scanner capacity sweep and fault recovery (docs/engine_stress.md)\n\
        stress full   The same with larger tenant counts\n\
        stress scale  Thousands of tenants; observational latency and capacity\n\
        stress open   Open target/coverage/stress/index.html\n\
        stress rounds Round length sweep: throughput, refetches and waits (docs/engine_stress.md)\n\
        snp-id-key    Make the engine image ID key: prints it (for the SNP_ID_KEY secret) and writes its\n\
                      digest to crates/key-custody; --from-env writes the digest of SNP_ID_KEY's key\n\
        snp-id-block --measurement HEX --guest-svn N --out DIR [--family-id HEX] [--image-id HEX] [--policy HEX]\n\
                      Sign an engine image's ID block with SNP_ID_KEY (deploy/sev-snp/README.md)\n\
        [driver]      The scanner entry point to measure (default: the production one)\n\
        test-summary TITLE LABEL=JUNIT...\n\
                      Print JUnit reports (nextest, Playwright, PHPUnit) as a GitHub job-summary table with\n\
                      their failures; a missing report is a row that says so\n\
        --help        Show this help");
    println!("        logo          Draw the Monokulo mark and write every copy of it (xtask/src/logo.rs)");
    println!("{}", exploration::HELP);
    println!("{}", mutations::HELP);
    println!("{}", quality::HELP);
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let wants_help = args.iter().any(|a| a == "--help" || a == "-h");
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        _ if wants_help => {
            help();
            return ExitCode::SUCCESS;
        }
        ["coverage", command] => coverage::coverage(command),
        ["stress", "rounds"] => rounds::run(),
        ["stress", profile] => stress::run(profile, None),
        ["stress", profile, driver] => stress::run(profile, Some(driver)),
        ["snp-id-key"] => snp::id_key(&root(), false).map_err(io::Error::other),
        ["snp-id-key", "--from-env"] => snp::id_key(&root(), true).map_err(io::Error::other),
        ["snp-id-block", rest @ ..] => snp::id_block(rest).map_err(io::Error::other),
        ["engine", command, rest @ ..] => {
            let tools = exploration::Tools::default();
            // fuzz, properties and scale end with their tests' own exit code.
            let code = match *command {
                "fuzz" => exploration::fuzz(&root(), rest, &tools),
                "properties" => exploration::properties(&root(), rest, &tools),
                "scale" => exploration::scale(&root(), rest, &tools),
                "mutations" => mutations::mutations(&root(), rest).map(|ok| if ok { 0 } else { 1 }),
                _ => {
                    help();
                    return ExitCode::FAILURE;
                }
            };
            match code {
                Ok(code) => return ExitCode::from(code.clamp(0, 255) as u8),
                Err(e) => Err(e),
            }
        }
        ["logo"] => logo::write(&root()),
        ["test-summary", title, suites @ ..] if !suites.is_empty() => {
            print!("{}", summary::test_summary(title, suites));
            Ok(true)
        }
        ["pages", "build", rest @ ..] => quality::site(&root(), rest),
        ["pages", "fetch", rest @ ..] => quality::pages_inputs(rest),
        ["serve", rest @ ..] => quality::serve(rest),
        _ => {
            help();
            return ExitCode::FAILURE;
        }
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}
