//! `cargo xtask live`: the daily live-network run (live-network.yml). Every
//! test that needs the outside world (a Monero node, an exchange-rate API,
//! DNS, Tor, or the funded stagenet wallets) is `#[ignore]`d, so no change
//! waits on a service someone else runs, and named `live_`, so this run can
//! pick out exactly those and never the scale suites, which are ignored too.
//! A test in this file checks that every ignored test is either named
//! `live_` or on [`NOT_DAILY`] with the reason it stays out, so a new live
//! test can't be forgotten.
//!
//! Before the tests run, each service they need is probed, so a failure can
//! be told apart: a test that failed while its service didn't answer is the
//! service's problem, not the code's. The wallets are a service too: the run
//! checks they hold enough for one run's payments before it starts, and
//! afterwards sweeps what the merchant received back to the spender and
//! splits the spender's outputs, so the pair keeps itself funded.

use crate::pages::{junit, Test, TestStatus};
use crate::summary::ignored_tests;
use crate::support::{at, read_json, write_json, Exit};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env, fmt, fs, io,
    net::ToSocketAddrs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub(crate) const HELP: &str = "\
        live tests    Run every #[ignore]d live_ test: they need the network, the stagenet node and the funded\n\
                      test wallets ($E2E_WALLET_DIR, else e2e/wallets); JUnit into target/live\n\
        live browser  Run the paid stagenet browser tests (pos-stagenet-payments.spec.js); JUnit into target/live\n\
        live probe    Check each outside service the live tests need answers (target/live/services.json)\n\
        live wallets check [--from HEIGHT]\n\
                      Rescan the test wallets (the last 1500 blocks, or from HEIGHT) and stop, saying what to\n\
                      do, unless the spender can pay one run's orders (target/live/wallets.json)\n\
        live wallets keep\n\
                      Sweep the merchant's unlocked balance back to the spender, and split or merge the\n\
                      spender's outputs so the next run has enough of the right size\n\
        live report   Join the run's JUnit reports, probes and wallets into target/live/live.json and print\n\
                      the services, wallets and each failure's cause as a GitHub job summary";

/// A test the daily run runs is named with this prefix, and `#[ignore]`d.
const PREFIX: &str = "live_";

/// The nextest filter for those tests: the last segment of the test's path
/// starts with [`PREFIX`]. It runs with `--run-ignored only`.
fn filter() -> String {
    format!("test(/(^|::){PREFIX}[^:]*$/)")
}
/// The crates' features the live tests are built behind.
const FEATURES: &str = "monokulo/snp,e2e-harness/e2e,mock-woocommerce/e2e";

/// How a service is checked before the tests run.
enum Probe {
    /// Any HTTP answer below 500 other than 429 means it's serving.
    Get(&'static [&'static str]),
    /// The same, for a node's JSON endpoints, which are posted to.
    Post(&'static [&'static str]),
    /// The node's `get_height`, posted: TLS, with its own certificate.
    MainnetNode,
    /// The system resolver finds this host.
    Dns(&'static str),
    /// The wallets' own check (`live wallets check`), read from its file.
    Wallets,
    /// Nothing short of the test itself says whether it works.
    None,
}

/// An outside service the live tests need.
struct Service {
    id: &'static str,
    name: &'static str,
    probe: Probe,
}

const SERVICES: [Service; 8] = [
    Service {
        id: "mainnet-node",
        name: "Monero mainnet node",
        probe: Probe::MainnetNode,
    },
    Service {
        id: "stagenet-node",
        name: "Monero stagenet nodes (monerodevs.org)",
        probe: Probe::Post(&[
            "http://node.monerodevs.org:38089/get_height",
            "http://node2.monerodevs.org:38089/get_height",
        ]),
    },
    Service {
        id: "wallets",
        name: "Funded stagenet test wallets",
        probe: Probe::Wallets,
    },
    Service {
        id: "coingecko",
        name: "Coingecko API",
        probe: Probe::Get(&["https://api.coingecko.com/api/v3/ping"]),
    },
    Service {
        id: "coinmarketcap",
        name: "CoinMarketCap public API",
        probe: Probe::Get(&["https://pro-api.coinmarketcap.com/public-api/"]),
    },
    Service {
        id: "haveno",
        name: "haveno.markets",
        probe: Probe::Get(&["https://haveno.markets/"]),
    },
    Service {
        id: "dns",
        name: "DNS",
        probe: Probe::Dns("google.com"),
    },
    Service {
        id: "tor",
        name: "Tor network",
        probe: Probe::None,
    },
];

