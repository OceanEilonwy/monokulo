//! `cargo xtask serve`: a built site, served on localhost to look at.

use crate::support::{unquote, Exit};
use std::{
    fs,
    io::{self, BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::{Component, Path, PathBuf},
    thread,
    time::Duration,
};

/// A client that sends nothing is dropped after this.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Serves a built report from `dir` until stopped: GET and HEAD only, files
/// only, nothing outside `dir`.
pub(crate) fn serve(args: &[&str]) -> io::Result<Exit> {
    let (dir, port) = match args {
        [dir] => (PathBuf::from(dir), "8000"),
        [dir, port] => (PathBuf::from(dir), *port),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "usage: cargo xtask serve DIR [PORT]",
            ))
        }
    };
    let listener = TcpListener::bind(format!("127.0.0.1:{port}"))?;
    eprintln!(
        "serving {} on http://127.0.0.1:{port}/ (Ctrl+C to stop)",
        dir.display()
    );
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let dir = dir.clone();
        // A thread per request: a browser opens several connections at once,
        // and one that stalls must not hold the others up.
        thread::spawn(move || {
            if let Err(e) = respond(&dir, stream) {
                eprintln!("serve: {e}");
            }
        });
    }
    Ok(Exit::SUCCESS)
}

/// The file a request target names inside `dir`, or nothing when it names
/// anything else: only plain names below `dir`, so no `..`, no absolute
/// paths and no drive prefixes, however they are encoded.
fn file_for(dir: &Path, target: &str) -> Option<PathBuf> {
    let path = unquote(target.split(['?', '#']).next().unwrap_or(""));
    let mut file = dir.to_path_buf();
    for part in path.split('/').filter(|p| !p.is_empty() && *p != ".") {
        let mut components = Path::new(part).components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(name)), None) => file.push(name),
            _ => return None,
        }
    }
    if file.is_dir() {
        file.push("index.html");
    }
    file.is_file().then_some(file)
}

fn content_type(file: &Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css",
        "js" => "text/javascript",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn respond(dir: &Path, mut stream: TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    let mut request = String::new();
    BufReader::new(&stream).read_line(&mut request)?;
    let mut words = request.split_whitespace();
    let method = words.next().unwrap_or("");
    let target = words.next().unwrap_or("/");
    let (status, body, kind, allow) = match method {
        "GET" | "HEAD" => {
            match file_for(dir, target).and_then(|f| fs::read(&f).ok().map(|b| (f, b))) {
                Some((file, body)) => ("200 OK", body, content_type(&file), ""),
                None => ("404 Not Found", b"not found".to_vec(), "text/plain", ""),
            }
        }
        _ => (
            "405 Method Not Allowed",
            b"method not allowed".to_vec(),
            "text/plain",
            "Allow: GET, HEAD\r\n",
        ),
    };
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n{allow}Connection: close\r\n\r\n", body.len())?;
    if method == "HEAD" {
        return Ok(());
    }
    stream.write_all(&body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::Scratch;
    use std::io::Read;

    fn put(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn serve_answers_files_and_refuses_paths_outside_its_folder() {
        let scratch = Scratch::new("quality-serve");
        put(&scratch.join("site/index.html"), "<p>report</p>");
        put(&scratch.join("site/a b.json"), "{}");
        put(&scratch.join("secret.txt"), "outside");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let site = scratch.join("site");
        let requests = 8;
        let server = thread::spawn(move || {
            for stream in listener.incoming().take(requests) {
                respond(&site, stream.unwrap()).unwrap();
            }
        });
        let send = |line: &str| {
            let mut stream = TcpStream::connect(addr).unwrap();
            write!(stream, "{line} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
            let mut reply = String::new();
            stream.read_to_string(&mut reply).unwrap();
            reply
        };
        let page = send("GET /");
        assert!(
            page.starts_with("HTTP/1.1 200")
                && page.contains("text/html")
                && page.ends_with("<p>report</p>"),
            "{page}"
        );
        assert!(send("GET /a%20b.json").starts_with("HTTP/1.1 200"));
        assert!(send("GET /missing.json").starts_with("HTTP/1.1 404"));
        for outside in ["/../secret.txt", "/%2e%2e/secret.txt", "/..%2fsecret.txt"] {
            assert!(
                send(&format!("GET {outside}")).starts_with("HTTP/1.1 404"),
                "{outside}"
            );
        }
        let head = send("HEAD /");
        assert!(
            head.starts_with("HTTP/1.1 200") && head.ends_with("\r\n\r\n"),
            "{head}"
        );
        let post = send("POST /");
        assert!(
            post.starts_with("HTTP/1.1 405") && post.contains("Allow: GET, HEAD"),
            "{post}"
        );
        server.join().unwrap();
    }
}
