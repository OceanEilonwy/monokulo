mod coverage;
mod exploration;
mod logo;
mod mutations;
mod pages;
mod recording;
mod rounds;
mod snp;
mod stress;
mod summary;
mod support;
mod timings;
mod validate;

use std::{env, io, process::ExitCode};
use support::{root, Exit};

const USAGE: &str = "\
Usage: cargo xtask coverage <rust|browser|woocommerce|stagenet|all|report|summary|validate|open>
       cargo xtask stress <ci|full|scale|open> [driver]
       cargo xtask stress rounds
       cargo xtask snp-id-key [--from-env]
       cargo xtask snp-id-block ...
       cargo xtask logo
       cargo xtask engine <fuzz|properties|scale|mutations> [...]
       cargo xtask test-summary TITLE LABEL=JUNIT...
       cargo xtask test-timings [--db PATH] <run|load|report> [...]
       cargo xtask pages <build|fetch> [...]
       cargo xtask record-stagenet-node
       cargo xtask serve DIR [PORT]
";

/// Every module's HELP, in the order the usage lines name the commands.
fn help() {
    println!("{USAGE}");
    for text in [
        coverage::HELP,
        stress::HELP,
        rounds::HELP,
        snp::HELP,
        logo::HELP,
        exploration::HELP,
        mutations::HELP,
        summary::HELP,
        timings::HELP,
        pages::HELP,
        recording::HELP,
    ] {
        println!("{text}");
    }
    println!("        --help        Show this help");
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        help();
        return ExitCode::SUCCESS;
    }
    let result: io::Result<Exit> = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["coverage", command] => coverage::coverage(command),
        ["stress", "rounds"] => rounds::run().map(Exit::passed),
        ["stress", profile] => stress::run(profile, None).map(Exit::passed),
        ["stress", profile, driver] => stress::run(profile, Some(driver)).map(Exit::passed),
        ["snp-id-key"] => snp::id_key(&root(), false)
            .map(Exit::passed)
            .map_err(io::Error::other),
        ["snp-id-key", "--from-env"] => snp::id_key(&root(), true)
            .map(Exit::passed)
            .map_err(io::Error::other),
        ["snp-id-block", rest @ ..] => snp::id_block(rest)
            .map(Exit::passed)
            .map_err(io::Error::other),
        ["engine", "fuzz", rest @ ..] => {
            exploration::fuzz(&root(), rest, &exploration::Tools::default())
        }
        ["engine", "properties", rest @ ..] => {
            exploration::properties(&root(), rest, &exploration::Tools::default())
        }
        ["engine", "scale", rest @ ..] => {
            exploration::scale(&root(), rest, &exploration::Tools::default())
        }
        ["engine", "mutations", rest @ ..] => mutations::mutations(&root(), rest),
        ["logo"] => logo::write(&root()),
        ["test-summary", title, suites @ ..] if !suites.is_empty() => {
            print!("{}", summary::test_summary(title, suites));
            Ok(Exit::SUCCESS)
        }
        ["test-timings", rest @ ..] => timings::timings(rest),
        ["pages", "build", rest @ ..] => pages::build(&root(), rest),
        ["pages", "fetch", rest @ ..] => pages::fetch(rest),
        ["pages", "docs", rest @ ..] => pages::docs(&root(), rest),
        ["serve", rest @ ..] => pages::serve(rest),
        ["record-stagenet-node"] => recording::record(&root()),
        _ => {
            help();
            return ExitCode::FAILURE;
        }
    };
    match result {
        Ok(exit) => exit.into(),
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}