/// The services each live test's `#[ignore]` reason names. Every live test
/// gives one of these reasons word for word (a test checks), so the run
/// knows what each one needs from the reason it already has to give.
const NEEDS: [(&str, &[&str]); 8] = [
    ("needs a live mainnet node", &["mainnet-node"]),
    ("needs the live stagenet node", &["stagenet-node"]),
    (
        "needs the live stagenet node and the funded test wallets",
        &["stagenet-node", "wallets"],
    ),
    ("needs the live Coingecko API", &["coingecko"]),
    ("needs the live CoinMarketCap API", &["coinmarketcap"]),
    ("needs the live haveno.markets API", &["haveno"]),
    ("needs live DNS", &["dns"]),
    ("needs tor and the live Tor network", &["tor"]),
];
/// What every paid browser test needs: each pays a real order.
const BROWSER_NEEDS: &str = "needs the live stagenet node and the funded test wallets";

/// Payments one run makes: the four Rust tests' and the two browser tests'.
pub(crate) const PAYMENTS: usize = 6;
/// The smallest output that pays one test order (0.000336 XMR at most)
/// and its fee, in piconero.
const PIECE: u64 = 500_000_000;
/// What the WooCommerce tests insist the spender holds unlocked before they
/// pay: 0.01 XMR.
const MIN_UNLOCKED: u64 = 10_000_000_000;
/// Blocks an output stays locked after the block that made it.
const LOCK_BLOCKS: u64 = 10;
/// How far back a routine rescan looks: two days of stagenet blocks, so the
/// day's sweep and anything a cancelled run left unrecorded are found.
const RESCAN_BLOCKS: u64 = 1500;
/// Where the faucet that funded the wallets is.
const FAUCET: &str = "https://stagenet-faucet.xmr-tw.org/";

/// The run's results, beside the target folder's other outputs.
fn out_dir(root: &Path) -> PathBuf {
    root.join("target/live")
}

pub(crate) fn live(root: &Path, args: &[&str]) -> io::Result<Exit> {
    let out = out_dir(root);
    fs::create_dir_all(&out).map_err(|e| at(&out, e))?;
    match args {
        ["tests"] => tests(root, &out),
        ["browser"] => browser(root, &out),
        ["probe"] => probe(&out),
        ["wallets", "check"] => check_wallets(root, &out, None),
        ["wallets", "check", "--from", height] => {
            let height = height.parse().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "--from takes a block height")
            })?;
            check_wallets(root, &out, Some(height))
        }
        ["wallets", "keep"] => keep_wallets(root, &out),
        ["report"] => report(root, &out),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: cargo xtask live <tests|browser|probe|wallets check|wallets keep|report>",
        )),
    }
}

fn tests(root: &Path, out: &Path) -> io::Result<Exit> {
    let status = Command::new("cargo")
        .args([
            "nextest",
            "run",
            "--workspace",
            "--locked",
            "--profile",
            "live",
        ])
        .args([
            "--features",
            FEATURES,
            "--run-ignored",
            "only",
            "-E",
            &filter(),
        ])
        .current_dir(root)
        .status()?;
    let junit = root.join("target/nextest/live/junit.xml");
    if junit.is_file() {
        let to = out.join("rust-junit.xml");
        fs::copy(&junit, &to).map_err(|e| at(&to, e))?;
    }
    Ok(Exit::of(status))
}

/// The paid browser tests with their own config (playwright.config.js), not
/// the coverage one: this run checks the services, so it needs no
/// instrumentation or screenshots.
fn browser(root: &Path, out: &Path) -> io::Result<Exit> {
    let dir = root.join("e2e/browser");
    let status = Command::new(dir.join("node_modules/.bin/playwright"))
        .args([
            "test",
            "-c",
            "playwright.config.js",
            "--reporter=list,junit",
        ])
        .env(
            "PLAYWRIGHT_JUNIT_OUTPUT_FILE",
            out.join("browser-junit.xml"),
        )
        .current_dir(&dir)
        .status()?;
    Ok(Exit::of(status))
}

/// One service's state when the run checked it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct Check {
    pub(crate) id: String,
    pub(crate) name: String,
    /// Whether it answered; nothing when it isn't probed.
    pub(crate) reachable: Option<bool>,
    pub(crate) detail: String,
}

/// Whether an HTTP status means the service is serving: a 404 for the
/// probe's own path still does; a 5xx or rate limit does not.
fn serving(code: u16) -> bool {
    (100..500).contains(&code) && code != 429
}

/// Asks `url` with curl, as `method` (a POST sends `{}`, as a node's JSON
/// endpoints take it).
fn http(method: &str, url: &str) -> (Option<bool>, String) {
    let mut command = Command::new("curl");
    command.args([
        "-sS",
        "-k",
        "-o",
        "/dev/null",
        "-m",
        "20",
        "-w",
        "%{http_code}",
    ]);
    if method == "POST" {
        command.args(["-H", "Content-Type: application/json", "-d", "{}"]);
    }
    match command.arg(url).output() {
        Err(e) => (None, format!("not probed: curl is missing ({e})")),
        Ok(output) => {
            let code: u16 = String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse()
                .unwrap_or(0);
            if code == 0 {
                let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
                (Some(false), format!("{url}: {error}"))
            } else {
                (Some(serving(code)), format!("{url} answered HTTP {code}"))
            }
        }
    }
}

