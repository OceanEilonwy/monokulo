//! `monokulo-push-relay` as a process: its options file, its help, its
//! version, `--check`, what stops it before it listens, and its status.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BINARY: &str = env!("CARGO_BIN_EXE_monokulo-push-relay");

/// A directory of the test's own.
fn dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "push-relay-binary-{tag}-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The relay run with `args` and nothing in its environment but `vars`.
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
    let path = dir.join("relay.toml");
    let options = path.to_str().unwrap();
    let output = run(&["--options", options, "--init"], &[]);
    assert!(output.status.success(), "{}", text(&output));
    let written = std::fs::read_to_string(&path).unwrap();
    for line in [
        "#   MONOKULO_RELAY_HANDLE_KEY\n",
        "#   MONOKULO_RELAY_APNS_KEY\n",
        "#   MONOKULO_RELAY_FCM_CREDENTIALS\n",
        "[server]\n",
        "# listen = \"0.0.0.0:8444\"\n",
        "[handles]\n",
        "# ttl_days = 90\n",
        "[limits]\n",
        "[apns]\n",
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
fn the_help_names_every_secret() {
    let output = run(&["--help"], &[]);
    assert!(output.status.success());
    let help = text(&output);
    for secret in [
        "MONOKULO_RELAY_HANDLE_KEY",
        "MONOKULO_RELAY_APNS_KEY",
        "MONOKULO_RELAY_FCM_CREDENTIALS",
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
        version.starts_with(&format!(
            "monokulo-push-relay {}",
            env!("CARGO_PKG_VERSION")
        )),
        "{version}"
    );
}

/// `--check` says what the relay would run on, then OK, and opens,
/// makes and binds nothing: not the database, not its directory, not
/// the ports (taken here, which a start would fail on).
#[test]
fn check_says_what_the_relay_would_run_on_and_touches_nothing() {
    let dir = dir("check");
    let store = dir.join("data").join("push-relay.db");
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let path = dir.join("relay.toml");
    std::fs::write(
        &path,
        format!(
            "[server]\nlisten = \"127.0.0.1:{port}\"\npublic_uri = \"https://relay.example.org\"\n\n[store]\npath = '{}'\n\n[status]\nlisten = \"127.0.0.1:{port}\"\n\n[handles.apps.monokular]\nfcm_project_id = \"monokular-test\"\n",
            store.display()
        ),
    )
    .unwrap();
    let account = serde_json::json!({
        "type": "service_account",
        "project_id": "monokular-test",
        "private_key_id": "0123456789abcdef0123456789abcdef01234567",
        "private_key": include_str!("fixtures/fcm-test-key.pem"),
        "client_email": "relay@monokular-test.iam.gserviceaccount.com",
        "client_id": "100000000000000000000",
        "token_uri": "https://oauth2.googleapis.com/token",
    })
    .to_string();
    let output = run(
        &["--options", path.to_str().unwrap(), "--check"],
        &[
            ("MONOKULO_RELAY_HANDLE_KEY", &"0123456789abcdef".repeat(4)),
            ("MONOKULO_RELAY_FCM_CREDENTIALS", &account),
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let said = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = said.lines().collect();
    assert_eq!(
        lines,
        [
            format!("listens on 127.0.0.1:{port}").as_str(),
            "public address https://relay.example.org",
            "apps: monokular",
            "APNs: not configured",
            "FCM: ready",
            format!("handles in {}", store.display()).as_str(),
            format!("status on 127.0.0.1:{port}").as_str(),
            "OK",
        ],
        "{}",
        text(&output)
    );
    assert!(output.stderr.is_empty(), "{}", text(&output));
    assert!(!store.parent().unwrap().exists(), "nothing was made");
    drop(taken);
    let _ = std::fs::remove_dir_all(dir);
}

/// Every one of these stops the relay at start, saying why, and `--check`
/// says the same, with the same status.
#[test]
fn invalid_settings_stop_the_relay_before_it_listens() {
    let dir = dir("invalid");
    let store = dir.join("relay.db");
    let store = store.to_str().unwrap();
    // The options file, the environment, and what the relay says.
    type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);
    let cases: [Case; 7] = [
        (
            "[handles.apps.monokular]\napns_topik = \"com.example.app\"\n",
            &[],
            "handles.apps",
        ),
        (
            "[server]\nlisten = \"127.0.0.1:0\"\n",
            &[],
            "server.public_uri must be set",
        ),
        (
            "[server]\npublic_uri = \"https://relay.example.org\"\n",
            &[("MONOKULO_RELAY_APNS_KEY", "not a key")],
            "MONOKULO_RELAY_APNS_KEY",
        ),
        (
            "[server]\npublic_uri = \"https://relay.example.org\"\n",
            &[],
            "MONOKULO_RELAY_HANDLE_KEY must be set",
        ),
        (
            "[server]\npublic_uri = \"https://relay.example.org\"\n",
            &[("MONOKULO_RELAY_HANDLE_KEY", "too short")],
            "handles.id_key",
        ),
        (
            "[relay.server]\nlisten = \"127.0.0.1:0\"\n",
            &[],
            "no setting called relay.server.listen",
        ),
        (
            "[server]\npublic_uri = \"https://relay.example.org\"\n\n[status]\nlisten = \"0.0.0.0:8446\"\n",
            &[("MONOKULO_RELAY_HANDLE_KEY", "0123456789abcdef0123456789abcdef")],
            "isn't a loopback address",
        ),
    ];
    for (file, vars, expected) in cases {
        let path = dir.join("relay.toml");
        // A literal string, so a Windows path's backslashes are not escapes.
        std::fs::write(&path, format!("[store]\npath = '{store}'\n{file}")).unwrap();
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
    let _ = std::fs::remove_dir_all(dir);
}

/// Started with what it needs, it opens its database, listens, answers
/// its status on loopback, and stops cleanly on SIGTERM.
#[cfg(unix)]
#[test]
fn the_relay_listens_serves_its_status_and_stops_on_sigterm_over_http() {
    use std::io::{BufRead as _, BufReader};

    let dir = dir("listen");
    let path = dir.join("relay.toml");
    let store = dir.join("data").join("push-relay.db");
    std::fs::write(
        &path,
        format!(
            "[server]\nlisten = \"127.0.0.1:0\"\npublic_uri = \"https://relay.example.org\"\n\n[store]\npath = '{}'\n\n[status]\nlisten = \"127.0.0.1:0\"\n\n[logging]\nformat = \"json\"\n",
            store.display()
        ),
    )
    .unwrap();
    let mut child = Command::new(BINARY)
        .args(["--options", path.to_str().unwrap()])
        .env_clear()
        .env("MONOKULO_RELAY_HANDLE_KEY", "0123456789abcdef".repeat(4))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Log lines go to stderr; read on a thread, so a relay that never
    // says it is listening fails the test instead of hanging it.
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
            Ok(line) if line.contains(what) => return Some(line),
            Ok(_) => {}
            Err(_) => return None,
        }
    };
    let listening = said("push relay listening").and_then(|_| said("push relay status listening"));
    if listening.is_none() {
        let _ = child.kill();
    }
    let line = listening.expect("the relay never said it was listening");
    assert!(store.exists(), "its database is made at store.path");
    let status = status_of(&line);
    assert!(
        status.contains("\"service\":\"monokulo-push-relay\""),
        "{status}"
    );
    assert!(status.contains("\"handles\":{\"stored\":0}"), "{status}");
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
