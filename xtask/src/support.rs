//! What the commands share: the repository root, the exit code, JSON files
//! read and written whole, directory walks, the links in a report page,
//! HTML escaping, waiting on a child with a deadline, and the tests' scratch
//! folders.

use regex::Regex;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
    process::{Child, ExitCode, ExitStatus},
    sync::LazyLock,
    time::{Duration, Instant},
};

pub(crate) fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one folder below the repository root")
        .to_path_buf()
}

/// The code xtask ends with. A command's own verdict is 0 or 1; the engine
/// commands pass on the code of the tests or fuzzer they ran, so a workflow
/// sees the failure they saw.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Exit(u8);

impl Exit {
    pub(crate) const SUCCESS: Self = Exit(0);
    pub(crate) const FAILURE: Self = Exit(1);
    /// `timeout(1)`'s code for a step its watchdog stopped.
    pub(crate) const TIMED_OUT: Self = Exit(124);
    /// The shell's code for a program that could not be started.
    pub(crate) const NOT_FOUND: Self = Exit(127);

    pub(crate) const fn new(code: u8) -> Self {
        Exit(code)
    }

    pub(crate) fn passed(ok: bool) -> Self {
        if ok {
            Self::SUCCESS
        } else {
            Self::FAILURE
        }
    }

    /// How a child ended: its code, or 128 plus the signal that stopped it,
    /// as the shell reports one. A code outside a byte is clamped into one.
    pub(crate) fn of(status: ExitStatus) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(signal) = status.signal() {
                return Exit(u8::try_from(128 + signal).unwrap_or(u8::MAX));
            }
        }
        Exit(
            status
                .code()
                .map_or(1, |c| u8::try_from(c).unwrap_or(u8::MAX)),
        )
    }

    pub(crate) fn succeeded(self) -> bool {
        self == Self::SUCCESS
    }
}

impl From<Exit> for ExitCode {
    fn from(exit: Exit) -> Self {
        ExitCode::from(exit.0)
    }
}

impl fmt::Display for Exit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// An error about a file: `path: error`, keeping the kind and the error
/// itself, so callers can still tell a missing file from a malformed one.
#[derive(Debug)]
struct At {
    path: PathBuf,
    error: io::Error,
}

impl fmt::Display for At {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.error)
    }
}

impl std::error::Error for At {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

pub(crate) fn at(path: &Path, error: io::Error) -> io::Error {
    let kind = error.kind();
    io::Error::new(
        kind,
        At {
            path: path.to_path_buf(),
            error,
        },
    )
}

/// A JSON file as `T`; the error names the file.
pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<T> {
    let text = fs::read(path).map_err(|e| at(path, e))?;
    serde_json::from_slice(&text)
        .map_err(|e| at(path, io::Error::new(io::ErrorKind::InvalidData, e)))
}

fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path.file_name().ok_or_else(|| {
        at(
            path,
            io::Error::new(io::ErrorKind::InvalidInput, "no file name"),
        )
    })?;
    let mut pending = name.to_os_string();
    pending.push(".tmp");
    let pending = path.with_file_name(pending);
    fs::write(&pending, bytes).map_err(|e| at(&pending, e))?;
    fs::rename(&pending, path).map_err(|e| at(path, e))
}

/// Writes `value` as readable JSON, whole or not at all, so an interrupted
/// run never leaves half a report.
pub(crate) fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> io::Result<()> {
    let mut text = serde_json::to_vec_pretty(value)?;
    text.push(b'\n');
    write_atomically(path, &text)
}

/// Every file under `dir`, sorted, so a search finds the same one each time.
/// Folders `prune` says yes to are left out, with everything under them.
pub(crate) fn files_under(
    dir: &Path,
    mut prune: impl FnMut(&Path) -> bool,
) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).map_err(|e| at(&dir, e))? {
            let path = entry.map_err(|e| at(&dir, e))?.path();
            if path.is_dir() {
                if !prune(&path) {
                    stack.push(path);
                }
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

pub(crate) fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// `%XX` escapes decoded; anything else is left as it is.
pub(crate) fn unquote(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What a browser doesn't read as markup: comments, and the bodies of
/// scripts and styles (a script's own `src` still counts).
static NOT_MARKUP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)<!--.*?-->|(<script\b[^>]*>).*?(</script>)|(<style\b[^>]*>).*?(</style>)")
        .unwrap()
});

static LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<(a|img|script|link)\b[^>]*?\s(?:href|src)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+))"#).unwrap()
});

static CSS_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"url\(\s*(?:"([^"]*)"|'([^']*)'|([^'")\s]+))\s*\)"#).unwrap());

/// A link in a page or stylesheet that names a file beside it: the schemes,
/// fragments and inline data a page can carry are not files.
fn local(reference: &str) -> Option<String> {
    let reference = reference.replace("&amp;", "&");
    if ["http:", "https:", "data:", "#", "mailto:", "javascript:"]
        .iter()
        .any(|p| reference.starts_with(p))
    {
        return None;
    }
    let target = unquote(reference.split(['#', '?']).next().unwrap_or(""));
    (!target.is_empty()).then_some(target)
}