/// Every probe's answer, joined: reachable only if every one answered.
fn joined(answers: Vec<(Option<bool>, String)>) -> (Option<bool>, String) {
    let reachable = answers
        .iter()
        .map(|(r, _)| *r)
        .try_fold(true, |all, r| r.map(|r| all && r));
    let detail = answers
        .into_iter()
        .map(|(_, d)| d)
        .collect::<Vec<_>>()
        .join("; ");
    (reachable, detail)
}

fn check(service: &Service) -> Check {
    let (reachable, detail) = match &service.probe {
        Probe::Get(urls) => joined(urls.iter().map(|url| http("GET", url)).collect()),
        Probe::Post(urls) => joined(urls.iter().map(|url| http("POST", url)).collect()),
        Probe::MainnetNode => {
            // The node the tests use: theirs unless ENGINE_LIVE_TEST_NODE
            // names another (`host:port`, TLS).
            let node = env::var("ENGINE_LIVE_TEST_NODE")
                .unwrap_or_else(|_| "node.hollingworth.xyz:18089".to_string());
            http("POST", &format!("https://{node}/get_height"))
        }
        Probe::Dns(host) => match (*host, 443).to_socket_addrs() {
            Ok(mut addresses) => match addresses.next() {
                Some(address) => (Some(true), format!("{host} is {}", address.ip())),
                None => (Some(false), format!("{host} has no address")),
            },
            Err(e) => (Some(false), format!("{host}: {e}")),
        },
        Probe::Wallets => (
            None,
            "checked by their own step, `cargo xtask live wallets check`".to_string(),
        ),
        Probe::None => (
            None,
            "only the test itself can tell whether it works".to_string(),
        ),
    };
    Check {
        id: service.id.to_string(),
        name: service.name.to_string(),
        reachable,
        detail,
    }
}

fn probe(out: &Path) -> io::Result<Exit> {
    let checks: Vec<Check> = SERVICES.iter().map(check).collect();
    for c in &checks {
        eprintln!("{}: {} ({})", c.name, state(c.reachable), c.detail);
    }
    write_json(&out.join("services.json"), &checks)?;
    Ok(Exit::SUCCESS)
}

fn state(reachable: Option<bool>) -> &'static str {
    match reachable {
        Some(true) => "reachable",
        Some(false) => "unreachable",
        None => "not probed",
    }
}

/// An amount in piconero, as XMR with every decimal, as wallet-cli prints it.
pub(crate) struct Xmr(pub(crate) u64);

impl fmt::Display for Xmr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{:012}",
            self.0 / 1_000_000_000_000,
            self.0 % 1_000_000_000_000
        )
    }
}

/// `0.060000000000` in piconero.
fn piconero(xmr: &str) -> Option<u64> {
    let (whole, fraction) = xmr.trim().split_once('.').unwrap_or((xmr.trim(), ""));
    if fraction.len() > 12 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let whole: u64 = whole.parse().ok()?;
    let fraction: u64 = format!("{fraction:0<12}").parse().ok()?;
    whole.checked_mul(1_000_000_000_000)?.checked_add(fraction)
}

/// `balance`'s total and unlocked figures.
fn parse_balance(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().find(|l| l.starts_with("Balance: "))?;
    let (total, unlocked) = line
        .strip_prefix("Balance: ")?
        .split_once(", unlocked balance: ")?;
    Some((piconero(total)?, piconero(unlocked)?))
}

/// `address`'s primary address: the first line is `0  <address>  <label>`.
fn parse_address(text: &str) -> Option<String> {
    text.lines()
        .next()?
        .split_whitespace()
        .nth(1)
        .map(str::to_string)
}

/// `unspent_outputs`' rows: each output's amount and height. The
/// histogram after them has a `|` in each line.
fn parse_outputs(text: &str) -> Vec<(u64, u64)> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let (amount, height) = (words.next()?, words.next()?);
            if words.next().is_some() || !amount.contains('.') {
                return None;
            }
            Some((piconero(amount)?, height.parse().ok()?))
        })
        .collect()
}

/// A wallet as the run found it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct Wallet {
    pub(crate) role: String,
    pub(crate) address: String,
    /// In piconero.
    pub(crate) balance: u64,
    pub(crate) unlocked: u64,
    pub(crate) outputs: usize,
    /// Unlocked outputs big enough to pay one test order.
    pub(crate) ready: usize,
}

/// `wallets.json`: the pair before the tests and after the upkeep.
#[derive(Serialize, Deserialize, Default, Debug)]
pub(crate) struct Wallets {
    /// The wallet directory, as `E2E_WALLET_DIR` named it.
    pub(crate) dir: String,
    pub(crate) before: Vec<Wallet>,
    #[serde(default)]
    pub(crate) after: Vec<Wallet>,
    /// Why the spender can't pay a run, and what to do; nothing when it can.
    pub(crate) problem: Option<String>,
    /// What the upkeep after the tests did.
    #[serde(default)]
    pub(crate) kept: Vec<String>,
}

const ROLES: [&str; 2] = ["spender", "merchant"];

