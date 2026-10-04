//! Drives the real `stagenet-wallet-cli` binary through the commands that
//! need no node: creating a wallet, then an interactive session run over
//! stdin, as a person would type it. Commands that talk to a node are
//! covered by hand against stagenet (see the crate README).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn temp_dir(test: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("stagenet-wallet-cli-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cli(dir: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_stagenet-wallet-cli"))
        .args(["--wallet-dir", dir.to_str().unwrap()])
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_new_wallet_can_be_opened_and_driven_interactively() {
    let dir = temp_dir("interactive");
    let created = cli(&dir, &["--generate-new-wallet", "alice"], "exit\n");
    assert!(created.status.success(), "{}", stderr(&created));
    let text = stdout(&created);
    let address = text
        .lines()
        .find_map(|l| l.strip_prefix("Generated new wallet: "))
        .unwrap()
        .to_string();
    assert!(address.starts_with('5'), "a stagenet address: {address}");
    assert!(
        dir.join("alice.json").exists(),
        "the whole wallet is one file"
    );
    assert!(
        text.contains(&format!("[wallet {}]: ", &address[..6])),
        "creating a wallet opens it: {text}"
    );

    let session = cli(
        &dir,
        &["--wallet-file", "alice"],
        "address\n\
         address new shop till\n\
         address all\n\
         set_description \"test wallet\"\n\
         get_description\n\
         address_book add 5AAAA not-an-address\n\
         set unit millinero\n\
         set\n\
         wallet_info\n\
         help transfer\n\
         no_such_command\n\
         seed\n\
         exit\n",
    );
    assert!(session.status.success(), "{}", stderr(&session));
    let out = stdout(&session);
    let err = stderr(&session);
    assert!(
        out.contains(&format!("0  {address}  Primary account")),
        "{out}"
    );
    assert!(
        out.contains("1  ") && out.contains("  shop till"),
        "address new prints the new subaddress: {out}"
    );
    assert!(out.contains("]: test wallet\n"), "{out}");
    assert!(
        err.contains("failed to parse address 5AAAA"),
        "a failing command reports and the session carries on: {err}"
    );
    assert!(out.contains("unit = millinero"), "{out}");
    assert!(
        out.contains("Description: test wallet") && out.contains("Network type: Stagenet"),
        "{out}"
    );
    assert!(
        out.contains("transfer [index=<N1>"),
        "help <command> shows the reference usage: {out}"
    );
    assert!(err.contains("no_such_command"), "{err}");
    let seed_line = out
        .lines()
        .rev()
        .find(|l| l.split_whitespace().count() == 25)
        .expect("the 25-word seed");

    // Everything set above persisted in the one file.
    let file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("alice.json")).unwrap()).unwrap();
    assert_eq!(file["description"], "test wallet");
    assert_eq!(file["settings"]["unit"], "millinero");
    assert_eq!(file["accounts"][0]["subaddress_labels"][1], "shop till");

    // The seed restores the same wallet.
    let restored = cli(
        &dir,
        &[
            "--generate-new-wallet",
            "alice-again",
            "--restore-deterministic-wallet",
            "--electrum-seed",
            seed_line,
            "wallet_info",
        ],
        "",
    );
    assert!(restored.status.success(), "{}", stderr(&restored));
    assert!(stdout(&restored).contains(&format!("Address: {address}")));
}

