//! Where a process keeps its files when nothing says otherwise: the XDG
//! base directories (`$XDG_CONFIG_HOME`, else `~/.config`; `$XDG_DATA_HOME`,
//! else `~/.local/share`) on every system, macOS included, so one path in
//! the docs holds everywhere. A directory that can't be used falls back to
//! the working directory.

use std::path::{Path, PathBuf};

/// The directory every monokulo process keeps its files under, inside
/// each base directory.
pub const APP_DIR: &str = "monokulo";

/// `$XDG_CONFIG_HOME/monokulo/<file>`, else `~/.config/monokulo/<file>`;
/// `None` without either variable.
pub fn config_file(file: &str) -> Option<PathBuf> {
    base("XDG_CONFIG_HOME", ".config").map(|dir| dir.join(APP_DIR).join(file))
}

/// `$XDG_DATA_HOME/monokulo/<file>`, else `~/.local/share/monokulo/<file>`;
/// `None` without either variable.
pub fn data_file(file: &str) -> Option<PathBuf> {
    base("XDG_DATA_HOME", ".local/share").map(|dir| dir.join(APP_DIR).join(file))
}

/// The base directory a variable names (an absolute path, as the XDG spec
/// requires), else `$HOME/<fallback>`.
fn base(var: &str, fallback: &str) -> Option<PathBuf> {
    let named = std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute());
    named.or_else(|| {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .map(|home| home.join(fallback))
    })
}

/// `preferred` if its directory exists or can be made, else `<file>` in the
/// working directory: where a file goes when no option names it.
pub fn usable_or_cwd(preferred: Option<PathBuf>, file: &str) -> PathBuf {
    match preferred {
        Some(path) if path.parent().is_none_or(dir_usable) => path,
        _ => PathBuf::from(file),
    }
}

/// Whether `dir` exists as a directory, or can be created.
fn dir_usable(dir: &Path) -> bool {
    dir.is_dir() || std::fs::create_dir_all(dir).is_ok()
}

/// Whether a file at `path` can be written: an existing one opened for
/// writing, or a new one created in its directory (made if need be).
pub fn writable(path: &Path) -> bool {
    if path.exists() {
        return std::fs::OpenOptions::new().append(true).open(path).is_ok();
    }
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    if !dir_usable(dir) {
        return false;
    }
    let probe = dir.join(format!(".{}.probe", std::process::id()));
    let made = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .is_ok();
    let _ = std::fs::remove_file(&probe);
    made
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_directory_that_cannot_be_made_falls_back_to_the_working_directory() {
        let dir = std::env::temp_dir().join(format!("live-settings-paths-{}", std::process::id()));
        let wanted = dir.join("monokulo").join("engine.toml");
        assert_eq!(usable_or_cwd(Some(wanted.clone()), "engine.toml"), wanted);
        assert!(writable(&wanted));
        assert_eq!(
            usable_or_cwd(
                Some(PathBuf::from("/proc/nowhere/engine.toml")),
                "engine.toml"
            ),
            PathBuf::from("engine.toml")
        );
        assert_eq!(
            usable_or_cwd(None, "engine.toml"),
            PathBuf::from("engine.toml")
        );
        assert!(!writable(Path::new("/proc/nowhere/engine.toml")));
        let _ = std::fs::remove_dir_all(dir);
    }
}