/// The wallet directory the tests use: `cli_wallet::WALLET_DIR_VAR`'s.
fn wallet_dir() -> String {
    env::var("E2E_WALLET_DIR").unwrap_or_else(|_| "e2e/wallets".to_string())
}

/// Runs one wallet-cli command on the wallet named `role` in the tests'
/// wallet directory (wallet-cli reads E2E_WALLET_DIR as they do), with no
/// terminal, so a transfer is not asked about.
fn wallet_cli(root: &Path, role: &str, args: &[&str]) -> io::Result<String> {
    let output = Command::new("cargo")
        .args([
            "run",
            "-q",
            "--locked",
            "-p",
            "cli-wallet",
            "--bin",
            "wallet-cli",
            "--",
        ])
        .args(["--wallet-file", role])
        .args(args)
        .stdin(Stdio::null())
        .current_dir(root)
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "wallet-cli --wallet-file {role} {}: {}{}",
            args.join(" "),
            stdout.trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(stdout)
}

fn unreadable(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("wallet-cli's {what} wasn't readable"),
    )
}

fn read_wallet(root: &Path, role: &str) -> io::Result<Wallet> {
    let address = parse_address(&wallet_cli(root, role, &["address"])?)
        .ok_or_else(|| unreadable("address"))?;
    let (balance, unlocked) = parse_balance(&wallet_cli(root, role, &["balance"])?)
        .ok_or_else(|| unreadable("balance"))?;
    let tip: u64 = wallet_cli(root, role, &["bc_height"])?
        .trim()
        .parse()
        .map_err(|_| unreadable("bc_height"))?;
    let outputs = parse_outputs(&wallet_cli(root, role, &["unspent_outputs"])?);
    Ok(Wallet {
        role: role.to_string(),
        address,
        balance,
        unlocked,
        outputs: outputs.len(),
        ready: outputs
            .iter()
            .filter(|(amount, height)| *amount >= PIECE && height + LOCK_BLOCKS <= tip)
            .count(),
    })
}

/// Why `spender` can't pay one run, and what to do about it.
fn shortfall(spender: &Wallet) -> Option<String> {
    if spender.ready >= PAYMENTS && spender.unlocked >= MIN_UNLOCKED {
        return None;
    }
    let need = format!(
        "a run needs {} XMR unlocked, in {PAYMENTS} outputs of {} XMR or more",
        Xmr(MIN_UNLOCKED),
        Xmr(PIECE)
    );
    Some(
        if spender.balance < MIN_UNLOCKED + PAYMENTS as u64 * PIECE {
            format!(
                "fund the CI spender at {}: it holds {} XMR and {need}. Send it stagenet XMR \
             (the faucet: {FAUCET}); the next run's rescan finds the payment.",
                spender.address,
                Xmr(spender.balance)
            )
        } else {
            format!(
            "the spender holds {} XMR, but only {} XMR in {} outputs big enough are unlocked, and \
             {need}. Outputs unlock {LOCK_BLOCKS} blocks (about 20 minutes) after the transaction \
             that made them, so run again later; the upkeep after each run splits outputs when too \
             few are big enough.",
            Xmr(spender.balance),
            Xmr(spender.unlocked),
            spender.ready
        )
        },
    )
}

fn rescan(root: &Path, role: &str, from: Option<u64>) -> io::Result<()> {
    let range = from.map_or_else(|| format!("^{RESCAN_BLOCKS}"), |h| format!("{h}.."));
    let said = wallet_cli(root, role, &["rescan", &range])?;
    eprintln!("{role}: {}", said.trim());
    Ok(())
}

/// Finds what the wallets were paid since the last run, and stops the run
/// with what to do if the spender can't pay one run's orders. `from` rescans
/// from that height instead of the last [`RESCAN_BLOCKS`], for wallet files
/// that missed more than that (the CI pair's cache expired).
fn check_wallets(root: &Path, out: &Path, from: Option<u64>) -> io::Result<Exit> {
    for role in ROLES {
        rescan(root, role, from)?;
    }
    let before = ROLES
        .iter()
        .map(|role| read_wallet(root, role))
        .collect::<io::Result<Vec<_>>>()?;
    let problem = shortfall(&before[0]);
    for w in &before {
        eprintln!(
            "{}: {} XMR, {} unlocked, {} outputs ({} ready)",
            w.role,
            Xmr(w.balance),
            Xmr(w.unlocked),
            w.outputs,
            w.ready
        );
    }
    if let Some(problem) = &problem {
        eprintln!("{problem}");
    }
    let failed = problem.is_some();
    write_json(
        &out.join("wallets.json"),
        &Wallets {
            dir: wallet_dir(),
            before,
            problem,
            ..Wallets::default()
        },
    )?;
    Ok(Exit::passed(!failed))
}

