//! The engine binary and its options file, run as an operator runs it: where
//! the file and the database go when nothing names them, `--init`, and the
//! start refused for a file it can't use. The settings themselves are tested
//! in `live-settings` and through the admin API; this is the wiring in
//! `main.rs` that only a real process shows.

// An integration test crate: every function in it is test code, which
// fails by panicking.
#![expect(
    clippy::tests_outside_test_module,
    clippy::unwrap_used,
    reason = "an integration test crate is all test code"
)]

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_monokulo-engine");
const TOKEN: &str = "options-file-test-token-0123456789abcdef";

/// A temporary directory of its own, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("engine-options-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The engine with nothing from the caller's environment but what is
/// given: no `ENGINE_*`, and a home of its own, so nothing here reads or
/// writes a real `~/.config`.
fn engine(dir: &Path) -> Command {
    let mut command = Command::new(BIN);
    command
        .env_clear()
        .env("HOME", dir.join("home"))
        .current_dir(dir)
        .stdin(Stdio::null());
    command
}

fn run(command: &mut Command) -> Output {
    command.output().unwrap()
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

/// With no `--options`, `--init` writes `$XDG_CONFIG_HOME/monokulo/engine.toml`
/// (else `~/.config/monokulo/`), says where, and never overwrites it.
#[test]
fn init_writes_the_options_file_where_xdg_says_and_never_overwrites_it() {
    let dir = TempDir::new("init");
    let config = dir.0.join("config");
    let output = run(engine(&dir.0).env("XDG_CONFIG_HOME", &config).arg("--init"));
    assert!(output.status.success(), "{}", text(&output));
    let path = config.join("monokulo").join("engine.toml");
    assert!(
        text(&output).contains(&format!("Wrote {}", path.display())),
        "{}",
        text(&output)
    );
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        written.contains("# Options file for monokulo-engine"),
        "{written}"
    );
    assert!(written.contains("#   ENGINE_TOKEN (required)"), "{written}");
    assert!(!written.contains(TOKEN));

    let again = run(engine(&dir.0).env("XDG_CONFIG_HOME", &config).arg("--init"));
    assert!(!again.status.success());
    assert!(text(&again).contains("already exists"), "{}", text(&again));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), written);

    let home = run(engine(&dir.0).arg("--init"));
    assert!(home.status.success(), "{}", text(&home));
    assert!(dir.0.join("home/.config/monokulo/engine.toml").exists());
}

/// No home and no XDG variables: the file goes in the working directory.
#[test]
fn without_a_home_the_options_file_is_in_the_working_directory() {
    let dir = TempDir::new("cwd");
    let output = run(engine(&dir.0).env_remove("HOME").arg("--init"));
    assert!(output.status.success(), "{}", text(&output));
    assert!(dir.0.join("engine.toml").exists());
}

/// Windows sets no `HOME`: the file goes under `%APPDATA%`.
#[cfg(windows)]
#[test]
fn on_windows_without_a_home_the_options_file_is_in_appdata() {
    let dir = TempDir::new("appdata");
    let appdata = dir.0.join("appdata");
    let output = run(engine(&dir.0)
        .env_remove("HOME")
        .env("APPDATA", &appdata)
        .arg("--init"));
    assert!(output.status.success(), "{}", text(&output));
    assert!(appdata.join("monokulo").join("engine.toml").exists());
    assert!(!dir.0.join("engine.toml").exists());
}

/// A file with anything wrong in it stops the engine before it does
/// anything, naming each problem by line; so does one it can't read, and a
/// missing engine token. None of them creates a database.
#[test]
fn the_engine_does_not_start_on_a_file_it_cannot_use() {
    let dir = TempDir::new("refused");
    let path = dir.0.join("engine.toml");
    std::fs::write(
        &path,
        "[payment]\nconfirmations_required = 7000\n\n[server]\ntoken = \"x\"\nnot_a_setting = 1\n",
    )
    .unwrap();
    let output = run(engine(&dir.0)
        .env("ENGINE_TOKEN", TOKEN)
        .arg("--options")
        .arg(&path));
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    let said = text(&output);
    assert!(
        said.contains("line 2: payment.confirmations_required"),
        "{said}"
    );
    assert!(said.contains("line 5: server.token can't be in the options file: it is a secret: set ENGINE_TOKEN in the environment"), "{said}");
    assert!(
        said.contains("line 6: there is no setting called server.not_a_setting"),
        "{said}"
    );

    std::fs::write(&path, "[payment\n").unwrap();
    let output = run(engine(&dir.0)
        .env("ENGINE_TOKEN", TOKEN)
        .arg("--options")
        .arg(&path));
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("line 1"), "{}", text(&output));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::write(&path, "").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_err() {
            let output = run(engine(&dir.0)
                .env("ENGINE_TOKEN", TOKEN)
                .arg("--options")
                .arg(&path));
            assert_eq!(output.status.code(), Some(1), "{}", text(&output));
            assert!(text(&output).contains("can't be read"), "{}", text(&output));
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    std::fs::write(&path, "").unwrap();
    let output = run(engine(&dir.0).arg("--options").arg(&path));
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains("ENGINE_TOKEN must be set"),
        "{}",
        text(&output)
    );
    assert!(
        !dir.0.join("home/.local/share/monokulo/engine.db").exists(),
        "no database was made"
    );
}

/// A missing options file is no problem: the engine starts on its
/// defaults, its database where XDG says, and the file is made by the
/// first save. A file that names the database and the address is
/// followed, and an option wins over it.
#[test]
fn the_engine_starts_without_a_file_and_follows_one_and_its_options() {
    let dir = TempDir::new("start");
    let data = dir.0.join("data");
    let port = free_port();
    let missing = dir.0.join("nowhere").join("engine.toml");
    let mut child = engine(&dir.0)
        .env("ENGINE_TOKEN", TOKEN)
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
    assert!(up, "the engine started with no options file");
    assert!(data.join("monokulo").join("engine.db").exists());
    assert!(!missing.exists(), "nothing is written until a save");

    let path = dir.0.join("engine.toml");
    let file_port = free_port();
    let option_port = free_port();
    std::fs::write(
        &path,
        format!(
            "[server]\nbind = \"127.0.0.1:{file_port}\"\n[database]\npath = \"{}\"\n",
            dir.0.join("mine.db").display()
        ),
    )
    .unwrap();
    let mut child = engine(&dir.0)
        .env("ENGINE_TOKEN", TOKEN)
        .arg("--options")
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let up = listening(&mut child, file_port);
    stop(child);
    assert!(up, "the engine listened where its file says");
    assert!(dir.0.join("mine.db").exists());

    let mut child = engine(&dir.0)
        .env("ENGINE_TOKEN", TOKEN)
        .arg("--options")
        .arg(&path)
        .arg("--server-bind")
        .arg(format!("127.0.0.1:{option_port}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let up = listening(&mut child, option_port);
    stop(child);
    assert!(up, "the option wins over the file");
}
