//! `monokulo-bell` as a process: its options file, its help, its
//! version, `--check`, what stops it before it listens, and its status.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BINARY: &str = env!("CARGO_BIN_EXE_monokulo-bell");

const SECRETS: [(&str, &str); 2] = [
    (
        "MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY",
        "Ibxx_5AKakO-3qeNVR61Dw",
    ),
    (
        "MONOKULO_BELL_VAPID_KEY",
        "4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d",
    ),
];

const CLOVER_CODE: (&str, &str) = (
    "MONOKULO_BELL_CLOVER_NORTH_AMERICA_AUTH_CODE",
    "9f2d4a8c-6b1e-4f3a-9c7d-2e5b8a1f4c6d",
);

/// A directory of the test's own.
fn dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "monokulo-bell-binary-{tag}-{}-{}",
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
        "#   MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY\n",
        "#   MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY\n",
        "#   MONOKULO_BELL_CLOVER_NORTH_AMERICA_AUTH_CODE\n",
        "#   MONOKULO_BELL_CLOVER_SANDBOX_AUTH_CODE\n",
        "#   MONOKULO_BELL_VAPID_KEY (required)\n",
        "[bell]\n",
        "# listen = \"127.0.0.1:8445\"\n",
        "# registration_failure_limit = 20\n",
        "# square.production.application_id = \"sq0idp-Monokular0Bell0Example\"\n",
        "# square.sandbox.api_url = \"http://127.0.0.1:8738\"\n",
        "# clover.latin_america.application_id = \"MONOKULARBELL\"\n",
        "[logging]\n",
        "# format = \"auto\"\n",
    ] {
        assert!(written.contains(line), "{line:?} in {written}");
    }
    let again = run(&["--options", options, "--init"], &[]);
    assert!(!again.status.success());
    assert!(text(&again).contains("already exists"), "{}", text(&again));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_help_names_every_secret() {
    let output = run(&["--help"], &[]);
    assert!(output.status.success());
    let help = text(&output);
    for secret in [
        "MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY",
        "MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY",
        "MONOKULO_BELL_CLOVER_NORTH_AMERICA_AUTH_CODE",
        "MONOKULO_BELL_CLOVER_EUROPE_AUTH_CODE",
        "MONOKULO_BELL_CLOVER_LATIN_AMERICA_AUTH_CODE",
        "MONOKULO_BELL_CLOVER_SANDBOX_AUTH_CODE",
        "MONOKULO_BELL_VAPID_KEY",
    ] {
        assert!(help.contains(secret), "{secret}: {help}");
    }
}

#[test]
fn the_version_names_the_binary() {
    let output = run(&["--version"], &[]);
    assert!(output.status.success());
    let version = String::from_utf8_lossy(&output.stdout);
    assert!(
        version.starts_with(&format!("monokulo-bell {}", env!("CARGO_PKG_VERSION"))),
        "{version}"
    );
}

