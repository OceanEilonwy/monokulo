//! The OpenWrt package (`openwrt/monokulo-bell/`, with what it shares with the
//! other service's in `openwrt/common/`) held to the Bell's own settings: its
//! init script writes every setting the options file takes, from a UCI
//! option its LuCI page shows, and names every secret by its variable and
//! no other, as the page's backend (`common/rpcd.uc`) does. A setting or
//! secret added, renamed or removed fails here until the package follows.
//! That the options file it writes starts the Bell is the rootfs test's
//! (`scripts/test-openwrt.sh`).

use std::collections::BTreeSet;
use std::path::Path;

use monokulo_bell::settings;

fn read(path: &str) -> String {
    let openwrt = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../openwrt");
    std::fs::read_to_string(openwrt.join(path)).unwrap()
}

/// A `NAME=value` line of the init script, its quotes dropped.
fn var(init: &str, name: &str) -> String {
    let line = init
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{name}=")))
        .unwrap_or_else(|| panic!("no {name}= in the init script"));
    line.trim_matches('"').to_owned()
}

/// The `<setting key> <UCI option> <kind>` lines of one of the init
/// script's maps (`NAME='` … `'`).
fn map(init: &str, name: &str) -> Vec<(String, String, String)> {
    let start = init
        .find(&format!("\n{name}='\n"))
        .unwrap_or_else(|| panic!("no {name} in the init script"));
    let body = &init[start + name.len() + 4..];
    let body = &body[..body.find("\n'\n").unwrap()];
    body.lines()
        .map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [key, option, kind] = fields.as_slice() else {
                panic!("{name}: {line:?} isn't <key> <option> <kind>");
            };
            assert!(["value", "list", "flag"].contains(kind), "{line}");
            ((*key).to_owned(), (*option).to_owned(), (*kind).to_owned())
        })
        .collect()
}

/// The strings naming a secret (`MONOKULO_…`) in some text.
fn secrets_named(text: &str) -> BTreeSet<String> {
    text.match_indices("MONOKULO_")
        .map(|(at, _)| {
            let rest = &text[at..];
            let end = rest
                .find(|c: char| !(c.is_ascii_uppercase() || c == '_'))
                .unwrap_or(rest.len());
            rest[..end].to_owned()
        })
        .collect()
}

#[test]
fn the_openwrt_package_writes_every_setting_and_names_every_secret() {
    let init = read("monokulo-bell/files/monokulo-bell.init");
    let page = read("monokulo-bell/files/luci/monokulo-bell.js");
    let config = read("monokulo-bell/files/monokulo-bell.config");
    let rpcd = read("common/rpcd.uc");
    let service = read("common/service.sh");

    // Every setting the options file takes is written: from the maps, or
    // apart (the listen address from port and listen, the database from
    // the data folder, the status listener).
    let mut written = BTreeSet::new();
    for name in ["OPTIONS_MAP", "SQUARE_MAP", "CLOVER_MAP"] {
        for (key, option, _) in map(&init, name) {
            assert!(
                page.contains(&format!("'{option}'")),
                "the LuCI page has no {option} ({key})"
            );
            assert!(written.insert(key.clone()), "{key} is written twice");
        }
    }
    for key in [
        var(&init, "LISTEN_KEY"),
        var(&init, "STORE_KEY"),
        "status.listen".to_owned(),
    ] {
        assert!(written.insert(key.clone()), "{key} is written twice");
    }
    let options_file: BTreeSet<String> = settings::ALL
        .iter()
        .filter(|setting| setting.sources().toml)
        .map(|setting| setting.key().to_owned())
        .collect();
    assert_eq!(written, options_file);
    for option in ["port", "listen", "data_dir", "enabled"] {
        assert!(
            page.contains(&format!("'{option}'")),
            "the LuCI page has no {option}"
        );
    }

    // Its listeners: the main one's port, and the status listener (this
    // router only), where the binary would put them by itself too.
    let port: u16 = var(&init, "DEFAULT_PORT").parse().unwrap();
    assert_eq!(settings::LISTEN.default_value().0.port(), port);
    assert!(config.contains(&format!("option port '{port}'")));
    let status = var(&init, "STATUS_LISTEN");
    assert_eq!(
        settings::STATUS_LISTEN.default_value(),
        settings::STATUS_LISTEN.parse(&status).unwrap()
    );

    // The secrets: the init script's, the backend's and the page's are
    // the binary's, by their variables; the one made on install first.
    let secrets: BTreeSet<String> = settings::ALL
        .iter()
        .filter(|setting| setting.sources().env && !setting.sources().cli)
        .map(|setting| setting.env_var().to_owned())
        .collect();
    let listed: BTreeSet<String> = var(&init, "SECRETS")
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    assert_eq!(listed, secrets);
    assert_eq!(secrets_named(&init), secrets);
    assert_eq!(secrets_named(&page), secrets);
    let generated = var(&init, "GENERATED_SECRET");
    let entry = rpcd.split("\t'monokulo-bell': {").nth(1).unwrap();
    let entry = &entry[..entry.find("\n\t}").unwrap()];
    assert_eq!(secrets_named(entry), secrets);
    assert!(
        entry.contains(&format!("generated: '{generated}'")),
        "{entry}"
    );
    assert!(
        entry.contains(&format!("'http://{status}/status'")),
        "{entry}"
    );

    // Each start checks the options file as it runs on it.
    assert!(service.contains(r#""$BIN" --options "$1" --check"#));
    assert!(service.contains(r#"procd_set_param command "$BIN" --options "$OPTIONS""#));
    let check = monokulo_bell::cli::parse_args(["monokulo-bell", "--options", "x.toml", "--check"])
        .unwrap();
    assert!(check.check);
}