#[test]
fn one_command_runs_and_exits_and_existing_wallets_are_never_replaced() {
    let dir = temp_dir("one-shot");
    assert!(cli(&dir, &["--generate-new-wallet", "bob", "version"], "")
        .status
        .success());
    let again = cli(&dir, &["--generate-new-wallet", "bob", "version"], "");
    assert!(!again.status.success());
    assert!(
        stderr(&again).contains("already exists"),
        "{}",
        stderr(&again)
    );

    let integrated = cli(
        &dir,
        &[
            "--wallet-file",
            "bob",
            "integrated_address",
            "0123456789abcdef",
        ],
        "",
    );
    let integrated_address = stdout(&integrated)
        .trim()
        .strip_prefix("Matching integrated address: ")
        .unwrap()
        .to_string();
    let decoded = cli(
        &dir,
        &[
            "--wallet-file",
            "bob",
            "integrated_address",
            &integrated_address,
        ],
        "",
    );
    assert!(
        stdout(&decoded).contains("payment ID: 0123456789abcdef"),
        "{}",
        stdout(&decoded)
    );

    let keys = stdout(&cli(&dir, &["--wallet-file", "bob", "spendkey"], ""));
    let secret = keys
        .lines()
        .find_map(|l| l.strip_prefix("secret: "))
        .unwrap()
        .to_string();
    let from_key = cli(
        &dir,
        &["--generate-from-spend-key", "bob-from-key", "wallet_info"],
        &format!("{secret}\n"),
    );
    assert!(from_key.status.success(), "{}", stderr(&from_key));
    let bob_info = stdout(&cli(&dir, &["--wallet-file", "bob", "wallet_info"], ""));
    let bob_address = bob_info
        .lines()
        .find(|l| l.starts_with("Address: "))
        .unwrap();
    assert!(
        stdout(&from_key).contains(bob_address),
        "a spend key restores its wallet"
    );

    let testnet = cli(&dir, &["--testnet", "--wallet-file", "bob", "version"], "");
    assert!(!testnet.status.success());
}

#[test]
fn transfer_arguments_are_checked_before_any_node_is_contacted() {
    let dir = temp_dir("transfer-args");
    assert!(
        cli(&dir, &["--generate-new-wallet", "carol", "version"], "")
            .status
            .success()
    );
    // An unreachable node: these must fail on their arguments, never get
    // as far as connecting.
    let node = ["--daemon-address", "127.0.0.1:9"];
    for (args, expected) in [
        (vec!["transfer", "5AAAA"], "wrong number of arguments"),
        (vec!["transfer", "11", "5AAAA", "1"], "ring size 11"),
        (
            vec!["transfer", "5AAAA", "1", "0123456789abcdef"],
            "payment IDs are obsolete",
        ),
        (vec!["sweep_all", "outputs=0", "5AAAA"], "greater than 0"),
        (vec!["mark_output_spent", "12345"], "<amount>/<offset>"),
        (vec!["set", "default-ring-size", "11"], "must be 16"),
    ] {
        let output = cli(
            &dir,
            &[&node[..], &["--wallet-file", "carol"], &args[..]].concat(),
            "",
        );
        assert!(!output.status.success(), "{args:?}");
        assert!(
            stderr(&output).contains(expected),
            "{args:?}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn rescan_takes_a_block_range_and_leaves_the_file_alone_without_a_node() {
    let dir = temp_dir("rescan");
    assert!(cli(&dir, &["--generate-new-wallet", "dave", "version"], "")
        .status
        .success());
    let before = std::fs::read_to_string(dir.join("dave.json")).unwrap();
    // An unreachable node: the bad arguments fail before connecting, and
    // the good one fails to connect.
    let node = ["--daemon-address", "127.0.0.1:9"];
    for (args, expected) in [
        (vec!["rescan"], "<BLOCKS>"),
        (vec!["rescan", "200"], "a bare number is ambiguous"),
        (vec!["rescan", "lots"], "expected ^<blocks back>"),
        (vec!["rescan", "5..x"], "expected ^<blocks back>"),
        (
            vec!["rescan", "http://node:38089", "^10"],
            "unexpected argument",
        ),
        (
            vec!["rescan", "^200..^100"],
            "cannot reach the stagenet node at http://127.0.0.1:9",
        ),
    ] {
        let output = cli(
            &dir,
            &[&node[..], &["--wallet-file", "dave"], &args[..]].concat(),
            "",
        );
        assert!(!output.status.success(), "{args:?}");
        assert!(
            stderr(&output).contains(expected),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("dave.json")).unwrap(),
        before,
        "a rescan that never reached a node changes nothing"
    );
}
