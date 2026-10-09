//! A merchant's named wallets (docs/wallets.md): the friendly names a wallet
//! gets when its owner doesn't give one, and the wallet apps a new wallet's
//! recovery phrase can be saved in.

use rand::seq::IndexedRandom as _;

/// The longest name a wallet may have.
pub const MAX_NAME_LEN: usize = 60;

const ADJECTIVES: &[&str] = &[
    "Amber", "Ash", "Autumn", "Brass", "Bright", "Bronze", "Calm", "Cedar", "Cobalt", "Copper",
    "Coral", "Crimson", "Dawn", "Dusk", "Ember", "Fern", "Frost", "Golden", "Granite", "Hazel",
    "Indigo", "Iron", "Ivory", "Jade", "Juniper", "Linen", "Maple", "Meadow", "Misty", "Mossy",
    "Oak", "Olive", "Pebble", "Pine", "Quiet", "River", "Russet", "Saffron", "Sage", "Silver",
    "Slate", "Spruce", "Steady", "Stone", "Summer", "Swift", "Teal", "Velvet", "Willow", "Winter",
];

const NOUNS: &[&str] = &[
    "Badger", "Beacon", "Bramble", "Brook", "Canyon", "Cardinal", "Comet", "Crane", "Falcon",
    "Fern", "Finch", "Fox", "Garden", "Harbor", "Hare", "Hearth", "Heron", "Kestrel", "Kettle",
    "Lantern", "Lark", "Lynx", "Marten", "Meadow", "Orchard", "Otter", "Owl", "Pine", "Plover",
    "Quarry", "Raven", "Ridge", "Robin", "Sparrow", "Spring", "Starling", "Stream", "Swallow",
    "Thistle", "Thrush", "Tide", "Valley", "Vale", "Warbler", "Willow", "Wren",
];

/// A memorable name, "Copper Heron", that none of `taken` already is
/// (compared without case). Falls back to a numbered name in the unlikely
/// case every pairing tried is taken.
pub fn friendly_name(taken: &[String]) -> String {
    let mut rng = rand::rng();
    let is_free = |name: &str| !taken.iter().any(|t| t.eq_ignore_ascii_case(name));
    for _ in 0..64 {
        let adjective = ADJECTIVES.choose(&mut rng).copied().unwrap_or("Copper");
        let noun = NOUNS.choose(&mut rng).copied().unwrap_or("Heron");
        if adjective == noun {
            continue;
        }
        let name = format!("{adjective} {noun}");
        if is_free(&name) {
            return name;
        }
    }
    (2..)
        .map(|n| format!("Wallet {n}"))
        .find(|name| is_free(name))
        .unwrap_or_else(|| "Wallet".to_owned())
}

/// A name as the merchant typed it: trimmed, inner runs of spaces made
/// one. `None` for a blank one (a friendly name is picked instead);
/// `Err` for one too long to show.
pub fn clean_name(raw: &str) -> Result<Option<String>, String> {
    let name = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Ok(None);
    }
    if name.chars().count() > MAX_NAME_LEN {
        return Err(format!(
            "A wallet name can be at most {MAX_NAME_LEN} characters."
        ));
    }
    Ok(Some(name))
}

/// A wallet app a new wallet's recovery phrase can be saved in, and how:
/// one tab of the backup screen.
pub struct WalletApp {
    /// The key a backup is recorded under (`wallets.backup`), and the
    /// suffix of its logo (`/static/wallet-logos/{key}.png`).
    pub key: &'static str,
    pub name: &'static str,
    /// What its tab says, after these logos (`/static/wallet-logos/`).
    pub tab: &'static str,
    pub logos: &'static [&'static str],
    /// The heading over the phrase on its tab.
    pub title: &'static str,
    pub method: AppMethod,
    /// How to restore the wallet in it, step by step.
    pub restore_steps: &'static [&'static str],
    /// What its owner ticks once the app has the wallet.
    pub done_check: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AppMethod {
    /// Scans a `monero_wallet:` restore link (Cake Wallet, Monero.com).
    ScanRestoreLink,
    /// Scans `{"mnemonic": [words]}` into its word boxes (Stack Wallet).
    ScanWordList,
    /// Has the 16 words typed in (Feather).
    TypeWords,
    /// Reads only 25-word phrases (Monero GUI and CLI).
    TypeLegacyWords,
}

impl AppMethod {
    /// The `data-qr` the page's script draws the code from; `None` for an
    /// app that has the words typed in.
    pub fn qr_kind(self) -> Option<&'static str> {
        match self {
            Self::ScanRestoreLink => Some("restore-link"),
            Self::ScanWordList => Some("word-list"),
            Self::TypeWords | Self::TypeLegacyWords => None,
        }
    }
}