/// The files a report page links or loads, relative to the page, as the
/// browser would resolve them.
pub(crate) fn html_links(page: &str) -> Vec<String> {
    let text = NOT_MARKUP.replace_all(page, "$1$2$3$4");
    LINK.captures_iter(&text)
        .filter_map(|found| {
            let reference = (2..=4)
                .find_map(|i| found.get(i))
                .map_or("", |m| m.as_str());
            local(reference)
        })
        .collect()
}

/// The files a stylesheet loads, relative to it.
pub(crate) fn css_links(sheet: &str) -> Vec<String> {
    CSS_URL
        .captures_iter(sheet)
        .filter_map(|found| {
            let reference = (1..=3)
                .find_map(|i| found.get(i))
                .map_or("", |m| m.as_str());
            local(reference)
        })
        .collect()
}

/// What to stop when a child outlives its deadline.
#[derive(Clone, Copy)]
pub(crate) enum OnTimeout {
    /// The child alone.
    KillChild,
    /// The child's whole process group, which it must lead
    /// (`CommandExt::process_group(0)`): cargo's rustc and test children
    /// would otherwise outlive a timed-out run.
    KillGroup,
}

/// Waits for `child` until `limit`; `None` when it ran out and was stopped.
pub(crate) fn wait_with_deadline(
    child: &mut Child,
    limit: Duration,
    on_timeout: OnTimeout,
) -> io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            #[cfg(unix)]
            if let (OnTimeout::KillGroup, Ok(pid)) = (on_timeout, libc::pid_t::try_from(child.id()))
            {
                // SAFETY: kill(2) on the process group our own child leads.
                unsafe { libc::kill(-pid, libc::SIGKILL) };
            }
            #[cfg(not(unix))]
            let _ = on_timeout;
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A folder of its own under the system temp dir, removed when dropped.
/// Named by the test and a counter as well as the process, so tests in one
/// process never share one.
#[cfg(test)]
pub(crate) struct Scratch(PathBuf);

#[cfg(test)]
impl Scratch {
    pub(crate) fn new(name: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("xtask-{name}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    pub(crate) fn join(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}

#[cfg(test)]
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_page_links_only_the_files_beside_it() {
        let links = html_links(
            r##"<link rel="stylesheet" href="css/a%20b.css"><a href="#top">x</a><a href="https://x">y</a>
            <img src='data:image/png;base64,'><a href="index.html?x#y">z</a><script src=plain.js></script>
            <!-- <a href="old.html"> --><script>const a = "<a href=\"x.html\">";</script><style>a{background:url(nope.png)}</style>"##,
        );
        assert_eq!(links, ["css/a b.css", "index.html", "plain.js"]);
        assert_eq!(
            css_links(
                r#"a{background:url(sort.png)}b{background:url("data:image/svg+xml,x")}c{src:url( 'fonts/x.woff2' )}"#
            ),
            ["sort.png", "fonts/x.woff2"]
        );
    }

    #[test]
    fn json_files_are_written_whole_and_errors_name_the_file() {
        let scratch = Scratch::new("json");
        let path = scratch.join("report.json");
        write_json(&path, &serde_json::json!({"a": 1})).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\n  \"a\": 1\n}\n");
        assert!(!scratch.join("report.json.tmp").exists());
        let missing = read_json::<serde_json::Value>(&scratch.join("none.json")).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
        assert!(missing.to_string().contains("none.json"));
        fs::write(&path, "{").unwrap();
        let broken = read_json::<serde_json::Value>(&path).unwrap_err();
        assert_eq!(broken.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_walk_can_leave_folders_out() {
        let scratch = Scratch::new("walk");
        fs::create_dir_all(scratch.join("keep/deeper")).unwrap();
        fs::create_dir_all(scratch.join("corpus")).unwrap();
        fs::write(scratch.join("keep/deeper/a"), "").unwrap();
        fs::write(scratch.join("corpus/b"), "").unwrap();
        fs::write(scratch.join("c"), "").unwrap();
        let found = files_under(scratch.path(), |p| p.ends_with("corpus")).unwrap();
        let names: Vec<_> = found
            .iter()
            .map(|p| {
                p.strip_prefix(scratch.path())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["c", "keep/deeper/a"]);
        assert!(files_under(&scratch.join("missing"), |_| false).is_err());
    }

    #[test]
    fn exit_codes_pass_a_child_status_through() {
        assert_eq!(Exit::passed(true), Exit::SUCCESS);
        assert_eq!(Exit::passed(false), Exit::FAILURE);
        assert_eq!(serde_json::to_value(Exit::TIMED_OUT).unwrap(), 124);
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(Exit::of(ExitStatus::from_raw(19 << 8)), Exit::new(19));
            assert_eq!(Exit::of(ExitStatus::from_raw(9)), Exit::new(137));
        }
    }
}
