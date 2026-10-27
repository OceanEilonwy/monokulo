//! `monokulo-push-relay` as a process: its options file, its help, and
//! what stops it before it listens.

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

/// Every one of these stops the relay at start, saying why.
#[test]
fn invalid_settings_stop_the_relay_before_it_listens() {
    let dir = dir("invalid");
    let store = dir.join("relay.db");
    let store = store.to_str().unwrap();
    // The options file, the environment, and what the relay says.
    type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);
    let cases: [Case; 6] = [
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
    ];
    for (file, vars, expected) in cases {
        let path = dir.join("relay.toml");
        std::fs::write(&path, format!("[store]\npath = \"{store}\"\n{file}")).unwrap();
        let output = run(&["--options", path.to_str().unwrap()], vars);
        assert_eq!(output.status.code(), Some(1), "{file}: {}", text(&output));
        assert!(
            text(&output).contains(expected),
            "{expected}: {}",
            text(&output)
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// Started with what it needs, it opens its database, listens, and stops
/// cleanly on SIGTERM.
#[cfg(unix)]
#[test]
fn the_relay_listens_and_stops_on_sigterm() {
    use std::io::{BufRead as _, BufReader};

    let dir = dir("listen");
    let path = dir.join("relay.toml");
    let store = dir.join("data").join("push-relay.db");
    std::fs::write(
        &path,
        format!(
            "[server]\nlisten = \"127.0.0.1:0\"\npublic_uri = \"https://relay.example.org\"\n\n[store]\npath = \"{}\"\n\n[logging]\nformat = \"json\"\n",
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
            Ok(line) if line.contains(what) => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    };
    let listening = said("push relay listening");
    if !listening {
        let _ = child.kill();
    }
    assert!(listening, "the relay never said it was listening");
    assert!(store.exists(), "its database is made at store.path");
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
