//! The monokulo binary and its options file, run as an operator runs it:
//! where the file and the databases go when nothing names them, `--init`,
//! the start refused for a file it can't use, and the engine's mode (inside
//! monokulo by default, docs/engine_as_library.md). The settings themselves
//! are tested in `live-settings` and on the admin page; this is the wiring
//! in `main.rs` that only a real process shows.

use std::io::{BufRead as _, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_monokulo");
const ENGINE_TOKEN: &str = "options-file-test-token-0123456789abcdef";

fn encryption_key() -> String {
    "07".repeat(32)
}

/// A temporary directory of its own, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let dir =
            std::env::temp_dir().join(format!("monokulo-options-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// monokulo with nothing from the caller's environment but what is given,
/// and a home of its own, so nothing here reads or writes a real
/// `~/.config`.
fn monokulo(dir: &Path) -> Command {
    let mut command = Command::new(BIN);
    command
        .env_clear()
        .env("HOME", dir.join("home"))
        .current_dir(dir)
        .stdin(Stdio::null());
    // Windows can't open a socket without it.
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
    command
}

/// [`monokulo`] with its one required secret: the engine runs inside it,
/// so it needs no engine token.
fn with_secrets(dir: &Path) -> Command {
    let mut command = monokulo(dir);
    command.env("MONOKULO_ENCRYPTION_KEY", encryption_key());
    command
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Starts `command` with its log piped back, and waits until it says it's
/// listening (its own "monokulo listening" log line): the address it listens on, as it
/// bound it, so a port of 0 says which port it got. The rest of its log is
/// read in the background, so it never blocks on a full pipe. `Err` is its
/// log, when it exits first or doesn't say it's listening within 30
/// seconds.
fn start_listening(command: &mut Command, said: &str) -> Result<(Child, SocketAddr), String> {
    let mut child = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("it didn't start: {e}"))?;
    let stderr = child.stderr.take().ok_or("its log isn't piped")?;
    let (lines, read) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut log = String::new();
    while let Ok(line) = read.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        if let Some(address) = listening_on(&line, said) {
            std::thread::spawn(move || read.into_iter().for_each(drop));
            return Ok((child, address));
        }
        log.push_str(&line);
        log.push('\n');
    }
    stop(child);
    Err(log)
}

/// The address a "`said`" log line (one JSON object) says it listens on.
fn listening_on(line: &str, said: &str) -> Option<SocketAddr> {
    let line: serde_json::Value = serde_json::from_str(line).ok()?;
    if line["message"] != said {
        return None;
    }
    line["attributes"]["server.address"].as_str()?.parse().ok()
}

fn stop(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// With no `--options`, `--init` writes `$XDG_CONFIG_HOME/monokulo/monokulo.toml`
/// (else `~/.config/monokulo/`, else the working directory), says where,
/// and never overwrites it.
#[test]
fn init_writes_the_options_file_where_xdg_says_and_never_overwrites_it() {
    let dir = TempDir::new("init");
    let config = dir.0.join("config");
    let output = monokulo(&dir.0)
        .env("XDG_CONFIG_HOME", &config)
        .arg("--init")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output));
    let path = config.join("monokulo").join("monokulo.toml");
    assert!(
        text(&output).contains(&format!("Wrote {}", path.display())),
        "{}",
        text(&output)
    );
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        written.contains("#   MONOKULO_ENCRYPTION_KEY (required)"),
        "{written}"
    );
    assert!(
        !written.contains("under_attack"),
        "a runtime switch: {written}"
    );
    assert!(written.contains("# mode = \"embedded\""), "{written}");
    assert!(
        written.contains("[engine.payment]\n"),
        "the engine's own settings, under [engine.*]: {written}"
    );
    assert!(
        !written.contains("[engine.logging]"),
        "the standalone engine's own: {written}"
    );

    let again = monokulo(&dir.0)
        .env("XDG_CONFIG_HOME", &config)
        .arg("--init")
        .output()
        .unwrap();
    assert!(!again.status.success());
    assert!(text(&again).contains("already exists"), "{}", text(&again));

    let home = monokulo(&dir.0).arg("--init").output().unwrap();
    assert!(home.status.success(), "{}", text(&home));
    assert!(dir.0.join("home/.config/monokulo/monokulo.toml").exists());

    let cwd = monokulo(&dir.0)
        .env_remove("HOME")
        .arg("--init")
        .output()
        .unwrap();
    assert!(cwd.status.success(), "{}", text(&cwd));
    assert!(dir.0.join("monokulo.toml").exists());
}

