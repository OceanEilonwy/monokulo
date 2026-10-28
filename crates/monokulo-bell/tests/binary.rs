//! `monokular-bell` as a process: its options file, its help, and what
//! stops it before it listens.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BINARY: &str = env!("CARGO_BIN_EXE_monokular-bell");

const SECRETS: [(&str, &str); 2] = [
    (
        "MONOKULAR_BELL_SQUARE_WEBHOOK_KEY",
        "Ibxx_5AKakO-3qeNVR61Dw",
    ),
    (
        "MONOKULAR_BELL_VAPID_KEY",
        "4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d",
    ),
];

/// A directory of the test's own.
fn dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "monokular-bell-binary-{tag}-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The Bell run with `args` and nothing in its environment but `vars`.
fn run(args: &[&str], vars: &[(&str, &str)]) -> Output {
    Command::new(BINARY)
        .args(args)
        .env_clear()
        .envs(vars.iter().copied())
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn init_writes_the_options_file_and_never_over_one() {
    let dir = dir("init");
    let path = dir.join("bell.toml");
    let options = path.to_str().unwrap();
    let output = run(&["--options", options, "--init"], &[]);
    assert!(output.status.success(), "{}", text(&output));
    let written = std::fs::read_to_string(&path).unwrap();
    for line in [
        "#   MONOKULAR_BELL_SQUARE_WEBHOOK_KEY (required)\n",
        "#   MONOKULAR_BELL_VAPID_KEY (required)\n",
        "[bell]\n",
        "# listen = \"127.0.0.1:8445\"\n",
        "# registration_failure_limit = 20\n",
        "# square.environment = \"production\"\n",
        "[logging]\n",
    ] {
        assert!(written.contains(line), "{line:?} in {written}");
    }
    let again = run(&["--options", options, "--init"], &[]);
    assert!(!again.status.success());
    assert!(text(&again).contains("already exists"), "{}", text(&again));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_help_names_both_secrets() {
    let output = run(&["--help"], &[]);
    assert!(output.status.success());
    let help = text(&output);
    for secret in [
        "MONOKULAR_BELL_SQUARE_WEBHOOK_KEY",
        "MONOKULAR_BELL_VAPID_KEY",
    ] {
        assert!(help.contains(secret), "{secret}: {help}");
    }
}

/// Every one of these stops the Bell at start, saying why.
#[test]
fn missing_or_invalid_settings_stop_the_bell_before_it_listens() {
    let dir = dir("invalid");
    let store = dir.join("bell.db");
    let ready = format!(
        "[bell]\nlisten = \"127.0.0.1:0\"\npublic_uri = \"https://bell.example.org\"\nstore_path = \"{}\"\n\n[bell.square]\napplication_id = \"sq0idp-Test\"\n",
        store.display()
    );
    // The options file, the environment, and what the Bell says.
    type Case<'a> = (String, &'a [(&'a str, &'a str)], &'a str);
    let cases: [Case; 6] = [
        (
            ready.clone(),
            &SECRETS[1..],
            "MONOKULAR_BELL_SQUARE_WEBHOOK_KEY must be set",
        ),
        (
            ready.clone(),
            &SECRETS[..1],
            "MONOKULAR_BELL_VAPID_KEY must be set",
        ),
        (
            ready.replace("public_uri = \"https://bell.example.org\"\n", ""),
            &SECRETS,
            "bell.public_uri must be set",
        ),
        (
            ready.replace("application_id = \"sq0idp-Test\"\n", ""),
            &SECRETS,
            "bell.square.application_id must be set",
        ),
        (
            ready.replace("sq0idp-Test", "sandbox-sq0idb-Test"),
            &SECRETS,
            "bell.square.environment is production",
        ),
        (
            format!("{ready}\n[bell.unknown]\nkey = 1\n"),
            &SECRETS,
            "bell.unknown.key",
        ),
    ];
    for (file, vars, expected) in cases {
        let path = dir.join("bell.toml");
        std::fs::write(&path, &file).unwrap();
        let output = run(&["--options", path.to_str().unwrap()], vars);
        assert_eq!(output.status.code(), Some(1), "{file}: {}", text(&output));
        assert!(
            text(&output).contains(expected),
            "{expected}: {}",
            text(&output)
        );
    }
    assert!(!store.exists(), "nothing was opened");
    let _ = std::fs::remove_dir_all(dir);
}

/// Started with what it needs, it opens its database, listens, and stops
/// cleanly on SIGTERM.
#[cfg(unix)]
#[test]
fn the_bell_listens_and_stops_on_sigterm() {
    use std::io::{BufRead as _, BufReader};

    let dir = dir("listen");
    let path = dir.join("bell.toml");
    let store = dir.join("data").join("bell.db");
    std::fs::write(
        &path,
        format!(
            "[bell]\nlisten = \"127.0.0.1:0\"\npublic_uri = \"https://bell.example.org\"\nstore_path = \"{}\"\n\n[bell.square]\napplication_id = \"sq0idp-Test\"\n",
            store.display()
        ),
    )
    .unwrap();
    let mut child = Command::new(BINARY)
        .args(["--options", path.to_str().unwrap()])
        .env_clear()
        .envs(SECRETS)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Log lines go to stderr; read on a thread, so a Bell that never says
    // it is listening fails the test instead of hanging it.
    let (sent, lines) = std::sync::mpsc::channel();
    let stderr = BufReader::new(child.stderr.take().unwrap());
    std::thread::spawn(move || {
        for line in stderr.lines().map_while(Result::ok) {
            let _ = sent.send(line);
        }
    });
    let wait = std::time::Duration::from_secs(30);
    let said = |what: &str| loop {
        match lines.recv_timeout(wait) {
            Ok(line) if line.contains(what) => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    };
    let listening = said("the Bell is listening");
    if !listening {
        let _ = child.kill();
    }
    assert!(listening, "the Bell never said it was listening");
    assert!(store.exists(), "its database is made at bell.store_path");
    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    assert!(said("shut down cleanly"));
    let status = child.wait().unwrap();
    assert!(status.success(), "{status}");
    let _ = std::fs::remove_dir_all(dir);
}
