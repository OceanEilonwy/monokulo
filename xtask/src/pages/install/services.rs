//! Run the push relay and the Bell: the two services beside Monokulo, each
//! its own program, on an OpenWrt router (the packages in the same apk
//! repository as monokulo's, each with its LuCI page) or on a computer (the
//! release zips, with a service file for systemd, launchd and Windows),
//! and the TLS proxy both need in front. Both LuCI pages and the two
//! READMEs link here; what each service does and its API are its own page
//! (`/push-relay/`, `/bell/`).

use super::{command, path, steps, Site};
use maud::{html, Markup};

pub(in crate::pages) const TITLE: &str = "Run the push relay and the Bell";

pub(in crate::pages) const LEAD: &str =
    "Install either one on your OpenWrt router or a computer, give it its https address, and put a TLS proxy in front.";

/// The Caddyfile that serves a service over https on the machine it runs
/// on: Caddy gets the certificate itself.
fn caddyfile(host: &str, port: u16) -> String {
    format!("{host} {{\n    reverse_proxy 127.0.0.1:{port}\n}}")
}

/// The same for nginx, with the certificate files certbot made.
fn nginx(host: &str, port: u16) -> String {
    format!(
        "server {{\n    listen 443 ssl;\n    server_name {host};\n    ssl_certificate     /etc/letsencrypt/live/{host}/fullchain.pem;\n    ssl_certificate_key /etc/letsencrypt/live/{host}/privkey.pem;\n    location / {{\n        proxy_pass http://127.0.0.1:{port};\n        proxy_set_header Host $host;\n        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;\n    }}\n}}"
    )
}