/// After the tests: what the merchant was paid goes back to the spender,
/// and the spender's outputs are split or merged so the next run has
/// [`PAYMENTS`] outputs of at least [`PIECE`] with some to spare. Locked
/// outputs (today's payments, change and sweep) wait for the next run, a
/// day later, when they have long unlocked.
fn keep_wallets(root: &Path, out: &Path) -> io::Result<Exit> {
    let path = out.join("wallets.json");
    let mut wallets: Wallets = if path.is_file() {
        read_json(&path)?
    } else {
        Wallets {
            dir: wallet_dir(),
            ..Wallets::default()
        }
    };
    let mut ok = true;
    let mut step = |what: String, result: io::Result<String>| match result {
        Ok(_) => {
            eprintln!("{what}");
            wallets.kept.push(what);
        }
        Err(e) => {
            eprintln!("{what} failed: {e}");
            wallets.kept.push(format!("{what} failed: {e}"));
            ok = false;
        }
    };
    for role in ROLES {
        if let Err(e) = rescan(root, role, None) {
            step(format!("Rescanning the {role}"), Err(e));
        }
    }
    let spender = read_wallet(root, "spender")?;
    let merchant = read_wallet(root, "merchant")?;
    if merchant.unlocked > 0 {
        step(
            format!(
                "Swept {} XMR from the merchant to the spender",
                Xmr(merchant.unlocked)
            ),
            wallet_cli(root, "merchant", &["sweep_all", &spender.address]),
        );
    }
    let outputs = parse_outputs(&wallet_cli(root, "spender", &["unspent_outputs"])?);
    // Locked outputs count: they unlock long before the next run.
    let big = outputs.iter().filter(|(a, _)| *a >= PIECE).count();
    if big < 2 * PAYMENTS {
        step(
            format!("Split the spender's largest outputs: {big} were big enough to pay an order"),
            wallet_cli(root, "spender", &["pocketchange", "inputs=16"]),
        );
    }
    let dust = outputs.iter().filter(|(a, _)| *a < PIECE).count();
    if dust >= 2 * PAYMENTS {
        let piece = Xmr(PIECE).to_string();
        step(
            format!("Merged the spender's {dust} outputs too small to pay an order"),
            wallet_cli(root, "spender", &["sweep_below", &piece, &spender.address]),
        );
    }
    wallets.after = ROLES
        .iter()
        .map(|role| read_wallet(root, role))
        .collect::<io::Result<Vec<_>>>()?;
    write_json(&path, &wallets)?;
    Ok(Exit::passed(ok))
}

/// `live.json`: what the pages and the job summary need besides the JUnit
/// reports.
#[derive(Serialize, Deserialize, Default, Debug)]
pub(crate) struct Live {
    pub(crate) services: Vec<Check>,
    pub(crate) wallets: Option<Wallets>,
    /// Each test's `#[ignore]` reason, by `classname › name` as its JUnit
    /// report names it.
    pub(crate) reasons: BTreeMap<String, String>,
}

/// The services a reason names, by id.
pub(crate) fn needs(reason: &str) -> &'static [&'static str] {
    NEEDS
        .iter()
        .find(|(r, _)| *r == reason)
        .map_or(&[], |(_, ids)| *ids)
}

/// The first service a test with `reason` needs that wasn't there when the
/// run checked: what a failure of that test is down to.
pub(crate) fn missing<'a>(reason: &str, services: &'a [Check]) -> Option<&'a Check> {
    needs(reason).iter().find_map(|id| {
        services
            .iter()
            .find(|c| c.id == *id && c.reachable == Some(false))
    })
}

/// A JUnit case's name as `live.json` keys it.
pub(crate) fn case_key(class: &str, name: &str) -> String {
    format!("{class} › {name}")
}

/// A JUnit report's cases, or none when the suite left no report.
fn cases(path: &Path) -> io::Result<Vec<Test>> {
    if path.is_file() {
        junit(path)
    } else {
        Ok(Vec::new())
    }
}

/// Each Rust case's `#[ignore]` reason: the ignored test whose path in its
/// file ends the case's module path in its binary.
fn reasons(rust: &[Test], ignored: &[(String, Option<String>)]) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    for case in rust {
        let reason = ignored.iter().find_map(|(name, reason)| {
            let path = name.split_once(" › ").map_or(name.as_str(), |(_, p)| p);
            (case.name == path || case.name.ends_with(&format!("::{path}")))
                .then_some(reason.clone())
                .flatten()
        });
        if let Some(reason) = reason {
            found.insert(case_key(&case.class, &case.name), reason);
        }
    }
    found
}

/// The wallets' own check as a service: reachable when the spender could
/// pay one run's orders.
fn wallets_check(wallets: Option<&Wallets>) -> Check {
    let service = SERVICES
        .iter()
        .find(|s| s.id == "wallets")
        .expect("a wallets service");
    let (reachable, detail) = match wallets {
        None => (None, "not checked".to_string()),
        Some(w) => match &w.problem {
            Some(problem) => (Some(false), problem.clone()),
            None => (
                Some(true),
                format!("the spender in {} can pay a run", w.dir),
            ),
        },
    };
    Check {
        id: service.id.to_string(),
        name: service.name.to_string(),
        reachable,
        detail,
    }
}

