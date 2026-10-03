//! The monokulo binary and its options file, run as an operator runs it:
//! where the file and the databases go when nothing names them, `--init`,
//! the start refused for a file it can't use, and the engine's mode (inside
//! monokulo by default, docs/engine_as_library.md). The settings themselves
//! are tested in `live-settings` and on the admin page; this is the wiring
//! in `main.rs` that only a real process shows.

use std::net::TcpStream;
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

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Waits for the server to listen on `port`, or for it to exit.
fn listening(child: &mut Child, port: u16) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        if child.try_wait().unwrap().is_some() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
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
    let port = free_port();
    let missing = dir.0.join("nowhere").join("monokulo.toml");
    let mut child = with_secrets(&dir.0)
        .env("XDG_DATA_HOME", &data)
        .arg("--options")
        .arg(&missing)
        .arg("--server-bind")
        .arg(format!("127.0.0.1:{port}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let up = listening(&mut child, port);
    stop(child);
    assert!(up, "monokulo started with no options file");
    assert!(data.join("monokulo").join("monokulo.db").exists());
    assert!(
        data.join("monokulo").join("engine.db").exists(),
        "the engine inside it, its database beside monokulo's"
    );
    assert!(!missing.exists(), "nothing is written until a save");

    let path = dir.0.join("monokulo.toml");
    let port = free_port();
    std::fs::write(
        &path,
        format!(
            "[server]\nbind = \"127.0.0.1:{port}\"\n[database]\npath = \"{}\"\n",
            dir.0.join("mine.db").display()
        ),
    )
    .unwrap();
    let mut child = with_secrets(&dir.0)
        .arg("--options")
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let up = listening(&mut child, port);
    stop(child);
    assert!(up, "monokulo listened where its file says");
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
