//! Install on a computer: the desktop builds, from a release's ZIP to the
//! first-run wizard, with a tab per operating system (each its own section
//! without a script).

use super::{command, steps, Site};
use maud::{html, Markup};

pub(in crate::pages) const TITLE: &str = "Install on Linux, macOS or Windows";

pub(in crate::pages) const LEAD: &str =
    "Download one executable, start it in a terminal, then finish setup in your browser.";

/// The key commands for Linux and macOS, which are the same.
const UNIX_KEY: &str = "umask 077\nopenssl rand -hex 32 > monokulo.key\nchmod +x monokulo";

/// The start command for Linux and macOS.
const UNIX_START: &str = "MONOKULO_ENCRYPTION_KEY=\"$(cat monokulo.key)\" ./monokulo";

/// The page under its title and lead: where to download a release (from
/// the given repository), a tab per operating system with its key and
/// start commands, and what to do in the browser once it runs.
pub(in crate::pages) fn render(site: &Site) -> Markup {
    let github_url = &site.github_url;
    let start = || {
        (
            "Start Monokulo",
            html! {
                p { "Run this each time you start it, from the folder containing your executable and key file." }
                (command(UNIX_START, "Copy the start command"))
            },
        )
    };
    let linux = steps(vec![
        (
            "Choose the Linux ZIP",
            html! {
                p {
                    "For Intel / AMD computers, choose the file ending in "
                    code { "x86_64-unknown-linux-gnu.zip" }
                    ". For ARM64, choose "
                    code { "aarch64-unknown-linux-gnu.zip" }
                    ". These builds need glibc 2.35 or newer (Ubuntu 22.04 or a comparable distribution)."
                }
            },
        ),
        (
            "Create your encryption key once",
            html! {
                p { "In the extracted folder, generate a private key file with OpenSSL. Keep this file for every restart and update." }
                (command(UNIX_KEY, "Copy the key commands"))
            },
        ),
        start(),
    ]);
    let macos = steps(vec![
        (
            "Choose the macOS ZIP",
            html! {
                p {
                    "For Apple silicon (M-series), choose the file ending in "
                    code { "aarch64-apple-darwin.zip" }
                    ". For an Intel Mac, choose "
                    code { "x86_64-apple-darwin.zip" }
                    ". Double-click the ZIP to extract it, then open Terminal in its folder."
                }
                p.muted.small {
                    "The executable is unsigned. If macOS blocks it after download, clear its quarantine attribute once for the binary you downloaded from the release page:"
                }
                (command("xattr -d com.apple.quarantine monokulo", "Copy the quarantine command"))
            },
        ),
        (
            "Create your encryption key once",
            html! {
                p { "Generate a private key file. Keep this file for every restart and update." }
                (command(UNIX_KEY, "Copy the key commands"))
            },
        ),
        start(),
    ]);
    let windows = steps(vec![
        (
            "Choose the Windows ZIP",
            html! {
                p {
                    "Choose the file ending in "
                    code { "x86_64-pc-windows-msvc.zip" }
                    " for an Intel / AMD 64-bit PC. Right-click the ZIP and choose "
                    strong { "Extract All…" }
                    ", then open PowerShell in the folder containing "
                    code { "monokulo.exe" }
                    "."
                }
                p.muted.small {
                    "The executable is unsigned, so SmartScreen may show "
                    strong { "More info › Run anyway" }
                    ". It needs the Microsoft Visual C++ runtime; if Windows reports a missing "
                    code { "VCRUNTIME140.dll" }
                    ", install the x64 Visual C++ Redistributable from Microsoft."
                }
            },
        ),
        (
            "Create your encryption key once",
            html! {
                p { "Run this in PowerShell to save a random 32-byte key as 64 hex characters. Keep the file private and reuse it for every restart and update." }
                (command("$keyBytes = New-Object byte[] 32\n$rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()\n$rng.GetBytes($keyBytes)\n$rng.Dispose()\n([BitConverter]::ToString($keyBytes) -replace '-', '').ToLowerInvariant() | Set-Content -Encoding ascii monokulo.key", "Copy the key commands"))
            },
        ),
        (
            "Start Monokulo",
            html! {
                p { "Run this each time you start it, from the folder containing your executable and key file." }
                (command("$env:MONOKULO_ENCRYPTION_KEY = (Get-Content -Raw .\\monokulo.key).Trim()\n.\\monokulo.exe", "Copy the start commands"))
            },
        ),
    ]);
    let tab = |id: &str, label: &str, panel: Markup| ui::Tab {
        id: id.into(),
        label: label.into(),
        panel: html! { h3 data-only="no-script" { (label) } (panel) }.into(),
    };
    html! {
        h2 id="download-and-extract" { "Download and extract" }
        p {
            "Open the "
            a href=(format!("{github_url}/releases/latest")) { "latest release" }
            " and download the "
            code { "monokulo-…" }
            " ZIP for your computer. For the newest build from main, use the "
            a href=(format!("{github_url}/releases/tag/latest-main")) { "latest-main prerelease" }
            ". Extract the ZIP, then open a terminal in the extracted folder containing "
            code { "monokulo" }
            " (or "
            code { "monokulo.exe" }
            ")."
        }
        p.muted.small {
            "Each ZIP has a matching "
            code { ".sha256" }
            " checksum on the release page. You don’t need Rust, Node.js or a separate engine download."
        }
        h2 id="create-a-key-and-start-it" { "Create a key and start it" }
        (ui::Tabs::new("Desktop operating system", vec![
            tab("linux", "Linux", linux),
            tab("macos", "macOS", macos),
            tab("windows", "Windows", windows),
        ]))
        h2 id="finish-setup-in-your-browser" { "Finish setup in your browser" }
        p {
            "Leave the terminal running and open "
            a href="http://127.0.0.1:8081/" target="_blank" rel="noreferrer" { code { "http://127.0.0.1:8081" } }
            ". The first-run wizard creates your admin account. On the admin page, add a Monero node for the network you’ll accept payments on, then set up a wallet (make a new one in your browser, or bring your own view key) and add a store that takes payments into it. You can use your own reachable node or a remote node over HTTPS."
        }
        p {
            "Monokulo listens on this computer only by default. To serve customers over the internet, configure its listen address in Server settings, put it behind an HTTPS reverse proxy, and set the public URI customers will use."
        }
        p {
            "Press "
            strong { "Ctrl+C" }
            " to stop it. For automatic startup, run it under your system’s service manager with the same encryption key. "
            a href="../config/" { "Config and data" }
            " says where its settings and databases are, and "
            a href="../backups/" { "Backups and updates" }
            " what to keep safe."
        }
    }
}