/// `--check` says what the Bell would run on, then OK, a secret still
/// under its old name said first, and opens, makes and binds nothing: not
/// the database, not its directory, not the ports (taken here, which a
/// start would fail on).
#[test]
fn check_says_what_the_bell_would_run_on_and_touches_nothing() {
    let dir = dir("check");
    let store = dir.join("data").join("bell.db");
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let path = dir.join("bell.toml");
    std::fs::write(
        &path,
        format!(
            "[bell]\nlisten = \"127.0.0.1:{port}\"\npublic_uri = \"https://bell.example.org\"\nstore_path = '{}'\n\n[bell.square.production]\napplication_id = \"sq0idp-Test\"\n\n[bell.clover.europe]\napplication_id = \"MONOKULARBELL\"\n\n[status]\nlisten = \"127.0.0.1:{port}\"\n",
            store.display()
        ),
    )
    .unwrap();
    let vars = [
        SECRETS[0],
        ("MONOKULAR_BELL_VAPID_KEY", SECRETS[1].1),
        (
            "MONOKULO_BELL_CLOVER_EUROPE_AUTH_CODE",
            "e0000000-0000-4000-8000-00000000e0e0",
        ),
    ];
    let output = run(&["--options", path.to_str().unwrap(), "--check"], &vars);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let said = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = said.lines().collect();
    assert_eq!(lines.len(), 10, "{said}");
    assert!(
        lines[0]
            .starts_with("warning: MONOKULAR_BELL_VAPID_KEY was renamed MONOKULO_BELL_VAPID_KEY"),
        "{said}"
    );
    assert_eq!(
        lines[1..6],
        [
            format!("listens on 127.0.0.1:{port}").as_str(),
            "public address https://bell.example.org",
            "Square production: on, application sq0idp-Test, webhooks to https://bell.example.org/v1/square/production/webhook",
            "Clover europe: on, application MONOKULARBELL, webhooks to https://bell.example.org/v1/clover/europe/webhook",
            "off: Square sandbox, and Clover north_america, latin_america and sandbox",
        ]
    );
    assert!(lines[6].starts_with("VAPID public key B"), "{said}");
    assert_eq!(lines[7], format!("registrations in {}", store.display()));
    assert_eq!(lines[8], format!("status on 127.0.0.1:{port}"));
    assert_eq!(lines[9], "OK");
    // No secret is said.
    for (_, value) in vars {
        assert!(!said.contains(value), "{said}");
    }
    assert!(output.stderr.is_empty(), "{}", text(&output));
    assert!(!store.parent().unwrap().exists(), "nothing was made");
    drop(taken);
    let _ = std::fs::remove_dir_all(dir);
}