/// Joins the run's results into `live.json` and prints the services, the
/// wallets and each failure's cause, for the job summary.
fn report(root: &Path, out: &Path) -> io::Result<Exit> {
    let rust = cases(&out.join("rust-junit.xml"))?;
    let browser = cases(&out.join("browser-junit.xml"))?;
    let services_path = out.join("services.json");
    let mut services: Vec<Check> = if services_path.is_file() {
        read_json(&services_path)?
    } else {
        Vec::new()
    };
    let wallets_path = out.join("wallets.json");
    let wallets: Option<Wallets> = if wallets_path.is_file() {
        Some(read_json(&wallets_path)?)
    } else {
        None
    };
    let check = wallets_check(wallets.as_ref());
    match services.iter_mut().find(|c| c.id == check.id) {
        Some(slot) => *slot = check,
        None => services.push(check),
    }
    let mut reasons = reasons(&rust, &ignored_tests(root)?);
    for case in &browser {
        reasons.insert(case_key(&case.class, &case.name), BROWSER_NEEDS.to_string());
    }
    let live = Live {
        services,
        wallets,
        reasons,
    };
    write_json(&out.join("live.json"), &live)?;
    print!("{}", markdown(&live, rust.iter().chain(&browser)));
    Ok(Exit::SUCCESS)
}

