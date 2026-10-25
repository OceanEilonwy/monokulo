//! Install on a router: the OpenWrt package, from LuCI or a terminal, then
//! opening it to the internet and the package files themselves.
//!
//! The steps that depend on the OpenWrt version are written once per
//! repository the site holds, each under its label. The site's script
//! (`assets/site/site.js`) shows the version picker and only the chosen
//! version's blocks; without it, every version shows under its own label.

use super::{command, distinct, path, steps, Site};
use maud::{html, Markup};

pub(in crate::pages) const TITLE: &str = "Install on an OpenWrt router";

pub(in crate::pages) const LEAD: &str =
    "About ten minutes. You can do it all from the router’s web interface.";

/// The page under its title and lead: what the router needs and the
/// version picker, the steps in LuCI and over SSH (the repository lines
/// once per apk repository the site holds), opening it to customers on the
/// internet, and each repository's package files.
pub(in crate::pages) fn render(site: &Site) -> Markup {
    let (repos, site_url) = (&site.repos, &site.site_url);
    let releases = distinct(repos.iter().map(|r| r.release.clone()), " / ");
    let arches = distinct(repos.iter().map(|r| r.arch.clone()), ", ");
    // One block per repository, each under its label.
    let per_repo = |block: &dyn Fn(&super::Repo) -> Markup| {
        html! {
            @for r in repos {
                div.target data-target=(r.id()) {
                    p.target-label.muted.small { "OpenWrt " (r.release) " · " (r.arch) }
                    (block(r))
                }
            }
        }
    };
    let web = steps(vec![
        (
            "Log in to LuCI",
            html! {
                p {
                    "Open "
                    a href="http://192.168.1.1/" target="_blank" rel="noreferrer" { code { "http://192.168.1.1" } }
                    " (your router’s address, if you changed it) and log in as "
                    strong { "root" }
                    "."
                }
            },
        ),
        (
            "Trust Monokulo’s signing key",
            html! {
                p.path { (path(&["System", "Administration"])) " " b { "›" } " " em { "Repo Public Keys" } " tab" }
                p {
                    "Copy the key below into the box that says "
                    em { "“Paste content of a file, or a URL to a key file…”" }
                    ", then click "
                    strong { "Add key" }
                    "."
                }
                (ui::CopyBlock::new(site.key.as_str()).copy_label("Copy the signing key"))
                p.muted.small {
                    "Or paste this address instead of the key: "
                    code { (format!("{site_url}/openwrt/monokulo.pem")) }
                    ". It’s the same key that signs Kringle, so if you already trust that, skip this step."
                }
            },
        ),
        (
            "Add the Monokulo repository",
            html! {
                p.path { (path(&["System", "Software"])) " " b { "›" } " " em { "Configure apk" } " button" }
                p {
                    "Find the box for "
                    code { "/etc/apk/repositories.d/customfeeds.list" }
                    ". Add this line at the end, on a line of its own, then click "
                    strong { "Save" }
                    "."
                }
                (per_repo(&|r| html! {
                    (command(format!("{site_url}/{}/packages.adb", r.path), "Copy the repository line"))
                }))
            },
        ),
        (
            "Install Monokulo",
            html! {
                p.path { (path(&["System", "Software"])) }
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
            },
        ),
        (
            "Open Monokulo",
            html! {
                p.path { (path(&["Services", "Monokulo"])) }
                p {
                    "Reload the page first so the new menu item appears. The status should say "
                    em { "Running" }
                    ". Click "
                    strong { "Open monokulo" }
                    " (it’s at "
                    code { "http://192.168.1.1:8081" }
                    ") and create the admin account: the first account made is the admin."
                }
            },
        ),
        (
            "Add a Monero node, then a store",
            html! {
                p { "On Monokulo’s admin page, add your node under the network you’ll take payments on, then set up a wallet and add a store on it." }
            },
        ),
    ]);
    let ssh = steps(vec![
        (
            "Connect to the router",
            html! {
                p {
                    "From a computer on your network, log in as root. Stock OpenWrt is at "
                    code { "192.168.1.1" }
                    " unless you changed it."
                }
                (command("ssh root@192.168.1.1", "Copy the ssh command"))
            },
        ),
        (
            "Trust the key and add the repository",
            html! {
                p { "You only do this once." }
                (per_repo(&|r| html! {
                    (command(format!("wget -O /etc/apk/keys/monokulo.pem {site_url}/openwrt/monokulo.pem\necho \"{site_url}/{}/packages.adb\" >> /etc/apk/repositories.d/customfeeds.list", r.path), "Copy the key and repository commands"))
                }))
            },
        ),
        (
            "Install Monokulo",
            html! {
                p { "One package: the program and the LuCI page. It starts straight away." }
                (command("apk update\napk add monokulo", "Copy the install commands"))
            },
        ),
        (
            "Open Monokulo",
            html! {
                p {
                    "Go to "
                    code { "http://192.168.1.1:8081" }
                    " and create the admin account, then add a Monero node and a store. Settings for how it starts are in LuCI under "
                    span.path.inline { (path(&["Services", "Monokulo"])) }
                    ". If it isn’t running:"
                }
                (command("service monokulo enable\nservice monokulo start\nlogread -e monokulo   # what it said", "Copy the service commands"))
            },
        ),
    ]);
    let tab = |id: &str, label: &str, heading: &str, panel: Markup| ui::Tab {
        id: id.into(),
        label: label.into(),
        panel: html! { h3 data-only="no-script" { (heading) } (panel) }.into(),
    };
    html! {
        h2 id="youll-need" { "You’ll need" }
        div.req {
            span { "OpenWrt " (releases) " with LuCI" }
            span { "A CPU we build for: " (arches) }
            span { "The router’s root password" }
            span { "A Monero node it can reach" }
        }
        p.muted {
            "Built and tested on the GL.iNet Flint 2 (GL-MT6000). This is for stock OpenWrt, not a vendor’s own firmware. Any router running one of these OpenWrt releases on one of these CPUs works the same way. To check yours, run "
            code { "cat /etc/openwrt_release" }
            " and look at "
            code { "DISTRIB_RELEASE" }
            " and "
            code { "DISTRIB_ARCH" }
            "."
        }
        div.target-pick hidden {
            label for="openwrt-target" { "Your OpenWrt version" }
            (ui::Select::new("openwrt_target", repos.iter()
                .map(|r| ui::Choice::new(r.id(), format!("OpenWrt {} · {}", r.release, r.arch)))
                .collect::<Vec<_>>())
                .id("openwrt-target")
                .search(ui::SearchMode::Hide))
        }
        p.muted {
            "The router can’t hold the Monero blockchain itself. Point Monokulo at a node on your network (a NAS or a desktop running "
            code { "monerod" }
            "), or at a remote node over HTTPS."
        }
        h2 id="install-it" { "Install it" }
        (ui::Tabs::new("How to install", vec![
            tab("web", "Web (LuCI)", "In the web interface (LuCI)", web),
            tab("ssh", "Terminal (SSH)", "In a terminal (SSH)", ssh),
        ]))
        p {
            "Your settings, data and the key Monokulo makes for them are on "
            a href="../config/" { "Config and data" }
            "; updating and removing the package on "
            a href="../backups/" { "Backups and updates" }
            "."
        }
        h2 id="optional-let-customers-outside-your-home-pay" { "Optional: let customers outside your home pay" }
        p { "By default Monokulo listens on your LAN only. For checkouts on the internet, it needs to listen on every interface, and the firewall needs to let its port in." }
        p.path { (path(&["Services", "Monokulo"])) }
        p {
            "Set "
            strong { "Listen on" }
            " to "
            em { "All interfaces" }
            ", then "
            strong { "Save & Apply" }
            "."
        }
        p.path { (path(&["Network", "Firewall"])) " " b { "›" } " " em { "Traffic Rules" } " tab " b { "›" } " Add" }
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
        (ui::Alert::new(html! {
            strong { "Use Traffic Rules, not Port Forwards." }
            " Monokulo runs on the router itself, so there’s nothing to forward. Only if your ISP’s modem sits in front of the router do you forward TCP 8081 on "
            em { "that" }
            " device."
        }, ui::Tone::Notice).quiet(true))
        p {
            "Then set the "
            strong { "public URI" }
            " on Monokulo’s admin page (Settings) to the address customers use, for example "
            code { "https://pay.example.com" }
            ". Behind CGNAT, or to keep your address private, turn on "
            em { "Onion service" }
            " instead: Tor runs inside Monokulo and customers on Tor reach the checkout with no port opened, even with "
            em { "Listen on" }
            " set to "
            em { "This router only" }
            "."
        }
        p { "Prefer a terminal? This does the same:" }
        (command("uci set monokulo.main.listen=all\nuci commit monokulo\nuci add firewall rule\nuci set firewall.@rule[-1].name=Monokulo\nuci set firewall.@rule[-1].src=wan\nuci set firewall.@rule[-1].proto=tcp\nuci set firewall.@rule[-1].dest_port=8081\nuci set firewall.@rule[-1].target=ACCEPT\nuci commit firewall\nservice firewall reload\nservice monokulo restart", "Copy the firewall commands"))
        h2 id="package-files" { "Package files" }
        p.muted {
            "You don’t need these if you use the repository above. The package is signed with the Monokulo key, so once it’s trusted (step 2) you can also install it directly: upload it in "
            span.path.inline { (path(&["System", "Software", "Upload Package…"])) }
            ", or use "
            code { "apk add" }
            "."
        }
        (per_repo(&|r| html! {
            ul.apk-files {
                li {
                    a href=(format!("../{}/{}", r.path, r.apk)) { (r.apk) }
                    " (" (r.size()) "): monokulo (the engine inside it) and the LuCI page"
                }
                li {
                    a href=(format!("../{}/packages.adb", r.path)) { "packages.adb" }
                    ": the signed repository index"
                }
            }
        }))
        ul.apk-files {
            li {
                a href="../openwrt/monokulo.pem" { "monokulo.pem" }
                ": the signing key, the same for every version"
            }
        }
    }
}
