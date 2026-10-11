//! The landing page (GitHub Pages: /): the project's front page, with the
//! install steps for the desktop builds and for OpenWrt routers. Rendered with
//! maud, beside its stylesheet and script in `xtask/src/pages/assets/landing/`.

use super::{distinct, Repo};
use crate::logo;
use maud::{html, Markup, PreEscaped, DOCTYPE};

/// The page, for the apk repositories the site serves. The router
/// instructions that depend on the OpenWrt version are written once per
/// repository; the page's dropdown (or, without JavaScript, the labels)
/// says which is which.
pub(super) fn render(github_url: &str, pages_url: &str, key: &str, repos: &[Repo]) -> Markup {
    let version = repos[0].version();
    let releases = distinct(repos.iter().map(|r| r.release.clone()), " / ");
    let arches = distinct(repos.iter().map(|r| r.arch.clone()), ", ");
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "Monokulo: self-hosted Monero payments" }
                meta name="description" content="Monokulo is a self-hosted Monero payment gateway for Linux, macOS, Windows and OpenWrt routers like the GL.iNet Flint 2. Watch-only: it never holds a spend key.";
                link rel="icon" href="assets/favicon.svg" type="image/svg+xml";
                // Colours come only from monokulo's own theme (crates/monokulo/src/views/theme.css,
                // copied here at build time): light or dark follows the OS.
                link rel="stylesheet" href="assets/theme.css";
                link rel="stylesheet" href="assets/landing.css";
            }
            body {
                header {
                    div class="bar" {
                        a class="brand" href="#top" {
                            svg class="logo-mark" width="30" height="30" viewBox="0 0 64 64" aria-hidden="true" focusable="false" {
                                (PreEscaped(logo::small()))
                            }
                            span { "Monokulo" }
                        }
                        nav aria-label="Sections" {
                            a href="#how" { "How it works" }
                            a href="#install" { "Install" }
                            a href="docs/" { "Docs" }
                            a href="testing/" { "Testing" }
                            a href=(github_url) { "Source code" }
                        }
                    }
                }
                main id="top" {
                    section class="hero" {
                        svg class="mark logo-mark" width="96" height="96" viewBox="0 0 64 64" aria-hidden="true" focusable="false" {
                            (PreEscaped(logo::full()))
                        }
                        div {
                            h1 { "Accept Monero, on your own computer or router" }
                            p class="lead" {
                                "Monokulo is a self-hosted Monero payment gateway. It runs on Linux, macOS, Windows or your OpenWrt router, watches the chain with each store’s view key, and tells the store when an order is paid. It never holds a spend key, so it can never move your coins."
                            }
                            div class="actions" {
                                a class="btn btn-primary" href="#install" { "Install Monokulo" }
                                a class="btn" href=(github_url) { "Source on GitHub" }
                            }
                        }
                    }
                    section id="how" {
                        div class="section-title" {
                            h2 { "How it works" }
                            p {
                                "One program with the engine inside it, a browser dashboard, and a Monero node it can reach."
                            }
                        }
                        div class="grid" {
                            div class="card" {
                                h3 { "Watch-only" }
                                p {
                                    "Each store gives Monokulo its view key, never its spend key. Payments go straight to the store’s own wallet; Monokulo just sees them arrive."
                                }
                            }
                            div class="card" {
                                h3 { "Checkout that works anywhere" }
                                p {
                                    "A hosted checkout page and embed for static sites, a point-of-sale screen, and webhooks when an order is paid. The checkout works without JavaScript."
                                }
                            }
                            div class="card" {
                                h3 { "Fast detection" }
                                p {
                                    "Payments show up within about a second of reaching the network, and are confirmed as blocks arrive."
                                }
                            }
                            div class="card" {
                                h3 { "Easy on the router" }
                                p {
                                    "One static program and SQLite. The engine runs inside Monokulo on threads of its own, kept to two of the Flint 2’s four cores at low priority, so routing comes first. Checking each block’s proof of work (on by default for mainnet) takes about 256 MB of memory; turn it off on the admin page if you run your own node."
                                }
                            }
                            div class="card" {
                                h3 { "Settings in your browser" }
                                p {
                                    "Manage stores, nodes and payment settings on Monokulo’s admin page. On OpenWrt, LuCI also controls start, stop, port, listen address and the engine’s CPUs under Services → Monokulo."
                                }
                            }
                            div class="card" {
                                h3 { "Survives upgrades" }
                                p {
                                    "Its settings, keys and databases are kept across OpenWrt firmware upgrades."
                                }
                            }
                        }
                    }
                    section id="install" {
                        div class="section-title" {
                            h2 { "Install Monokulo" }
                            p {
                                "Choose your computer or router. Every download includes the payment engine."
                            }
                        }
                        div class="actions" {
                            a class="btn btn-primary" href="#desktop" { "Linux, macOS or Windows" }
                            a class="btn" href="#openwrt" { "OpenWrt router" }
                        }
                    }
                    section id="desktop" class="install" {
                        div class="section-title" {
                            h2 { "Install on your desktop" }
                            p {
                                "Download one executable, start it in a terminal, then finish setup in your browser."
                            }
                        }
                        div class="card" {
                            h3 { "Download and extract" }
                            p {
                                "Open the "
                                a href=(format!("{github_url}/releases/latest")) { "latest release" }
                                " and download the "
                                code { "monokulo-…" }
                                " ZIP for your computer. For the newest build from main, use the "
                                a href=(format!("{github_url}/releases/tag/latest-main")) {
                                    "latest-main prerelease"
                                }
                                ". Extract the ZIP, then open a terminal in the extracted folder containing "
                                code { "monokulo" }
                                " (or "
                                code { "monokulo.exe" }
                                ")."
                            }
                            p class="muted small" style="margin-top:var(--space-sm)" {
                                "Each ZIP has a matching "
                                code { ".sha256" }
                                " checksum on the release page. You don’t need Rust, Node.js or a separate engine download."
                            }
                        }
                        div class="card" {
                            div class="tabs" role="tablist" aria-label="Desktop operating system" hidden {
                                button type="button" role="tab" id="tab-linux" aria-controls="panel-linux" aria-selected="true" {
                                    "Linux"
                                }
                                button type="button" role="tab" id="tab-macos" aria-controls="panel-macos" aria-selected="false" tabindex="-1" {
                                    "macOS"
                                }
                                button type="button" role="tab" id="tab-windows" aria-controls="panel-windows" aria-selected="false" tabindex="-1" {
                                    "Windows"
                                }
                            }
                            div class="tabpanel" id="panel-linux" role="tabpanel" aria-labelledby="tab-linux" {
                                h3 class="nojs" { "Linux" }
                                ol {
                                    li {
                                        span class="num" { "1" }
                                        div {
                                            h3 { "Choose the Linux ZIP" }
                                            p {
                                                "For Intel / AMD computers, choose the file ending in "
                                                code { "x86_64-unknown-linux-gnu.zip" }
                                                ". For ARM64, choose "
                                                code { "aarch64-unknown-linux-gnu.zip" }
                                                ". These builds need glibc 2.35 or newer (Ubuntu 22.04 or a comparable distribution)."
                                            }
                                        }
                                    }
                                    li {
                                        span class="num" { "2" }
                                        div {
                                            h3 { "Create your encryption key once" }
                                            p {
                                                "In the extracted folder, generate a private key file with OpenSSL. Keep this file for every restart and update."
                                            }
                                            div class="cmd" {
                                                pre {
                                                    "umask 077\nopenssl rand -hex 32 > monokulo.key\nchmod +x monokulo"
                                                }
                                            }
                                        }
                                    }
                                    li {
                                        span class="num" { "3" }
                                        div {
                                            h3 { "Start Monokulo" }
                                            p {
                                                "Run this each time you start it, from the folder containing your executable and key file."
                                            }
                                            div class="cmd" {
                                                pre {
                                                    "MONOKULO_ENCRYPTION_KEY=\"$(cat monokulo.key)\" ./monokulo"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            div class="tabpanel" id="panel-macos" role="tabpanel" aria-labelledby="tab-macos" {
                                h3 class="nojs" { "macOS" }
                                ol {
                                    li {
                                        span class="num" { "1" }
                                        div {
                                            h3 { "Choose the macOS ZIP" }
                                            p {
                                                "For Apple silicon (M-series), choose the file ending in "
                                                code { "aarch64-apple-darwin.zip" }
                                                ". For an Intel Mac, choose "
                                                code { "x86_64-apple-darwin.zip" }
                                                ". Double-click the ZIP to extract it, then open Terminal in its folder."
                                            }
                                            p class="muted small" {
                                                "The executable is unsigned. If macOS blocks it after download, clear its quarantine attribute once for the binary you downloaded from the release page:"
                                            }
                                            div class="cmd" {
                                                pre { "xattr -d com.apple.quarantine monokulo" }
                                            }
                                        }
                                    }
                                    li {
                                        span class="num" { "2" }
                                        div {
                                            h3 { "Create your encryption key once" }
                                            p {
                                                "Generate a private key file. Keep this file for every restart and update."
                                            }
                                            div class="cmd" {
                                                pre {
                                                    "umask 077\nopenssl rand -hex 32 > monokulo.key\nchmod +x monokulo"
                                                }
                                            }
                                        }
                                    }
                                    li {
                                        span class="num" { "3" }
                                        div {
                                            h3 { "Start Monokulo" }
                                            p {
                                                "Run this each time you start it, from the folder containing your executable and key file."
                                            }
                                            div class="cmd" {
                                                pre {
                                                    "MONOKULO_ENCRYPTION_KEY=\"$(cat monokulo.key)\" ./monokulo"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            div class="tabpanel" id="panel-windows" role="tabpanel" aria-labelledby="tab-windows" {
                                h3 class="nojs" { "Windows" }
                                ol {
                                    li {
                                        span class="num" { "1" }
                                        div {
                                            h3 { "Choose the Windows ZIP" }
                                            p {
                                                "Choose the file ending in "
                                                code { "x86_64-pc-windows-msvc.zip" }
                                                " for an Intel / AMD 64-bit PC. Right-click the ZIP and choose "
                                                strong { "Extract All…" }
                                                ", then open PowerShell in the folder containing "
                                                code { "monokulo.exe" }
                                                "."
                                            }
                                            p class="muted small" {
                                                "The executable is unsigned, so SmartScreen may show "
                                                strong { "More info › Run anyway" }
                                                ". It needs the Microsoft Visual C++ runtime; if Windows reports a missing "
                                                code { "VCRUNTIME140.dll" }
                                                ", install the x64 Visual C++ Redistributable from Microsoft."
                                            }
                                        }
                                    }
                                    li {
                                        span class="num" { "2" }
                                        div {
                                            h3 { "Create your encryption key once" }
                                            p {
                                                "Run this in PowerShell to save a random 32-byte key as 64 hex characters. Keep the file private and reuse it for every restart and update."
                                            }
                                            div class="cmd" {
                                                pre {
                                                    "$keyBytes = New-Object byte[] 32\n$rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()\n$rng.GetBytes($keyBytes)\n$rng.Dispose()\n([BitConverter]::ToString($keyBytes) -replace '-', '').ToLowerInvariant() | Set-Content -Encoding ascii monokulo.key"
                                                }
                                            }
                                        }
                                    }
                                    li {
                                        span class="num" { "3" }
                                        div {
                                            h3 { "Start Monokulo" }
                                            p {
                                                "Run this each time you start it, from the folder containing your executable and key file."
                                            }
                                            div class="cmd" {
                                                pre {
                                                    "$env:MONOKULO_ENCRYPTION_KEY = (Get-Content -Raw .\\monokulo.key).Trim()\n.\\monokulo.exe"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        div class="card" {
                            h3 { "Finish setup in your browser" }
                            p {
                                "Leave the terminal running and open "
                                a href="http://127.0.0.1:8081/" target="_blank" rel="noreferrer" {
                                    code { "http://127.0.0.1:8081" }
                                }
                                ". The first-run wizard creates your admin account. On the admin page, add a Monero node for the network you’ll accept payments on, then set up a wallet (make a new one in your browser, or bring your own view key) and add a store that takes payments into it. You can use your own reachable node or a remote node over HTTPS."
                            }
                            p style="margin-top:var(--space-md)" {
                                "Monokulo listens on this computer only by default. To serve customers over the internet, configure its listen address in Server settings, put it behind an HTTPS reverse proxy, and set the public URL customers will use. For a Tor onion service, see "
                                a href=(format!("{github_url}/blob/main/docs/TOR.md")) { "the Tor guide" }
                                "."
                            }
                            p style="margin-top:var(--space-md)" {
                                "Press "
                                strong { "Ctrl+C" }
                                " to stop it. For automatic startup, run it under your system’s service manager with the same encryption key. For data paths, options and building from source, see "
                                a href=(format!("{github_url}/blob/main/docs/RUNNING.md#from-release-binaries-or-source")) {
                                    "Running Monokulo"
                                }
                                "."
                            }
                        }
                        div class="card" {
                            h3 { "Where your config and data live" }
                            p {
                                "Monokulo’s admin page saves settings in "
                                code { "monokulo.toml" }
                                ". The default locations are:"
                            }
                            table class="config-paths" {
                                thead {
                                    tr {
                                        th scope="col" { "System" }
                                        th scope="col" { "Config file" }
                                        th scope="col" { "Data folder" }
                                    }
                                }
                                tbody {
                                    tr {
                                        th scope="row" { "Linux / macOS" }
                                        td { code { "~/.config/monokulo/monokulo.toml" } }
                                        td { code { "~/.local/share/monokulo/" } }
                                    }
                                    tr {
                                        th scope="row" { "Windows" }
                                        td { code { "%APPDATA%\\monokulo\\monokulo.toml" } }
                                        td { code { "%LOCALAPPDATA%\\monokulo\\" } }
                                    }
                                }
                            }
                            p class="muted small" style="margin-top:var(--space-sm)" {
                                "On Linux and macOS, "
                                code { "~" }
                                " means your home folder. On Windows, paste "
                                code { "%APPDATA%\\monokulo" }
                                " into File Explorer’s address bar to find the config folder."
                            }
                            p style="margin-top:var(--space-md)" {
                                "If set to absolute paths, "
                                code { "XDG_CONFIG_HOME" }
                                " and "
                                code { "XDG_DATA_HOME" }
                                " replace the base folders above. A Windows environment that sets "
                                code { "HOME" }
                                " uses "
                                code { "$HOME/.config" }
                                " and "
                                code { "$HOME/.local/share" }
                                " before the Windows folders. If a default folder cannot be used, Monokulo falls back to its working directory."
                            }
                            p style="margin-top:var(--space-md)" {
                                "To choose a config file explicitly, start with "
                                code { "--options /path/to/monokulo.toml" }
                                ". Running "
                                code { "./monokulo --init" }
                                " (PowerShell: "
                                code { ".\\monokulo.exe --init" }
                                ") writes a template with documented defaults and prints its location. The data folder contains "
                                code { "monokulo.db" }
                                " and "
                                code { "engine.db" }
                                "; "
                                code { "--database-path" }
                                " overrides the main database path. Your encryption key stays in the separate "
                                code { "monokulo.key" }
                                " file you created, rather than in the config."
                            }
                        }
                        div class="card" {
                            h3 { "Backups and updates" }
                            p {
                                "Back up "
                                code { "monokulo.key" }
                                ", your options file and both databases ("
                                code { "monokulo.db" }
                                " and "
                                code { "engine.db" }
                                "). Stop Monokulo before copying the databases. To update, stop it, extract the new ZIP, then start the new executable with your existing key and data."
                            }
                            p class="note" style="margin-top:var(--space-md)" {
                                "Never generate a new key for an existing installation. Losing the key makes encrypted data unreadable. Keep it private: anyone with the key and database can read your stores’ details."
                            }
                        }
                    }
                    section id="openwrt" class="install" {
                        div class="section-title" {
                            h2 { "Install on an OpenWrt router" }
                            p { "About ten minutes. You can do it all from the router’s web interface." }
                        }
                        div class="card" {
                            h3 { "You’ll need" }
                            div class="req" {
                                span { "OpenWrt " (releases) " with LuCI" }
                                span { "A CPU we build for: " (arches) }
                                span { "The router’s root password" }
                                span { "A Monero node it can reach" }
                            }
                            p class="muted" style="margin-top:var(--space-md)" {
                                "Built and tested on the GL.iNet Flint 2 (GL-MT6000). This is for stock OpenWrt, not a vendor’s own firmware. Any router running one of these OpenWrt releases on one of these CPUs works the same way. To check yours, run "
                                code { "cat /etc/openwrt_release" }
                                " and look at "
                                code { "DISTRIB_RELEASE" }
                                " and "
                                code { "DISTRIB_ARCH" }
                                "."
                            }
                            div class="target-pick" hidden {
                                label for="openwrt-target" { "Your OpenWrt version" }
                                select id="openwrt-target" {
                                    @for r in repos {
                                        option value=(r.id()) { "OpenWrt " (r.release) " · " (r.arch) }
                                    }
                                }
                            }
                            p class="muted" style="margin-top:var(--space-sm)" {
                                "The router can’t hold the Monero blockchain itself. Point Monokulo at a node on your network (a NAS or a desktop running "
                                code { "monerod" }
                                "), or at a remote node over HTTPS."
                            }
                        }
                        div class="card" {
                            div class="tabs" role="tablist" aria-label="How to install" hidden {
                                button type="button" role="tab" id="tab-web" aria-controls="panel-web" aria-selected="true" {
                                    "Web (LuCI)"
                                }
                                button type="button" role="tab" id="tab-ssh" aria-controls="panel-ssh" aria-selected="false" tabindex="-1" {
                                    "Terminal (SSH)"
                                }
                            }
                            div class="tabpanel" id="panel-web" role="tabpanel" aria-labelledby="tab-web" {
                                h3 class="nojs" { "In the web interface (LuCI)" }
                                ol {
                                    li {
                                        span class="num" { "1" }
                                        div {
                                            h3 { "Log in to LuCI" }
                                            p {
                                                "Open "
                                                a href="http://192.168.1.1/" target="_blank" rel="noreferrer" {
                                                    code { "http://192.168.1.1" }
                                                }
                                                " (your router’s address, if you changed it) and log in as "
                                                strong { "root" }
                                                "."
                                            }
                                        }
                                    }
                                    li {
                                        span class="num" { "2" }
                                        div {
                                            h3 { "Trust Monokulo’s signing key" }
                                            p class="path" {
                                                "System "
                                                b { "›" }
                                                " Administration "
                                                b { "›" }
                                                em { "Repo Public Keys" }
                                                " tab"
                                            }
                                            p {
                                                "Copy the key below into the box that says "
                                                em { "“Paste content of a file, or a URL to a key file…”" }
                                                ", then click "
                                                strong { "Add key" }
                                                "."
                                            }
                                            div class="cmd" { pre { (key) } }
                                            p class="muted small" {
                                                "Or paste this address instead of the key: "
                                                code { (format!("{pages_url}/openwrt/monokulo.pem")) }
                                                ". It’s the same key that signs Kringle, so if you already trust that, skip this step."
                                            }
                                        }
                                    }
                                    li {
                                        span class="num" { "3" }
                                        div {
                                            h3 { "Add the Monokulo repository" }
                                            p class="path" {
                                                "System "
                                                b { "›" }
                                                " Software "
                                                b { "›" }
                                                em { "Configure apk" }
                                                " button"
                                            }
                                            p {
                                                "Find the box for "
                                                code { "/etc/apk/repositories.d/customfeeds.list" }
                                                ". Add this line at the end, on a line of its own, then click "
                                                strong { "Save" }
                                                "."
                                            }
                                            @for r in repos {
                                                div class="target" data-target=(r.id()) {
                                                    p class="target-label muted small" { "OpenWrt " (r.release) " · " (r.arch) }
                                                    div class="cmd" {
                                                        pre { (format!("{pages_url}/{}/packages.adb", r.path)) }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    li {
                                        span class="num" { "4" }
                                        div {
                                            h3 { "Install Monokulo" }
                                            p class="path" {
                                                "System "
                                                b { "›" }
                                                " Software"
                                            }
                                            p {
                                                "Click "
                                                strong { "Update lists…" }
                                                " and wait for it to finish. Type "
                                                code { "monokulo" }
                                                " into the "
                                                em { "Type to filter…" }
                                                " box, open the "
                                                em { "Available" }
                                                " tab, and click "
                                                strong { "Install…" }
                                                " next to "
                                                strong { "monokulo" }
                                                ". Then confirm with "
                                                strong { "Install" }
                                                ". It’s one package with the program and the LuCI page, and it starts straight away."
                                            }
                                        }
                                    }
                                    li {
                                        span class="num" { "5" }
                                        div {
                                            h3 { "Open Monokulo" }
                                            p class="path" {
                                                "Services "
                                                b { "›" }
                                                " Monokulo"
                                            }
                                            p {
                                                "Reload the page first so the new menu item appears. The status should say "
                                                em { "Running" }
                                                ". Click "
                                                strong { "Open monokulo" }
                                                " (it’s at "
                                                code { "http://192.168.1.1:8081" }
                                                ") and create the admin account: the first account made is the admin."
                                            }
                                        }
                                    }
                                    li {
                                        span class="num" { "6" }
                                        div {
                                            h3 { "Add a Monero node, then a store" }
                                            p {
                                                "On Monokulo’s admin page, add your node under the network you’ll take payments on, then set up a wallet and add a store on it."
                                            }
                                        }
                                    }
                                }
                            }
                            div class="tabpanel" id="panel-ssh" role="tabpanel" aria-labelledby="tab-ssh" {
                                h3 class="nojs" { "In a terminal (SSH)" }
                                ol {
                                    li {
                                        span class="num" { "1" }
                                        div {
                                            h3 { "Connect to the router" }
                                            p {
                                                "From a computer on your network, log in as root. Stock OpenWrt is at "
                                                code { "192.168.1.1" }
                                                " unless you changed it."
                                            }
                                            div class="cmd" { pre { "ssh root@192.168.1.1" } }
                                        }
                                    }
                                    li {
                                        span class="num" { "2" }
                                        div {
                                            h3 { "Trust the key and add the repository" }
                                            p { "You only do this once." }
                                            @for r in repos {
                                                div class="target" data-target=(r.id()) {
                                                    p class="target-label muted small" { "OpenWrt " (r.release) " · " (r.arch) }
                                                    div class="cmd" {
                                                        pre {
                                                            (format!("wget -O /etc/apk/keys/monokulo.pem {pages_url}/openwrt/monokulo.pem\necho \"{pages_url}/{}/packages.adb\" >> /etc/apk/repositories.d/customfeeds.list", r.path))
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    li {
                                        span class="num" { "3" }
                                        div {
                                            h3 { "Install Monokulo" }
                                            p {
                                                "One package: the program and the LuCI page. It starts straight away."
                                            }
                                            div class="cmd" { pre { "apk update\napk add monokulo" } }
                                        }
                                    }
                                    li {
                                        span class="num" { "4" }
                                        div {
                                            h3 { "Open Monokulo" }
                                            p {
                                                "Go to "
                                                code { "http://192.168.1.1:8081" }
                                                " and create the admin account, then add a Monero node and a store. Settings for how it starts are in LuCI under "
                                                span class="path inline" {
                                                    "Services "
                                                    b { "›" }
                                                    " Monokulo"
                                                }
                                                ". If it isn’t running:"
                                            }
                                            div class="cmd" {
                                                pre {
                                                    "service monokulo enable\nservice monokulo start\nlogread -e monokulo   # what it said"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        div class="card" {
                            h3 { "Config, data and secrets on OpenWrt" }
                            p style="margin-bottom:var(--space-md)" {
                                "LuCI’s service settings live in "
                                code { "/etc/config/monokulo" }
                                ". Monokulo’s admin settings are saved to "
                                code { "/srv/monokulo/monokulo.toml" }
                                " by default, alongside "
                                code { "monokulo.db" }
                                " and "
                                code { "engine.db" }
                                ". Changing the data folder in LuCI moves the location used for the config and databases."
                            }
                            p {
                                "On first start Monokulo makes an encryption key for its data, in "
                                code { "/etc/monokulo/secrets" }
                                ". Firmware upgrades keep the file, but if it’s ever lost, Monokulo’s data can’t be read. Copy it somewhere safe from your computer:"
                            }
                            div class="cmd" style="margin-top:var(--space-sm)" {
                                pre { "scp root@192.168.1.1:/etc/monokulo/secrets monokulo-secrets" }
                            }
                            p class="note" style="margin-top:var(--space-md)" {
                                "Keep that file private: with it and a copy of the data folder, anyone can read your stores’ details."
                            }
                        }
                        div class="card" {
                            h3 { "Optional: let customers outside your home pay" }
                            p {
                                "By default Monokulo listens on your LAN only. For checkouts on the internet, it needs to listen on every interface, and the firewall needs to let its port in."
                            }
                            p class="path" style="margin-top:var(--space-md)" {
                                "Services "
                                b { "›" }
                                " Monokulo"
                            }
                            p {
                                "Set "
                                strong { "Listen on" }
                                " to "
                                em { "All interfaces" }
                                ", then "
                                strong { "Save & Apply" }
                                "."
                            }
                            p class="path" style="margin-top:var(--space-md)" {
                                "Network "
                                b { "›" }
                                " Firewall "
                                b { "›" }
                                em { "Traffic Rules" }
                                " tab "
                                b { "›" }
                                " Add"
                            }
                            p {
                                "Name "
                                em { "Monokulo" }
                                ", Protocol "
                                em { "TCP" }
                                ", Source zone "
                                em { "wan" }
                                ", Destination zone "
                                em { "Device (input)" }
                                ", Destination port "
                                em { "8081" }
                                ", Action "
                                em { "accept" }
                                ". Click "
                                strong { "Save" }
                                ", then "
                                strong { "Save & Apply" }
                                "."
                            }
                            p class="note info" style="margin-top:var(--space-md)" {
                                strong { "Use Traffic Rules, not Port Forwards." }
                                " Monokulo runs on the router itself, so there’s nothing to forward. Only if your ISP’s modem sits in front of the router do you forward TCP 8081 on "
                                em { "that" }
                                " device."
                            }
                            p style="margin-top:var(--space-md)" {
                                "Then set the "
                                strong { "public URL" }
                                " on Monokulo’s admin page (Settings) to the address customers use, for example "
                                code { "https://pay.example.com" }
                                ". Behind CGNAT, or to keep your address private, serve it as a Tor onion service instead: see "
                                a href=(format!("{github_url}/blob/main/docs/TOR.md")) { "docs/TOR.md" }
                                " and set "
                                em { "Listen on" }
                                " to "
                                em { "This router only" }
                                "."
                            }
                            p style="margin-top:var(--space-md)" {
                                "Prefer a terminal? This does the same:"
                            }
                            div class="cmd" style="margin-top:var(--space-sm)" {
                                pre {
                                    "uci set monokulo.main.listen=all\nuci commit monokulo\nuci add firewall rule\nuci set firewall.@rule[-1].name=Monokulo\nuci set firewall.@rule[-1].src=wan\nuci set firewall.@rule[-1].proto=tcp\nuci set firewall.@rule[-1].dest_port=8081\nuci set firewall.@rule[-1].target=ACCEPT\nuci commit firewall\nservice firewall reload\nservice monokulo restart"
                                }
                            }
                        }
                        div class="grid two" {
                            div class="card" {
                                h3 { "Updating" }
                                p {
                                    span class="path inline" {
                                        "System "
                                        b { "›" }
                                        " Software"
                                    }
                                    ": click "
                                    strong { "Update lists…" }
                                    ", then look in the "
                                    em { "Updates" }
                                    " tab. Or in a terminal:"
                                }
                                div class="cmd" style="margin-top:var(--space-sm)" {
                                    pre { "apk update && apk upgrade monokulo" }
                                }
                                p class="muted small" style="margin-top:var(--space-sm)" {
                                    "Settings, secrets and databases are kept. After a firmware upgrade, repeat the install steps to put the package back; your data is still there."
                                }
                            }
                            div class="card" {
                                h3 { "Removing" }
                                p {
                                    span class="path inline" {
                                        "System "
                                        b { "›" }
                                        " Software"
                                    }
                                    ": in the "
                                    em { "Installed" }
                                    " tab, click "
                                    strong { "Remove…" }
                                    " on monokulo. Or in a terminal:"
                                }
                                div class="cmd" style="margin-top:var(--space-sm)" {
                                    pre {
                                        "apk del monokulo\nrm -rf /srv/monokulo /etc/monokulo   # also delete all data and secrets"
                                    }
                                }
                            }
                        }
                        div class="card" {
                            h3 { "Package files" }
                            p class="muted" {
                                "You don’t need these if you use the repository above. The package is signed with the Monokulo key, so once it’s trusted (step 2) you can also install it directly: upload it in "
                                span class="path inline" {
                                    "System "
                                    b { "›" }
                                    " Software "
                                    b { "›" }
                                    " Upload Package…"
                                }
                                ", or use "
                                code { "apk add" }
                                "."
                            }
                            @for r in repos {
                                div class="target" data-target=(r.id()) {
                                    p class="target-label muted small" { "OpenWrt " (r.release) " · " (r.arch) }
                                    ul class="files" {
                                        li {
                                            a href=(format!("{}/{}", r.path, r.apk)) { (r.apk) }
                                            " (" (r.size()) "): monokulo (the engine inside it) and the LuCI page"
                                        }
                                        li {
                                            a href=(format!("{}/packages.adb", r.path)) { "packages.adb" }
                                            ": the signed repository index"
                                        }
                                    }
                                }
                            }
                            ul class="files" {
                                li {
                                    a href="openwrt/monokulo.pem" { "monokulo.pem" }
                                    ": the signing key, the same for every version"
                                }
                            }
                        }
                    }
                }
                footer {
                    (format!("Monokulo {version} · "))
                    a href=(github_url) { "GitHub" }
                    " · Typeset in Manrope."
                }
                script src="assets/landing.js" defer {}
            }
        }
    }
}