/// The page under its title and lead: which service is which, what each
/// needs, the steps on a router and on a computer, the TLS proxy, how to
/// see what it is doing, and updating and removing it.
pub(in crate::pages) fn render(site: &Site) -> Markup {
    let (repos, site_url, github_url) = (&site.repos, &site.site_url, &site.github_url);
    let newest = &repos[0];
    let router = steps(vec![
        (
            "Trust the key and add the repository",
            html! {
                p {
                    "The same repository as Monokulo’s: steps 2 and 3 of "
                    a href="../install-router/#install-it" { "Install on a router" }
                    ". If Monokulo is installed from it already, skip this."
                }
                (command(format!("wget -O /etc/apk/keys/monokulo.pem {site_url}/openwrt/monokulo.pem\necho \"{site_url}/{}/packages.adb\" >> /etc/apk/repositories.d/customfeeds.list", newest.path), "Copy the key and repository commands"))
                p.muted.small { "That line is for OpenWrt " (newest.release) " on " (newest.arch) "; the router page has every version." }
            },
        ),
        (
            "Install the service",
            html! {
                p.path { (path(&["System", "Software"])) }
                p {
                    "Click "
                    strong { "Update lists…" }
                    ", type "
                    code { "monokulo" }
                    " into the filter, open "
                    em { "Available" }
                    " and click "
                    strong { "Install…" }
                    " next to "
                    strong { "monokulo-push-relay" }
                    " or "
                    strong { "monokulo-bell" }
                    ". Each is one package: the program, its LuCI page and the setting that keeps its data across firmware upgrades. Or over SSH:"
                }
                (command("apk update\napk add monokulo-push-relay   # the push relay\napk add monokulo-bell         # the Monokular Bell", "Copy the install commands"))
                p.muted.small {
                    "On install each makes the one secret it can make for you, root’s alone: the relay’s handle key in "
                    code { "/etc/monokulo-push-relay/secrets/MONOKULO_RELAY_HANDLE_KEY" }
                    ", the Bell’s VAPID key in "
                    code { "/etc/monokulo-bell/secrets/MONOKULO_BELL_VAPID_KEY" }
                    ". Back it up: the endpoints and subscriptions already handed out are made under it."
                }
            },
        ),
        (
            "Set it up on its page",
            html! {
                p.path { (path(&["Services", "Push relay"])) " or " (path(&["Services", "Bell"])) }
                p {
                    "Reload LuCI so the menu item appears. The page says whether it runs and, if not, why, in the program’s own words. Set the "
                    strong { "Public address" }
                    " (the https address apps, points of sale and tills will reach it at), then what it serves: the relay’s "
                    em { "Apps" }
                    " (a name, its iOS bundle ID for APNs, its Firebase project ID for FCM) with the APNs key uploaded under "
                    em { "Apple" }
                    " and the service account under "
                    em { "Google" }
                    "; the Bell’s "
                    em { "Square" }
                    " and "
                    em { "Clover" }
                    " cards, one per environment (production and North America open, the other regions and the sandboxes folded away), each application with its key or Auth Code set on its card. A secret is shown as set or not set, never its value."
                }
                p {
                    strong { "Save & Apply" }
                    " writes the options file from these settings, checks it with the program itself ("
                    code { "--check" }
                    ") and restarts it only if it passes: a refused setting shows the program’s reason and the service keeps running on what it had."
                }
            },
        ),
        (
            "Put a TLS proxy in front",
            html! {
                p {
                    "Neither program serves https itself. Install Caddy on the router and point it at the service ("
                    a href="#tls-proxy" { "TLS proxy" }
                    " below), then open ports 80 and 443 to the internet for Caddy. With the proxy on the router, leave "
                    strong { "Listen on" }
                    " at "
                    em { "This router only" }
                    " for the Bell and set it so for the relay too, with "
                    code { "127.0.0.1" }
                    " among the relay’s trusted proxies."
                }
                p {
                    "If the proxy is on another machine instead, set "
                    strong { "Listen on" }
                    " to "
                    em { "LAN" }
                    " and name that machine as a trusted proxy. Only when the service itself must be reached from the internet, turn on the firewall rule each package made, off, under "
                    span.path.inline { (path(&["Network", "Firewall", "Traffic Rules"])) }
                    ": "
                    em { "Allow-monokulo-push-relay" }
                    " (TCP 8444) or "
                    em { "Allow-monokulo-bell" }
                    " (TCP 8445)."
                }
            },
        ),
    ]);
    let linux = steps(vec![
        (
            "Install the program and its options file",
            html! {
                p {
                    "From the unzipped folder, as root. Then set the public address and what it serves in the options file ("
                    code { "/etc/monokulo/relay.toml" }
                    " or "
                    code { "/etc/monokulo/bell.toml" }
                    ")."
                }
                (command("install -m 755 monokulo-push-relay /usr/local/bin/\ninstall -d /etc/monokulo\ninstall -m 644 relay.toml /etc/monokulo/relay.toml   # then set server.public_uri and your apps", "Copy the install commands"))
            },
        ),
        (
            "Write the secrets file",
            html! {
                p {
                    "Root’s alone; one "
                    code { "VARIABLE='value'" }
                    " a line, the single quotes keeping a multi-line value (the APNs "
                    code { ".p8" }
                    ", the FCM JSON) whole. The Bell’s is "
                    code { "/etc/monokulo/monokulo-bell.env" }
                    " with "
                    code { "MONOKULO_BELL_VAPID_KEY" }
                    " and each integration’s secret, such as "
                    code { "MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY" }
                    "."
                }
                (command("install -m 600 /dev/null /etc/monokulo/monokulo-push-relay.env\nprintf \"MONOKULO_RELAY_HANDLE_KEY='%s'\\n\" \"$(openssl rand -hex 32)\" >> /etc/monokulo/monokulo-push-relay.env\nprintf \"MONOKULO_RELAY_APNS_KEY='%s'\\n\" \"$(cat AuthKey_FGHIJ67890.p8)\" >> /etc/monokulo/monokulo-push-relay.env", "Copy the secrets commands"))
            },
        ),
        (
            "Install the unit and start it",
            html! {
                p {
                    "The unit runs it as a user systemd makes for it, with its database in "
                    code { "/var/lib/monokulo/push-relay/" }
                    " (the Bell’s in "
                    code { "/var/lib/monokulo/bell/" }
                    "), and runs "
                    code { "--check" }
                    " before every start, so a bad setting stops it with the reason in "
                    code { "journalctl -u monokulo-push-relay" }
                    "."
                }
                (command("install -m 644 service/monokulo-push-relay.service /etc/systemd/system/\nsystemctl daemon-reload\nsystemctl enable --now monokulo-push-relay\nsystemctl status monokulo-push-relay", "Copy the service commands"))
            },
        ),
    ]);
    let macos = steps(vec![
        (
            "Install the program and its options file",
            html! {
                p {
                    "The binary is unsigned: clear its quarantine once. Then set the public address and what it serves in "
                    code { "relay.toml" }
                    " (or "
                    code { "bell.toml" }
                    ")."
                }
                (command("dir=\"/Library/Application Support/monokulo\"\nsudo mkdir -p /usr/local/bin \"$dir/push-relay\"\nsudo install -m 755 monokulo-push-relay /usr/local/bin/\nsudo xattr -d com.apple.quarantine /usr/local/bin/monokulo-push-relay\nsudo install -m 644 relay.toml \"$dir/relay.toml\"   # then set server.public_uri and your apps\nsudo chown nobody \"$dir/push-relay\" && sudo chmod 700 \"$dir/push-relay\"", "Copy the install commands"))
            },
        ),
        (
            "Install the daemon with its secrets",
            html! {
                p {
                    "The plist holds the secrets, so it is root’s alone (600): put each in place of its "
                    code { "REPLACE" }
                    " and delete the lines of one you don’t use. A plist left with a "
                    code { "REPLACE" }
                    " is refused at start, with the reason, rather than running half set up."
                }
                (command("sudo install -m 600 -o root -g wheel service/com.monokulo.push-relay.plist /Library/LaunchDaemons/\nsudo nano /Library/LaunchDaemons/com.monokulo.push-relay.plist   # the secrets, in place of REPLACE", "Copy the daemon commands"))
            },
        ),
        (
            "Check, then start it",
            html! {
                p {
                    "It runs at boot as "
                    code { "nobody" }
                    ", its log in "
                    code { "$dir/push-relay/push-relay.log" }
                    ". Stop it with "
                    code { "sudo launchctl bootout system/com.monokulo.push-relay" }
                    "."
                }
                (command("sudo -u nobody MONOKULO_RELAY_HANDLE_KEY=\"$(cat handle.key)\" /usr/local/bin/monokulo-push-relay \\\n  --options \"$dir/relay.toml\" --store-path \"$dir/push-relay/push-relay.db\" --check\nsudo launchctl bootstrap system /Library/LaunchDaemons/com.monokulo.push-relay.plist", "Copy the start commands"))
            },
        ),
    ]);
    let windows = steps(vec![
        (
            "Install it as a startup task",
            html! {
                p {
                    "From an administrator PowerShell in the unzipped folder. SmartScreen may ask first ("
                    strong { "More info › Run anyway" }
                    ")."
                }
                (command("powershell -ExecutionPolicy Bypass -File service\\install-windows-task.ps1", "Copy the install command"))
                p {
                    "It copies the program to "
                    code { "C:\\Program Files\\monokulo\\monokulo-push-relay\\" }
                    ", keeps the options file, the secrets, the database and the log in "
                    code { "C:\\ProgramData\\monokulo\\monokulo-push-relay\\" }
                    " (the Bell’s under "
                    code { "monokulo-bell\\" }
                    "), makes the handle key or the VAPID key there if there is none, and registers a task that starts it at boot as "
                    code { "LOCAL SERVICE" }
                    " and again whenever it stops with an error. It is a startup task, not a Windows service: the programs speak no service control protocol."
                }
            },
        ),
        (
            "Set the public address and the secrets, then restart it",
            html! {
                p {
                    "Edit "
                    code { "relay.toml" }
                    " (or "
                    code { "bell.toml" }
                    ") in that ProgramData folder. Each secret is a file in its "
                    code { "secrets\\" }
                    " folder named for its variable ("
                    code { "MONOKULO_RELAY_APNS_KEY" }
                    " is the "
                    code { ".p8" }
                    " file itself, renamed), readable by administrators and "
                    code { "LOCAL SERVICE" }
                    " alone."
                }
                (command("Stop-ScheduledTask monokulo-push-relay; Start-ScheduledTask monokulo-push-relay", "Copy the restart command"))
                p.muted.small {
                    "Run the script again after an upgrade: it keeps the options, secrets and data. "
                    code { "-Uninstall" }
                    " removes the task and the program and keeps the data."
                }
            },
        ),
    ]);
    let tab = |id: &str, label: &str, panel: Markup| ui::Tab {
        id: id.into(),
        label: label.into(),
        panel: html! { h3 data-only="no-script" { (label) } (panel) }.into(),
    };
    html! {
        h2 id="which-one" { "Which one" }
        p {
            "The "
            a href="../push-relay/" { "push relay" }
            " ("
            code { "monokulo-push-relay" }
            ") hands the Web Push that Monokulo and the Bell send to a till app on to Apple’s and Google’s push services, under the app’s own keys, so an iPhone till, or an Android one with only Google’s services, gets its pushes. Monokulo can run the same relay inside itself ("
            code { "relay.enabled" }
            ", on its Server tab); this is for running it on another machine, or without Monokulo."
        }
        p {
            "The "
            a href="../bell/" { "Monokular Bell" }
            " ("
            code { "monokulo-bell" }
            ") turns a Square or Clover payment webhook into a Web Push ding for the Monokular tills at that location, so a till shows a Monero payment the moment it lands. It is its own program, never part of Monokulo."
        }
        h2 id="youll-need" { "You’ll need" }
        div.req {
            span { "An https address for each, with ports 80 and 443 reachable from the internet" }
            span { "The relay: your apps’ APNs key and Firebase service account" }
            span { "The Bell: a Square application, a Clover app, or several, one per environment" }
        }
        p.muted {
            "Each program checks its settings before it listens and stops with the reason if anything is wrong, and answers "
            code { "GET /status" }
            " on this machine only; a page or a service manager can ask it what it is doing without reading its logs. Their settings and secrets, every one, are on the "
            a href="../push-relay/#run-one" { "relay’s" }
            " and the "
            a href="../bell/#run-one" { "Bell’s" }
            " own pages."
        }
        h2 id="on-an-openwrt-router" { "On an OpenWrt router" }
        p {
            "OpenWrt " (newest.release) " with LuCI on a CPU the repository is built for ("
            @for (i, repo) in repos.iter().enumerate() {
                @if i > 0 { ", " }
                code { (repo.arch) }
            }
            "), as "
            a href="../install-router/" { "Install on a router" }
            " says. Monokulo itself needn’t be installed; the standalone relay refuses to start beside Monokulo’s own relay on the same router, and its page says so."
        }
        (router)
        h3 id="on-the-router-from-a-terminal" { "From a terminal" }
        p { "Everything the page does is UCI, the secrets are files, and the init script checks the settings as the page does:" }
        (command("uci set monokulo-push-relay.main.public_uri='https://relay.example.org'\nuci commit monokulo-push-relay\ncat AuthKey_FGHIJ67890.p8 > /etc/monokulo-push-relay/secrets/MONOKULO_RELAY_APNS_KEY\nservice monokulo-push-relay check     # the program's own words, or OK\nservice monokulo-push-relay restart\nlogread -e monokulo-push-relay        # what it said", "Copy the terminal commands"))
        p {
            "Settings are in "
            code { "/etc/config/monokulo-push-relay" }
            " and "
            code { "/etc/config/monokulo-bell" }
            ", secrets in "
            code { "/etc/<name>/secrets/" }
            " (root’s alone), the data in "
            code { "/srv/<name>/" }
            ". A firmware upgrade keeps all three; so does removing the package, which stops the service and removes the program, its page and its firewall rule. Updating is "
            code { "apk upgrade" }
            " after "
            code { "apk update" }
            ", the service restarted with its settings and secrets kept."
        }
        h2 id="on-a-computer" { "On a computer" }
        p {
            "Download the "
            code { "monokulo-push-relay-…" }
            " or "
            code { "monokulo-bell-…" }
            " ZIP for your computer from the "
            a href=(format!("{github_url}/releases/latest")) { "latest release" }
            ": Linux "
            code { "x86_64-unknown-linux-gnu" }
            " and "
            code { "aarch64-unknown-linux-gnu" }
            ", macOS "
            code { "aarch64-apple-darwin" }
            " and "
            code { "x86_64-apple-darwin" }
            ", Windows "
            code { "x86_64-pc-windows-msvc" }
            ", each with a "
            code { ".sha256" }
            " beside it. The ZIP holds the program, its README, a starter options file written by its own "
            code { "--init" }
            " ("
            code { "relay.toml" }
            " or "
            code { "bell.toml" }
            ") and a "
            code { "service/" }
            " folder with a systemd unit, a launchd daemon and a Windows script, each saying how to install it in its first lines. The steps below are the relay’s; the Bell’s are the same with "
            code { "monokulo-bell" }
            ", "
            code { "bell.toml" }
            ", "
            code { "com.monokulo.bell.plist" }
            " and its own secrets."
        }
        p {
            "Try it in a terminal first: "
            code { "--check" }
            " says what it would run on, then "
            code { "OK" }
            ", or why not, and touches nothing."
        }
        (command("(umask 077; openssl rand -hex 32 > handle.key)   # once; keep it\nexport MONOKULO_RELAY_HANDLE_KEY=\"$(cat handle.key)\"\n./monokulo-push-relay --options relay.toml --check\n./monokulo-push-relay --options relay.toml", "Copy the try-out commands"))
        (ui::Tabs::new("Operating system", vec![
            tab("linux", "Linux (systemd)", linux),
            tab("macos", "macOS (launchd)", macos),
            tab("windows", "Windows (startup task)", windows),
        ]))
        h2 id="tls-proxy" { "TLS proxy" }
        p {
            "Both programs serve plain HTTP and need an https address: a reverse proxy in front terminates TLS and renews the certificate, on the same machine or another. The relay listens on "
            code { "0.0.0.0:8444" }
            " by default and the Bell on "
            code { "127.0.0.1:8445" }
            "; the proxy’s address goes in "
            code { "server.trusted_proxies" }
            " (the relay) or "
            code { "bell.trusted_proxies" }
            " (the Bell; one on the same machine is trusted already), so the client addresses the limits count are the real ones."
        }
        h3 id="caddy" { "Caddy" }
        p {
            "One package on OpenWrt ("
            code { "apk add caddy" }
            ") and most systems; it gets the certificate from Let’s Encrypt itself, with ports 80 and 443 open to the internet. "
            code { "/etc/caddy/Caddyfile" }
            ", one block per service:"
        }
        (ui::CopyBlock::new(format!("{}\n\n{}", caddyfile("relay.example.org", 8444), caddyfile("bell.example.org", 8445))).copy_label("Copy the Caddyfile"))
        (command("service caddy enable && service caddy restart    # OpenWrt; systemctl reload caddy elsewhere", "Copy the Caddy commands"))
        h3 id="nginx" { "nginx" }
        p {
            "With a certificate from certbot ("
            code { "certbot certonly --nginx -d relay.example.org" }
            "), one "
            code { "server" }
            " block per service:"
        }
        (ui::CopyBlock::new(nginx("relay.example.org", 8444)).copy_label("Copy the nginx server block"))
        p.muted.small {
            "Behind a proxy on another machine, the Bell must listen on more than loopback: set "
            code { "bell.listen" }
            " (LuCI: Listen on, LAN) and name the proxy in "
            code { "bell.trusted_proxies" }
            "."
        }
        h2 id="see-what-it-is-doing" { "See what it is doing" }
        p {
            "Each answers on its own status listener, on this machine only ("
            code { "status.listen" }
            ": "
            code { "127.0.0.1:8446" }
            " for the relay, "
            code { "127.0.0.1:8447" }
            " for the Bell): where it listens, its public address, the apps or points of sale it serves and whether each is ready, how many handles or registrations it holds, and the last push or ding. The LuCI pages’ Status card is this; a request naming any other host is refused. It never carries a token, an endpoint or a payment."
        }
        (command("curl -s http://127.0.0.1:8446/status   # the relay\ncurl -s http://127.0.0.1:8447/status   # the Bell", "Copy the status commands"))
        p {
            "Logs: "
            code { "logread -e monokulo-push-relay" }
            " on OpenWrt, "
            code { "journalctl -u monokulo-push-relay" }
            " under systemd, the "
            code { ".log" }
            " beside the database under launchd and on Windows. A start the program refused says why there, and "
            code { "--check" }
            " says the same on the command line."
        }
        p {
            "What each setting means, and the API each serves: "
            a href="../push-relay/" { "The push relay" }
            " and "
            a href="../bell/" { "The Monokular Bell" }
            "."
        }
    }
}