fn markdown<'a>(live: &Live, cases: impl Iterator<Item = &'a Test>) -> String {
    use fmt::Write as _;
    let mut out =
        String::from("## Live services\n\n| Service | State | Detail |\n| --- | --- | --- |\n");
    for c in &live.services {
        let mark = match c.reachable {
            Some(true) => "✅",
            Some(false) => "❌",
            None => "➖",
        };
        writeln!(
            out,
            "| {} | {mark} {} | {} |",
            c.name,
            state(c.reachable),
            crate::support::escape_html(&c.detail).replace('|', "\\|")
        )
        .unwrap();
    }
    if let Some(w) = &live.wallets {
        writeln!(
            out,
            "\n### Wallets ({})\n\n| Wallet | When | Balance | Unlocked | Outputs | Ready to pay |\n| --- | --- | ---: | ---: | ---: | ---: |",
            w.dir
        )
        .unwrap();
        for (when, list) in [("before", &w.before), ("after", &w.after)] {
            for wallet in list.iter() {
                writeln!(
                    out,
                    "| {} <code>{}</code> | {when} | {} | {} | {} | {} |",
                    wallet.role,
                    wallet.address,
                    Xmr(wallet.balance),
                    Xmr(wallet.unlocked),
                    wallet.outputs,
                    wallet.ready
                )
                .unwrap();
            }
        }
        if let Some(problem) = &w.problem {
            writeln!(out, "\n> [!WARNING]\n> {problem}").unwrap();
        }
        for kept in &w.kept {
            writeln!(out, "- {kept}").unwrap();
        }
    }
    let failed: Vec<&Test> = cases.filter(|c| c.status == TestStatus::Failed).collect();
    if !failed.is_empty() {
        writeln!(out, "\n### Why each test failed\n").unwrap();
        for case in failed {
            let key = case_key(&case.class, &case.name);
            let reason = live.reasons.get(&key).map_or("", String::as_str);
            let cause = match missing(reason, &live.services) {
                Some(service) => format!("{} was unreachable", service.name),
                None => "a test failure: its services answered".to_string(),
            };
            writeln!(
                out,
                "- <code>{}</code>: {cause}",
                crate::support::escape_html(&key)
            )
            .unwrap();
        }
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::root;
    use regex::Regex;

    /// The `#[ignore]`d tests the daily run leaves out, by the name
    /// `cargo xtask test-summary` lists them under (file › path in the file),
    /// and why.
    const NOT_DAILY: [(&str, &str); 9] = [
    (
        "crates/engine/tests/backup_restore.rs › backup_then_restore_preserves_tenants_and_orders_under_concurrent_writes",
        "needs the sqlite3 CLI, not the network",
    ),
    (
        "crates/engine/tests/backup_restore.rs › restore_refuses_to_overwrite_an_existing_destination_without_force",
        "needs the sqlite3 CLI, not the network",
    ),
    (
        "crates/engine/tests/verification/store/queue_scale_properties.rs › twelve_thousand_accepted_jobs_survive_sustained_saturation",
        "a scale suite: the weekly engine-scale.yml runs it",
    ),
    (
        "crates/engine/tests/verification/webhooks/scale.rs › thirteen_thousand_webhooks_recover_without_starving_healthy_merchants",
        "a scale suite: the weekly engine-scale.yml runs it",
    ),
    (
        "crates/engine/tests/verification/work/mempool/scale.rs › eight_thousand_pool_transactions_recover_and_release_cache",
        "a scale suite: the weekly engine-scale.yml runs it",
    ),
    (
        "crates/engine/tests/verification/work/mempool/scale.rs › tenant_and_transaction_rotations_cannot_phase_lock",
        "a scale suite: the weekly engine-scale.yml runs it",
    ),
    (
        "crates/engine/tests/verification/work/scale/properties.rs › thousands_of_tenants_recover_across_group_and_page_boundaries",
        "a scale suite: the weekly engine-scale.yml runs it",
    ),
    (
        "crates/engine/tests/verification/scanner/tests.rs › scale_many_stores_a_busy_mempool_and_a_block_with_payments_for_all_of_them",
        "the scanner's scale run: minutes long, run in release by hand",
    ),
    (
        "crates/engine/tests/verification/work/mempool/lock_profile.rs › compare_blocking_and_yielding_cache_mutexes",
        "a manual contention profile: its timings depend on the machine",
    ),
];

    fn case(class: &str, name: &str, status: TestStatus) -> Test {
        let scratch = crate::support::Scratch::new("live-case");
        let path = scratch.join("junit.xml");
        let body = match status {
            TestStatus::Failed => r#"<failure message="boom"/>"#,
            TestStatus::Skipped => "<skipped/>",
            TestStatus::Passed => "",
        };
        fs::write(
            &path,
            format!(r#"<testsuites><testcase classname="{class}" name="{name}">{body}</testcase></testsuites>"#),
        )
        .unwrap();
        junit(&path).unwrap().remove(0)
    }

    #[test]
    fn every_ignored_test_runs_daily_or_says_why_not() {
        let ignored = ignored_tests(&root()).unwrap();
        let mut problems = Vec::new();
        for (name, reason) in &ignored {
            let path = name.split_once(" › ").map_or(name.as_str(), |(_, p)| p);
            let daily = path.rsplit("::").next().unwrap_or(path).starts_with(PREFIX);
            let listed = NOT_DAILY.iter().any(|(n, _)| n == name);
            let reason = reason.as_deref().unwrap_or("");
            match (daily, listed) {
                (true, true) => problems.push(format!("{name} is named {PREFIX} but listed in NOT_DAILY")),
                (false, false) => problems.push(format!(
                    "{name} is #[ignore]d but neither named {PREFIX}, so the daily live run picks it up, \
                     nor listed in xtask/src/live.rs's NOT_DAILY with why it stays out"
                )),
                (true, false) if needs(reason).is_empty() => problems.push(format!(
                    "{name}'s reason {reason:?} isn't one of NEEDS', so the run can't tell what it needs"
                )),
                _ => {}
            }
        }
        for (name, _) in NOT_DAILY {
            if !ignored.iter().any(|(n, _)| n == name) {
                problems.push(format!(
                    "NOT_DAILY lists {name}, which is no longer an ignored test"
                ));
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    #[test]
    fn the_filter_picks_the_prefixed_test_and_nothing_else() {
        let filter = filter();
        let pattern = filter
            .strip_prefix("test(/")
            .and_then(|f| f.strip_suffix("/)"))
            .unwrap();
        let picks = Regex::new(pattern).unwrap();
        for name in [
            "live_tor_visitors_are_told_apart",
            "daemon_rpc::live_node_tests::live_node_get_height_returns_a_plausible_value",
        ] {
            assert!(picks.is_match(name), "{name}");
        }
        for name in [
            "daemon_rpc::live_node_tests::real_node_get_height",
            "work::scale::properties::thousands_of_tenants_recover",
            "alive_and_well",
        ] {
            assert!(!picks.is_match(name), "{name}");
        }
    }

    #[test]
    fn every_reason_names_known_services_and_every_service_is_needed() {
        let ids: Vec<&str> = SERVICES.iter().map(|s| s.id).collect();
        for (reason, needed) in NEEDS {
            assert!(!needed.is_empty(), "{reason}");
            assert!(
                needed.iter().all(|id| ids.contains(id)),
                "{reason}: {needed:?}"
            );
        }
        for id in ids {
            assert!(
                NEEDS.iter().any(|(_, n)| n.contains(&id)),
                "nothing needs {id}"
            );
        }
        assert!(!needs(BROWSER_NEEDS).is_empty());
    }

    #[test]
    fn wallet_cli_output_reads_as_amounts() {
        assert_eq!(piconero("0.060000000000"), Some(60_000_000_000));
        assert_eq!(piconero("1.5"), Some(1_500_000_000_000));
        assert_eq!(piconero("0.0000000000001"), None);
        assert_eq!(Xmr(60_000_000_123).to_string(), "0.060000000123");
        assert_eq!(
            parse_balance("Currently selected account: [0] Primary account\nTag: (No tag assigned)\nBalance: 0.252262344183, unlocked balance: 0.002262344183\n"),
            Some((252_262_344_183, 2_262_344_183))
        );
        assert_eq!(
            parse_address("0  5648a3A1PX6F  Primary account\n").as_deref(),
            Some("5648a3A1PX6F")
        );
        assert_eq!(
            parse_outputs("               Amount       Height\n       0.000873750160      2215900\n       0.006250000000      2216135\n     2216135 |\n     2216136 | *\n"),
            [(873_750_160, 2_215_900), (6_250_000_000, 2_216_135)]
        );
    }

    fn spender(balance: u64, unlocked: u64, ready: usize) -> Wallet {
        Wallet {
            role: "spender".into(),
            address: "53etP".into(),
            balance,
            unlocked,
            outputs: ready + 3,
            ready,
        }
    }

    #[test]
    fn a_spender_short_of_a_run_says_whether_to_fund_it_or_wait() {
        assert_eq!(
            shortfall(&spender(MIN_UNLOCKED * 5, MIN_UNLOCKED * 5, PAYMENTS)),
            None
        );
        let empty = shortfall(&spender(PIECE, PIECE, 1)).unwrap();
        assert!(
            empty.starts_with("fund the CI spender at 53etP: it holds 0.000500000000 XMR"),
            "{empty}"
        );
        let locked = shortfall(&spender(MIN_UNLOCKED * 5, 0, 0)).unwrap();
        assert!(locked.contains("run again later"), "{locked}");
        // Unlocked enough, but in too few outputs to pay each order.
        let lumpy = shortfall(&spender(MIN_UNLOCKED * 5, MIN_UNLOCKED * 5, 2)).unwrap();
        assert!(lumpy.contains("in 2 outputs big enough"), "{lumpy}");
    }

    #[test]
    fn a_failure_is_put_down_to_a_service_only_when_one_it_needs_was_down() {
        let services = vec![
            Check {
                id: "stagenet-node".into(),
                name: "Monero stagenet nodes".into(),
                reachable: Some(true),
                detail: String::new(),
            },
            wallets_check(Some(&Wallets {
                dir: "e2e/wallets/ci".into(),
                problem: Some("fund the CI spender at 53etP".into()),
                ..Wallets::default()
            })),
            Check {
                id: "tor".into(),
                name: "Tor network".into(),
                reachable: None,
                detail: String::new(),
            },
        ];
        let paid = missing(BROWSER_NEEDS, &services).unwrap();
        assert_eq!(paid.id, "wallets");
        assert!(missing("needs the live stagenet node", &services).is_none());
        assert!(missing("needs tor and the live Tor network", &services).is_none());
        assert!(missing("no reason we know", &services).is_none());
    }

    #[test]
    fn each_case_gets_its_ignore_reason_by_its_path() {
        let ignored = vec![
            (
                "crates/engine/src/daemon_rpc.rs › live_node_tests::live_node_get_height"
                    .to_string(),
                Some("needs a live mainnet node".to_string()),
            ),
            (
                "crates/monokulo/tests/e2e_tor.rs › live_tor_visitors".to_string(),
                Some("needs tor and the live Tor network".to_string()),
            ),
        ];
        let case = |class: &str, name: &str| case(class, name, TestStatus::Passed);
        let found = reasons(
            &[
                case(
                    "engine",
                    "daemon_rpc::live_node_tests::live_node_get_height",
                ),
                case("monokulo::e2e_tor", "live_tor_visitors"),
                case("engine", "something_else"),
            ],
            &ignored,
        );
        assert_eq!(
            found.into_iter().collect::<Vec<_>>(),
            [
                (
                    "engine › daemon_rpc::live_node_tests::live_node_get_height".to_string(),
                    "needs a live mainnet node".to_string()
                ),
                (
                    "monokulo::e2e_tor › live_tor_visitors".to_string(),
                    "needs tor and the live Tor network".to_string()
                ),
            ]
        );
    }

    #[test]
    fn the_summary_says_why_each_test_failed() {
        let live = Live {
            services: vec![wallets_check(Some(&Wallets {
                dir: "e2e/wallets/ci".into(),
                before: vec![spender(PIECE, PIECE, 1)],
                problem: Some("fund the CI spender at 53etP".into()),
                ..Wallets::default()
            }))],
            wallets: Some(Wallets {
                dir: "e2e/wallets/ci".into(),
                before: vec![spender(PIECE, PIECE, 1)],
                problem: Some("fund the CI spender at 53etP".into()),
                ..Wallets::default()
            }),
            reasons: BTreeMap::from([(
                "pos.spec.js › pays".to_string(),
                BROWSER_NEEDS.to_string(),
            )]),
        };
        let failed = |class: &str, name: &str| case(class, name, TestStatus::Failed);
        let cases = [failed("pos.spec.js", "pays"), failed("shared", "live_dns")];
        let text = markdown(&live, cases.iter());
        assert!(
            text.contains(
                "| Funded stagenet test wallets | ❌ unreachable | fund the CI spender at 53etP |"
            ),
            "{text}"
        );
        assert!(
            text.contains("| spender <code>53etP</code> | before | 0.000500000000 |"),
            "{text}"
        );
        assert!(
            text.contains(
                "- <code>pos.spec.js › pays</code>: Funded stagenet test wallets was unreachable"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "- <code>shared › live_dns</code>: a test failure: its services answered"
            ),
            "{text}"
        );
    }
}
