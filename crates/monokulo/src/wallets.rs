//! A merchant's named wallets: the friendly names a wallet
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
    /// name of its logo (the asset `wallet-logos/{key}.png`).
    pub key: &'static str,
    pub name: &'static str,
    /// What its tab says, after these logos (the `wallet-logos/` assets).
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

/// A wallet app the backup screen offers, and a brought-in wallet can be
/// in: [`AppKey::app`] says which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppKey {
    Cake,
    Stack,
    Feather,
    Gui,
}

impl AppKey {
    /// Every app, in the order of the backup screen's tabs.
    pub const ALL: [AppKey; 4] = [AppKey::Cake, AppKey::Stack, AppKey::Feather, AppKey::Gui];

    pub const fn app(self) -> &'static WalletApp {
        match self {
            AppKey::Cake => &CAKE,
            AppKey::Stack => &STACK,
            AppKey::Feather => &FEATHER,
            AppKey::Gui => &GUI,
        }
    }

    /// Its key, as the forms send it and `wallets` keeps it.
    pub fn key(self) -> &'static str {
        self.app().key
    }

    /// The app a form names.
    pub fn parse(key: &str) -> Option<Self> {
        AppKey::ALL.into_iter().find(|app| app.key() == key)
    }

    /// The app a stored row names: as [`Self::parse`], and `monerocom`,
    /// Monero.com's own key before it shared Cake Wallet's tab (the same
    /// team's app), as Cake's.
    fn stored(key: &str) -> Option<Self> {
        Self::parse(key).or((key == "monerocom").then_some(AppKey::Cake))
    }
}

/// How a made wallet's recovery phrase was saved (`wallets.backup`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backup {
    App(AppKey),
    /// The words written down.
    Paper,
    Skipped,
}

impl Backup {
    /// As the backup screen sends it and `wallets.backup` keeps it.
    pub fn as_str(self) -> &'static str {
        match self {
            Backup::App(app) => app.key(),
            Backup::Paper => "paper",
            Backup::Skipped => "skipped",
        }
    }

    /// The backup a form names: one of the apps' keys, `paper`, or
    /// `skipped`.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "paper" => Some(Backup::Paper),
            "skipped" => Some(Backup::Skipped),
            key => AppKey::parse(key).map(Backup::App),
        }
    }

    fn stored(text: &str) -> Option<Self> {
        Self::parse(text).or_else(|| AppKey::stored(text).map(Backup::App))
    }

    /// What it is called on a wallet's page.
    pub fn label(self) -> String {
        match self {
            Backup::Paper => "Written down".to_owned(),
            Backup::Skipped => "Not backed up".to_owned(),
            Backup::App(app) => format!("Saved in {}", app.app().name),
        }
    }
}

/// Which app a brought-in wallet is in, as its owner said (`wallets.app`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BroughtFrom {
    App(AppKey),
    /// An app the page doesn't list.
    Other,
}

impl BroughtFrom {
    /// As the form sends it and `wallets.app` keeps it.
    pub fn as_str(self) -> &'static str {
        match self {
            BroughtFrom::App(app) => app.key(),
            BroughtFrom::Other => "other",
        }
    }

    /// The app a form names: one of the apps' keys, or `other`.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "other" => Some(BroughtFrom::Other),
            key => AppKey::parse(key).map(BroughtFrom::App),
        }
    }

    fn stored(text: &str) -> Option<Self> {
        Self::parse(text).or_else(|| AppKey::stored(text).map(BroughtFrom::App))
    }
}

/// A stored value no variant has: the row is refused rather than read as
/// something else.
fn unknown(column: &str, text: &str) -> rusqlite::types::FromSqlError {
    rusqlite::types::FromSqlError::Other(format!("unknown wallets.{column} {text:?}").into())
}

impl rusqlite::types::FromSql for Backup {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let text = value.as_str()?;
        Backup::stored(text).ok_or_else(|| unknown("backup", text))
    }
}

impl rusqlite::types::ToSql for Backup {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl rusqlite::types::FromSql for BroughtFrom {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let text = value.as_str()?;
        BroughtFrom::stored(text).ok_or_else(|| unknown("app", text))
    }
}

impl rusqlite::types::ToSql for BroughtFrom {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

/// The wallet apps the backup screen offers, in the order of its tabs
/// (writing the words on paper is the last tab, `paper`).
pub const WALLET_APPS: [&WalletApp; 4] = [
    AppKey::Cake.app(),
    AppKey::Stack.app(),
    AppKey::Feather.app(),
    AppKey::Gui.app(),
];

const CAKE: WalletApp = WalletApp {
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
};

const STACK: WalletApp = WalletApp {
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
};

const FEATHER: WalletApp = WalletApp {
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
    };

const GUI: WalletApp = WalletApp {
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
};

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
        for app in AppKey::ALL {
            assert_eq!(Backup::parse(app.key()), Some(Backup::App(app)));
            assert_eq!(BroughtFrom::parse(app.key()), Some(BroughtFrom::App(app)));
        }
        assert_eq!(Backup::parse("paper"), Some(Backup::Paper));
        assert_eq!(Backup::parse("skipped"), Some(Backup::Skipped));
        assert_eq!(Backup::parse("dropbox"), None);
        assert_eq!(Backup::App(AppKey::Cake).label(), "Saved in Cake Wallet");
        assert_eq!(BroughtFrom::parse("other"), Some(BroughtFrom::Other));
        for not_an_app in ["paper", "skipped", ""] {
            assert_eq!(BroughtFrom::parse(not_an_app), None);
        }
    }
}