/// Every one of these stops the Bell at start, saying why, and `--check`
/// says the same, with the same status.
#[test]
fn missing_or_invalid_settings_stop_the_bell_before_it_listens() {
    let dir = dir("invalid");
    let store = dir.join("bell.db");
    let ready = format!(
        "[bell]\nlisten = \"127.0.0.1:0\"\npublic_uri = \"https://bell.example.org\"\nstore_path = '{}'\n\n[bell.square.production]\napplication_id = \"sq0idp-Test\"\n",
        store.display()
    );
    // The options file, the environment, and what the Bell says.
    type Case<'a> = (String, &'a [(&'a str, &'a str)], &'a str);
    let clover = ready.replace(
        "[bell.square.production]\napplication_id = \"sq0idp-Test\"\n",
        "[bell.clover.north_america]\napplication_id = \"MONOKULARBELL\"\n",
    );
    let with_clover_code = [SECRETS[0], SECRETS[1], CLOVER_CODE];
    let clover_only = [SECRETS[1], CLOVER_CODE];
    let both_names = [
        SECRETS[0],
        SECRETS[1],
        ("MONOKULAR_BELL_VAPID_KEY", &"02".repeat(32)),
    ];
    let old_square = ready.replace("[bell.square.production]", "[bell.square]");
    let old_key = [
        ("MONOKULO_BELL_SQUARE_WEBHOOK_KEY", SECRETS[0].1),
        SECRETS[1],
    ];
    let cases: [Case; 17] = [
        (
            format!(
                "{ready}\n[bell.square.sandbox]\napi_url = \"https://connect.squareupsandbox.com\"\n"
            ),
            &SECRETS,
            "bell.square.sandbox.api_url: is set but bell.square.sandbox.application_id isn't",
        ),
        (
            ready.clone(),
            &SECRETS[1..],
            "MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY must be set",
        ),
        (
            clover.clone(),
            &SECRETS[1..],
            "MONOKULO_BELL_CLOVER_NORTH_AMERICA_AUTH_CODE must be set",
        ),
        (
            ready.clone(),
            &with_clover_code,
            "MONOKULO_BELL_CLOVER_NORTH_AMERICA_AUTH_CODE is set but bell.clover.north_america.application_id isn't",
        ),
        (
            clover.replace("north_america", "mars"),
            &clover_only,
            "there is no setting called bell.clover.mars.application_id",
        ),
        (
            clover.replace(
                "[bell.clover.north_america]",
                "[bell.clover.north_america]\napi_url = \"http://example.org\"",
            ),
            &clover_only,
            "bell.clover.north_america.api_url",
        ),
        (
            clover.replace("application_id = \"MONOKULARBELL\"\n", ""),
            &SECRETS[1..],
            "an integration's application_id must be set, such as bell.square.production.application_id or bell.clover.north_america.application_id",
        ),
        (
            ready.clone(),
            &SECRETS[..1],
            "MONOKULO_BELL_VAPID_KEY must be set",
        ),
        (
            ready.replace("public_uri = \"https://bell.example.org\"\n", ""),
            &SECRETS,
            "bell.public_uri must be set",
        ),
        (
            ready.replace("application_id = \"sq0idp-Test\"\n", ""),
            &SECRETS,
            "MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY is set but bell.square.production.application_id isn't",
        ),
        (
            ready.replace("sq0idp-Test", "sandbox-sq0idb-Test"),
            &SECRETS,
            "is a sandbox application's: it goes in [bell.square.sandbox]",
        ),
        // An options file, a secret and an option from before the
        // integrations, each refused with what replaced it.
        (
            old_square.clone(),
            &SECRETS,
            "bell.square.application_id is now called bell.square.production.application_id or bell.square.sandbox.application_id",
        ),
        (
            old_square.replace(
                "application_id",
                "environment = \"sandbox\"\napplication_id",
            ),
            &SECRETS,
            "bell.square.environment is now called the name of the application's table",
        ),
        (
            ready.clone(),
            &old_key,
            "MONOKULO_BELL_SQUARE_WEBHOOK_KEY is now called MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY or MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY",
        ),
        (
            format!("{ready}\n[bell.unknown]\nkey = 1\n"),
            &SECRETS,
            "bell.unknown.key",
        ),
        (
            ready.clone(),
            &both_names,
            "MONOKULAR_BELL_VAPID_KEY and MONOKULO_BELL_VAPID_KEY are both set, to different values",
        ),
        (
            format!("{ready}\n[status]\nlisten = \"0.0.0.0:8447\"\n"),
            &SECRETS,
            "isn't a loopback address",
        ),
    ];
    for (file, vars, expected) in cases {
        let path = dir.join("bell.toml");
        std::fs::write(&path, &file).unwrap();
        for check in [false, true] {
            let options = path.to_str().unwrap();
            let args: &[&str] = if check {
                &["--options", options, "--check"]
            } else {
                &["--options", options]
            };
            let output = run(args, vars);
            assert_eq!(output.status.code(), Some(1), "{file}: {}", text(&output));
            assert!(
                text(&output).contains(expected),
                "{expected}: {}",
                text(&output)
            );
            if check {
                assert!(output.stdout.is_empty(), "{}", text(&output));
            }
        }
    }
    assert!(!store.exists(), "nothing was opened");
    let _ = std::fs::remove_dir_all(dir);
}

/// Started with what it needs, for Square or for two Clover environments, it opens
/// its database, listens, answers its status on loopback, and stops
/// cleanly on SIGTERM.
#[cfg(unix)]
#[test]
fn the_bell_listens_serves_its_status_and_stops_on_sigterm_over_http() {
    listens_and_stops(
        "listen-square",
        "[bell.square.production]\napplication_id = \"sq0idp-Test\"\n",
        &SECRETS,
        &["serving Square production"],
    );
    // A service file written for monokular-bell: the VAPID key's old name
    // is read for a release, and said at start.
    let old = [SECRETS[0], ("MONOKULAR_BELL_VAPID_KEY", SECRETS[1].1)];
    listens_and_stops(
        "listen-old-names",
        "[bell.square.production]\napplication_id = \"sq0idp-Test\"\n",
        &old,
        &[
            "MONOKULAR_BELL_VAPID_KEY was renamed MONOKULO_BELL_VAPID_KEY",
            "serving Square production",
        ],
    );
    listens_and_stops(
        "listen-clover",
        "[bell.clover.north_america]\napplication_id = \"MONOKULARBELL\"\n\n[bell.clover.sandbox]\napplication_id = \"MONOKULARBELL\"\n\n[logging]\nformat = \"json\"\n",
        &[
            SECRETS[1],
            CLOVER_CODE,
            (
                "MONOKULO_BELL_CLOVER_SANDBOX_AUTH_CODE",
                "5a1e0c2b-7d3f-4e8a-b6c9-1f0e2d3c4b5a",
            ),
        ],
        &["serving Clover north_america", "serving Clover sandbox"],
    );
}

