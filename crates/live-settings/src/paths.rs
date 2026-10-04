//! Where a process keeps its files when nothing says otherwise: the XDG
//! base directories (`$XDG_CONFIG_HOME`, else `~/.config`; `$XDG_DATA_HOME`,
//! else `~/.local/share`) on every system, macOS included, so one path in
//! the docs holds everywhere. Windows, which sets no `HOME`, uses its own
//! (`%APPDATA%` and `%LOCALAPPDATA%`). A directory that can't be used falls
//! back to the working directory.

use std::path::{Path, PathBuf};

/// The directory every monokulo process keeps its files under, inside
/// each base directory.
pub const APP_DIR: &str = "monokulo";

/// `$XDG_CONFIG_HOME/monokulo/<file>`, else `~/.config/monokulo/<file>`,
/// else (on Windows) `%APPDATA%\monokulo\<file>`; `None` without any of
/// them.
pub fn config_file(file: &str) -> Option<PathBuf> {
    base("XDG_CONFIG_HOME", ".config", "APPDATA").map(|dir| dir.join(APP_DIR).join(file))
}

/// `$XDG_DATA_HOME/monokulo/<file>`, else `~/.local/share/monokulo/<file>`,
/// else (on Windows) `%LOCALAPPDATA%\monokulo\<file>`; `None` without any
/// of them.
pub fn data_file(file: &str) -> Option<PathBuf> {
    base("XDG_DATA_HOME", ".local/share", "LOCALAPPDATA").map(|dir| dir.join(APP_DIR).join(file))
}

/// The base directory a variable names (an absolute path, as the XDG spec
/// requires), else `$HOME/<fallback>`, else on Windows the directory
/// `windows` names.
fn base(var: &str, fallback: &str, windows: &str) -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
    };
    absolute(var)
        .or_else(|| absolute("HOME").map(|home| home.join(fallback)))
        .or_else(|| {
            if cfg!(windows) {
                absolute(windows)
            } else {
                None
            }
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

/// The file a write to `path` changes: the target of a symlink (so a
/// linked options file stays linked), else `path` itself.
pub fn resolved(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether a file at `path` can be written the way an options file is
/// saved: an existing one must itself be writable (a read-only file is the
/// owner saying no), and its directory must take a new file, which is
/// written beside it and renamed over it. A missing directory is made.
pub fn writable(path: &Path) -> bool {
    let target = resolved(path);
    if target.exists()
        && std::fs::OpenOptions::new()
            .append(true)
            .open(&target)
            .is_err()
    {
        return false;
    }
    let dir = match target.parent() {
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
        let file = dir.join("a-file");
        std::fs::write(&file, "").unwrap();
        let under_a_file = file.join("nowhere").join("engine.toml");
        assert_eq!(
            usable_or_cwd(Some(under_a_file.clone()), "engine.toml"),
            PathBuf::from("engine.toml")
        );
        assert_eq!(
            usable_or_cwd(None, "engine.toml"),
            PathBuf::from("engine.toml")
        );
        assert!(!writable(&under_a_file));
        let _ = std::fs::remove_dir_all(dir);
    }
}
