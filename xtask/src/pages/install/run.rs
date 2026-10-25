//! Running Monokulo once it's installed: where its config and data live
//! (`config`), and how to back them up, update and remove it (`backups`),
//! each on a computer and on OpenWrt.

use super::{command, path, Site};
use maud::{html, Markup};

pub(in crate::pages) const CONFIG_TITLE: &str = "Config and data";

pub(in crate::pages) const CONFIG_LEAD: &str =
    "Where Monokulo keeps its settings, its databases and the key that encrypts them.";

pub(in crate::pages) fn config(_: &Site) -> Markup {
    html! {
        h2 id="on-a-computer" { "On a computer" }
        p {
            "Monokulo’s admin page saves settings in "
            code { "monokulo.toml" }
            ". The default locations are:"
        }
        (ui::Table::new(html! {
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
        }).head(html! {
            tr {
                th scope="col" { "System" }
                th scope="col" { "Config file" }
                th scope="col" { "Data folder" }
            }
        }))
        p.muted.small {
            "On Linux and macOS, "
            code { "~" }
            " means your home folder. On Windows, paste "
            code { "%APPDATA%\\monokulo" }
            " into File Explorer’s address bar to find the config folder."
        }
        p {
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
        p {
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
        h2 id="on-openwrt" { "On OpenWrt" }
        p {
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
        (command("scp root@192.168.1.1:/etc/monokulo/secrets monokulo-secrets", "Copy the backup command"))
        (ui::Alert::new("Keep that file private: with it and a copy of the data folder, anyone can read your stores’ details.", ui::Tone::Warning).quiet(true))
    }
}

pub(in crate::pages) const BACKUPS_TITLE: &str = "Backups and updates";

pub(in crate::pages) const BACKUPS_LEAD: &str =
    "What to keep a copy of, and how to update or remove Monokulo without losing it.";

pub(in crate::pages) fn backups(_: &Site) -> Markup {
    html! {
        h2 id="on-a-computer" { "On a computer" }
        p {
            "Back up "
            code { "monokulo.key" }
            ", your options file and both databases ("
            code { "monokulo.db" }
            " and "
            code { "engine.db" }
            "; "
            a href="../config/" { "Config and data" }
            " says where they are). Stop Monokulo before copying the databases. To update, stop it, extract the new ZIP, then start the new executable with your existing key and data."
        }
        (ui::Alert::new("Never generate a new key for an existing installation. Losing the key makes encrypted data unreadable. Keep it private: anyone with the key and database can read your stores’ details.", ui::Tone::Warning).quiet(true))
        h2 id="updating-on-openwrt" { "Updating on OpenWrt" }
        p {
            span.path.inline { (path(&["System", "Software"])) }
            ": click "
            strong { "Update lists…" }
            ", then look in the "
            em { "Updates" }
            " tab. Or in a terminal:"
        }
        (command("apk update && apk upgrade monokulo", "Copy the update command"))
        p.muted.small {
            "Settings, secrets and databases are kept. After a firmware upgrade, repeat the "
            a href="../install-router/#install-it" { "install steps" }
            " to put the package back; your data is still there."
        }
        h2 id="removing-on-openwrt" { "Removing on OpenWrt" }
        p {
            span.path.inline { (path(&["System", "Software"])) }
            ": in the "
            em { "Installed" }
            " tab, click "
            strong { "Remove…" }
            " on monokulo. Or in a terminal:"
        }
        (command("apk del monokulo\nrm -rf /srv/monokulo /etc/monokulo   # also delete all data and secrets", "Copy the remove commands"))
    }
}
