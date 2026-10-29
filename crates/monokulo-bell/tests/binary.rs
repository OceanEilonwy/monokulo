//! `monokulo-bell` as a process: its options file, its help, its
//! version, `--check`, what stops it before it listens, and its status.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BINARY: &str = env!("CARGO_BIN_EXE_monokulo-bell");

const SECRETS: [(&str, &str); 2] = [
    ("MONOKULO_BELL_SQUARE_WEBHOOK_KEY", "Ibxx_5AKakO-3qeNVR61Dw"),
    (
        "MONOKULO_BELL_VAPID_KEY",
        "4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d4b1d",
    ),
];

const CLOVER_CODE: (&str, &str) = (
    "MONOKULO_BELL_CLOVER_AUTH_CODE",
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
        "#   MONOKULO_BELL_SQUARE_WEBHOOK_KEY\n",
        "#   MONOKULO_BELL_CLOVER_AUTH_CODE\n",
        "#   MONOKULO_BELL_VAPID_KEY (required)\n",
        "[bell]\n",
        "# listen = \"127.0.0.1:8445\"\n",
        "# registration_failure_limit = 20\n",
        "# square.environment = \"production\"\n",
        "# clover.environment = \"north_america\"\n",
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
fn the_help_names_the_three_secrets() {
    let output = run(&["--help"], &[]);
    assert!(output.status.success());
    let help = text(&output);
    for secret in [
        "MONOKULO_BELL_SQUARE_WEBHOOK_KEY",
        "MONOKULO_BELL_CLOVER_AUTH_CODE",
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
            "[bell]\nlisten = \"127.0.0.1:{port}\"\npublic_uri = \"https://bell.example.org\"\nstore_path = '{}'\n\n[bell.square]\napplication_id = \"sq0idp-Test\"\n\n[status]\nlisten = \"127.0.0.1:{port}\"\n",
            store.display()
        ),
    )
    .unwrap();
    let vars = [SECRETS[0], ("MONOKULAR_BELL_VAPID_KEY", SECRETS[1].1)];
    let output = run(&["--options", path.to_str().unwrap(), "--check"], &vars);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let said = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = said.lines().collect();
    assert_eq!(lines.len(), 9, "{said}");
    assert!(
        lines[0]
            .starts_with("warning: MONOKULAR_BELL_VAPID_KEY was renamed MONOKULO_BELL_VAPID_KEY"),
        "{said}"
    );
    assert_eq!(
        lines[1..4],
        [
            format!("listens on 127.0.0.1:{port}").as_str(),
            "public address https://bell.example.org",
            "Square: on, application sq0idp-Test (production), webhooks to https://bell.example.org/v1/square/webhook",
        ]
    );
    assert_eq!(lines[4], "Clover: off");
    assert!(lines[5].starts_with("VAPID public key B"), "{said}");
    assert_eq!(lines[6], format!("registrations in {}", store.display()));
    assert_eq!(lines[7], format!("status on 127.0.0.1:{port}"));
    assert_eq!(lines[8], "OK");
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
        "[bell]\nlisten = \"127.0.0.1:0\"\npublic_uri = \"https://bell.example.org\"\nstore_path = '{}'\n\n[bell.square]\napplication_id = \"sq0idp-Test\"\n",
        store.display()
    );
    // The options file, the environment, and what the Bell says.
    type Case<'a> = (String, &'a [(&'a str, &'a str)], &'a str);
    let clover = ready.replace(
        "[bell.square]\napplication_id = \"sq0idp-Test\"\n",
        "[bell.clover]\napplication_id = \"MONOKULARBELL\"\n",
    );
    let with_clover_code = [SECRETS[0], SECRETS[1], CLOVER_CODE];
    let clover_only = [SECRETS[1], CLOVER_CODE];
    let both_names = [
        SECRETS[0],
        SECRETS[1],
        ("MONOKULAR_BELL_VAPID_KEY", &"02".repeat(32)),
    ];
    let cases: [Case; 13] = [
        (
            ready.clone(),
            &SECRETS[1..],
            "MONOKULO_BELL_SQUARE_WEBHOOK_KEY must be set",
        ),
        (
            clover.clone(),
            &SECRETS[1..],
            "MONOKULO_BELL_CLOVER_AUTH_CODE must be set",
        ),
        (
            ready.clone(),
            &with_clover_code,
            "MONOKULO_BELL_CLOVER_AUTH_CODE is set but bell.clover.application_id isn't",
        ),
        (
            clover.replace("[bell.clover]", "[bell.clover]\nenvironment = \"mars\""),
            &clover_only,
            "sandbox, north_america, europe, latin_america",
        ),
        (
            clover.replace(
                "[bell.clover]",
                "[bell.clover]\napi_url = \"http://example.org\"",
            ),
            &clover_only,
            "bell.clover.api_url",
        ),
        (
            clover.replace("application_id = \"MONOKULARBELL\"\n", ""),
            &SECRETS[1..],
            "bell.square.application_id or bell.clover.application_id (or both) must be set",
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
            "MONOKULO_BELL_SQUARE_WEBHOOK_KEY is set but bell.square.application_id isn't",
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

/// Started with what it needs, for Square or for Clover alone, it opens
/// its database, listens, answers its status on loopback, and stops
/// cleanly on SIGTERM.
#[cfg(unix)]
#[test]
fn the_bell_listens_serves_its_status_and_stops_on_sigterm_over_http() {
    listens_and_stops(
        "listen-square",
        "[bell.square]\napplication_id = \"sq0idp-Test\"\n",
        &SECRETS,
        &["serving Square"],
    );
    // A service file written for monokular-bell: its secrets' old names
    // are read for a release, each said at start.
    let old: Vec<(String, &str)> = SECRETS
        .iter()
        .map(|(var, value)| (var.replace("MONOKULO_", "MONOKULAR_"), *value))
        .collect();
    let old: Vec<(&str, &str)> = old
        .iter()
        .map(|(var, value)| (var.as_str(), *value))
        .collect();
    listens_and_stops(
        "listen-old-names",
        "[bell.square]\napplication_id = \"sq0idp-Test\"\n",
        &old,
        &[
            "MONOKULAR_BELL_SQUARE_WEBHOOK_KEY was renamed MONOKULO_BELL_SQUARE_WEBHOOK_KEY",
            "MONOKULAR_BELL_VAPID_KEY was renamed MONOKULO_BELL_VAPID_KEY",
            "serving Square",
        ],
    );
    listens_and_stops(
        "listen-clover",
        "[bell.clover]\napplication_id = \"MONOKULARBELL\"\nenvironment = \"sandbox\"\n\n[logging]\nformat = \"json\"\n",
        &[SECRETS[1], CLOVER_CODE],
        &["serving Clover"],
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