/// A file with anything wrong in it, one it can't read, or a missing
/// secret stops monokulo before it opens a database.
#[test]
fn monokulo_does_not_start_on_a_file_it_cannot_use() {
    let dir = TempDir::new("refused");
    let path = dir.0.join("monokulo.toml");
    std::fs::write(
        &path,
        "public_url = \"not a url\"\n[abuse]\nunder_attack = true\n[engine]\ntoken = \"x\"\n",
    )
    .unwrap();
    let output = with_secrets(&dir.0)
        .arg("--options")
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    let said = text(&output);
    assert!(said.contains("line 1: public_url"), "{said}");
    assert!(
        said.contains(
            "line 3: abuse.under_attack can't be in the options file: the admin page keeps it"
        ),
        "{said}"
    );
    assert!(
        said.contains("line 5: engine.token can't be in the options file: it is a secret"),
        "{said}"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&path, "").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_err() {
            let output = with_secrets(&dir.0)
                .arg("--options")
                .arg(&path)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1), "{}", text(&output));
            assert!(text(&output).contains("can't be read"), "{}", text(&output));
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    std::fs::write(&path, "").unwrap();
    let output = monokulo(&dir.0)
        .arg("--options")
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains("MONOKULO_ENCRYPTION_KEY must be set"),
        "{}",
        text(&output)
    );
    assert!(!dir
        .0
        .join("home/.local/share/monokulo/monokulo.db")
        .exists());
}

/// A missing options file is no problem: monokulo starts on its defaults,
/// the engine inside it, both databases where XDG says, writing no file
/// until a save. A file naming the address and database is followed, the
/// engine's database beside monokulo's.
#[test]
fn monokulo_starts_without_a_file_and_follows_one() {
    let dir = TempDir::new("start");
    let data = dir.0.join("data");
    let missing = dir.0.join("nowhere").join("monokulo.toml");
    let (child, _) = start_listening(
        with_secrets(&dir.0)
            .env("XDG_DATA_HOME", &data)
            .arg("--options")
            .arg(&missing)
            .arg("--server-bind")
            .arg("127.0.0.1:0"),
        "monokulo listening",
    )
    .unwrap();
    stop(child);
    assert!(data.join("monokulo").join("monokulo.db").exists());
    assert!(
        data.join("monokulo").join("engine.db").exists(),
        "the engine inside it, its database beside monokulo's"
    );
    assert!(!missing.exists(), "nothing is written until a save");

    // Listening where its file says: any free port, not its default 8081
    // (the path as a TOML literal string: a Windows path's backslashes
    // aren't escapes).
    let path = dir.0.join("monokulo.toml");
    std::fs::write(
        &path,
        format!(
            "[server]\nbind = \"127.0.0.1:0\"\n[database]\npath = '{}'\n",
            dir.0.join("mine.db").display()
        ),
    )
    .unwrap();
    let (child, address) = start_listening(
        with_secrets(&dir.0).arg("--options").arg(&path),
        "monokulo listening",
    )
    .unwrap();
    stop(child);
    assert_ne!(
        address.port(),
        8081,
        "monokulo listened where its file says"
    );
    assert!(dir.0.join("mine.db").exists());
    assert!(dir.0.join("engine.db").exists());
}

/// What only one mode of the engine uses is refused in the other, before
/// any database opens, rather than ignored: the engine token or URL with
/// the engine inside monokulo, the standalone engine's own settings in
/// `[engine.*]`, `[engine.*]` with a remote engine, and a remote engine
/// without its token.
#[test]
fn settings_for_the_other_engine_mode_stop_monokulo() {
    let dir = TempDir::new("modes");
    let run = |file: &str, extra: &[(&str, &str)]| {
        let path = dir.0.join("monokulo.toml");
        std::fs::write(&path, file).unwrap();
        let mut command = with_secrets(&dir.0);
        command.arg("--options").arg(&path);
        for (name, value) in extra {
            command.env(name, value);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{}", text(&output));
        text(&output)
    };
    let said = run("", &[("MONOKULO_ENGINE_TOKEN", ENGINE_TOKEN)]);
    assert!(
        said.contains("MONOKULO_ENGINE_TOKEN only apply to a remote engine"),
        "{said}"
    );
    let said = run("[engine]\nurl = \"http://engine:8443\"\n", &[]);
    assert!(
        said.contains("engine.url only apply to a remote engine"),
        "{said}"
    );
    let said = run("[engine.server]\nbind = \"0.0.0.0:8443\"\n", &[]);
    assert!(
        said.contains("engine.server.bind in the options file: these only apply to the engine running on its own"),
        "{said}"
    );
    let said = run(
        "[engine]\nmode = \"remote\"\n\n[engine.payment]\nconfirmations_required = 3\n",
        &[("MONOKULO_ENGINE_TOKEN", ENGINE_TOKEN)],
    );
    assert!(
        said.contains("there is no setting called engine.payment.confirmations_required: the engine's own settings go here only when it runs inside monokulo"),
        "{said}"
    );
    let said = run("[engine]\nmode = \"remote\"\n", &[]);
    assert!(
        said.contains("MONOKULO_ENGINE_TOKEN must be set: engine.mode is remote"),
        "{said}"
    );
    assert!(
        !dir.0.join("home/.local/share/monokulo").exists(),
        "nothing opened"
    );
}