#[cfg(unix)]
/// The Bell started with `section` and `vars` says each of `lines` in
/// order, then that it is listening, and stops cleanly on SIGTERM.
fn listens_and_stops(tag: &str, section: &str, vars: &[(&str, &str)], lines: &[&str]) {
    use std::io::{BufRead as _, BufReader};

    let dir = dir(tag);
    let path = dir.join("bell.toml");
    let store = dir.join("data").join("bell.db");
    std::fs::write(
        &path,
        format!(
            "[status]\nlisten = \"127.0.0.1:0\"\n\n[bell]\nlisten = \"127.0.0.1:0\"\npublic_uri = \"https://bell.example.org\"\nstore_path = '{}'\n\n{section}",
            store.display()
        ),
    )
    .unwrap();
    let mut child = Command::new(BINARY)
        .args(["--options", path.to_str().unwrap()])
        .env_clear()
        .envs(vars.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Log lines go to stderr; read on a thread, so a Bell that never says
    // it is listening fails the test instead of hanging it.
    let (sent, said_lines) = std::sync::mpsc::channel();
    let stderr = BufReader::new(child.stderr.take().unwrap());
    std::thread::spawn(move || {
        for line in stderr.lines().map_while(Result::ok) {
            let _ = sent.send(line);
        }
    });
    let wait = std::time::Duration::from_secs(30);
    let said = |what: &str| loop {
        match said_lines.recv_timeout(wait) {
            Ok(line) if line.contains(what) => return Some(line),
            Ok(_) => {}
            Err(_) => return None,
        }
    };
    let listening = lines
        .iter()
        .try_for_each(|line| said(line).map(|_| ()))
        .and_then(|()| said("the Bell is listening"));
    if listening.is_none() {
        let _ = child.kill();
    }
    let line = listening.expect("the Bell never said it was listening");
    // `logging.format`: JSON lines when asked, readable text otherwise
    // (`auto` away from a terminal is JSON, so the default isn't asked).
    if section.contains("format = \"json\"") {
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(parsed.is_object(), "{line}");
    }
    assert!(store.exists(), "its database is made at bell.store_path");
    let status = said("the Bell's status is listening").expect("the status never listened");
    let status = status_of(&status);
    assert!(status.contains("\"service\":\"monokulo-bell\""), "{status}");
    assert!(
        status.contains("\"registrations\":{\"stored\":0}"),
        "{status}"
    );
    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    assert!(said("shut down cleanly").is_some());
    let status = child.wait().unwrap();
    assert!(status.success(), "{status}");
    let _ = std::fs::remove_dir_all(dir);
}

/// The body of `GET /status` from the address the log `line` names, over
/// a plain socket, as the router's status page asks it.
#[cfg(unix)]
fn status_of(line: &str) -> String {
    use std::io::{Read as _, Write as _};

    let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
    let address = parsed["attributes"]["status.address"]
        .as_str()
        .unwrap_or_else(|| panic!("no status.address in {line}"))
        .to_owned();
    let mut stream = std::net::TcpStream::connect(&address).unwrap();
    write!(
        stream,
        "GET /status HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    answer
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default()
}