/// The wallet apps the backup screen offers, in the order of its tabs
/// (writing the words on paper is the last tab, `paper`).
pub const WALLET_APPS: &[WalletApp] = &[
    WalletApp {
        key: "cake",
        name: "Cake Wallet",
        tab: "Cake / Monero.com",
        logos: &["cake", "monerocom"],
        title: "Enter into Cake Wallet",
        method: AppMethod::ScanRestoreLink,
        restore_steps: &[
            "Open Cake Wallet (or Monero.com, by the same team) and choose Restore wallet.",
            "Pick Scan QR code and point your phone at the code here.",
            "Check the wallet has this wallet's name, then set a PIN.",
        ],
        done_check: "Cake Wallet opened the wallet from this QR.",
    },
    WalletApp {
        key: "stack",
        name: "Stack Wallet",
        tab: "Stack",
        logos: &["stack"],
        title: "Enter into Stack Wallet",
        method: AppMethod::ScanWordList,
        restore_steps: &[
            "Tap Add wallet, choose Monero, then Restore, and the 16-word phrase length.",
            "Tap the QR icon above the word boxes and scan the code here.",
            "Set the restore date to today: the wallet is new.",
        ],
        done_check: "Stack Wallet opened the wallet from this QR.",
    },
    WalletApp {
        key: "feather",
        name: "Feather",
        tab: "Feather",
        logos: &["feather"],
        title: "Type these 16 words into Feather",
        method: AppMethod::TypeWords,
        restore_steps: &[
            "In Feather: File, then New/Restore.",
            "Choose Restore wallet from seed, seed type Polyseed, and type the 16 words. Feather reads the wallet's birthday from them.",
        ],
        done_check: "Feather opened the wallet with these words.",
    },
    WalletApp {
        key: "gui",
        name: "Monero GUI / CLI",
        tab: "Monero GUI / CLI",
        logos: &["gui"],
        title: "25-word phrase",
        method: AppMethod::TypeLegacyWords,
        restore_steps: &[
            "These apps only read 25-word phrases: this is the same wallet in that older format.",
            "Type the 25 words, then the restore height shown with them.",
        ],
        done_check: "The Monero GUI or CLI opened the wallet with these 25 words.",
    },
];

/// How a created wallet's recovery phrase was saved: one of the apps'
/// keys, `paper`, or `skipped`. Anything else isn't recorded.
pub fn is_known_backup(backup: &str) -> bool {
    backup == "paper" || backup == "skipped" || WALLET_APPS.iter().any(|app| app.key == backup)
}

/// Which app a brought-in wallet is in, as its owner said: one of the
/// apps' keys, or `other`. Anything else isn't recorded.
pub fn is_known_app(app: &str) -> bool {
    app == "other" || WALLET_APPS.iter().any(|a| a.key == app)
}

/// The app with this key.
pub fn wallet_app(key: &str) -> Option<&'static WalletApp> {
    WALLET_APPS.iter().find(|app| app.key == key)
}

/// What a backup is called on a wallet's page.
pub fn backup_label(backup: &str) -> String {
    match backup {
        "paper" => "Written down".to_owned(),
        "skipped" => "Not backed up".to_owned(),
        key => WALLET_APPS
            .iter()
            .find(|app| app.key == key)
            .map(|app| format!("Saved in {}", app.name))
            .unwrap_or_else(|| key.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_friendly_name_is_two_words_and_never_one_already_taken() {
        let mut taken = Vec::new();
        for _ in 0..200 {
            let name = friendly_name(&taken);
            assert!(!taken.iter().any(|t: &String| t.eq_ignore_ascii_case(&name)));
            taken.push(name);
        }
        assert!(taken[0].split(' ').count() == 2);
    }

    #[test]
    fn when_every_pairing_is_taken_a_numbered_name_is_used() {
        let mut taken: Vec<String> = ADJECTIVES
            .iter()
            .flat_map(|a| NOUNS.iter().map(move |n| format!("{a} {n}")))
            .collect();
        taken.push("Wallet 2".to_owned());
        assert_eq!(friendly_name(&taken), "Wallet 3");
    }

    #[test]
    fn names_are_tidied_blank_means_pick_one_and_long_ones_are_refused() {
        assert_eq!(
            clean_name("  Market   stall ").unwrap().as_deref(),
            Some("Market stall")
        );
        assert_eq!(clean_name("   ").unwrap(), None);
        assert!(clean_name(&"x".repeat(MAX_NAME_LEN + 1)).is_err());
    }

    #[test]
    fn backups_are_the_apps_paper_or_skipped() {
        for app in WALLET_APPS {
            assert!(is_known_backup(app.key));
        }
        assert!(is_known_backup("paper") && is_known_backup("skipped"));
        assert!(!is_known_backup("dropbox"));
        assert_eq!(backup_label("cake"), "Saved in Cake Wallet");
        for app in WALLET_APPS {
            assert!(is_known_app(app.key));
        }
        assert!(is_known_app("other"));
        assert!(!is_known_app("paper") && !is_known_app("skipped") && !is_known_app(""));
    }
}
