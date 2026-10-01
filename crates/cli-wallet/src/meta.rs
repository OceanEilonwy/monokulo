//! Everything `monero-wallet-cli` keeps in its wallet file besides keys and
//! chain data: accounts and subaddress labels, the address book, the
//! description, and `set` options. Flattened into the wallet's own file
//! ([`crate::file::WalletData`]); nothing is written until it's set.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::amount::Unit;

/// The label `monero-wallet-cli` gives account 0 and its address 0.
pub const PRIMARY_ACCOUNT_LABEL: &str = "Primary account";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WalletMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Empty means "just the primary account" - see [`Self::accounts`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    accounts: Vec<AccountMeta>,
    #[serde(skip_serializing_if = "is_zero")]
    pub current_account: u32,
    /// Account tag name to its description (`account tag_description`).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub tag_descriptions: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub address_book: Vec<AddressBookEntry>,
    #[serde(skip_serializing_if = "Settings::is_default")]
    pub settings: Settings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountMeta {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// One label per subaddress this account has created, index 0 (the
    /// account's own address) first - so its length is the number of
    /// subaddresses the wallet watches for in this account.
    pub subaddress_labels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddressBookEntry {
    pub address: String,
    #[serde(default)]
    pub description: String,
}

/// The `set` options this wallet actually honours.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    /// `set priority`: 0 (default) to 4, as in the reference wallet.
    #[serde(skip_serializing_if = "is_zero")]
    pub priority: u32,
    #[serde(skip_serializing_if = "is_default_unit")]
    pub unit: Unit,
    /// `set always-confirm-transfers 0`: send without asking "Is this
    /// okay?".
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub skip_transfer_confirmation: bool,
}

impl Settings {
    fn is_default(&self) -> bool {
        *self == Settings::default()
    }
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn is_default_unit(unit: &Unit) -> bool {
    *unit == Unit::default()
}

impl WalletMeta {
    /// Every account, always at least the primary one.
    pub fn accounts(&self) -> Vec<AccountMeta> {
        if self.accounts.is_empty() {
            vec![AccountMeta {
                label: PRIMARY_ACCOUNT_LABEL.to_string(),
                tag: None,
                subaddress_labels: vec![PRIMARY_ACCOUNT_LABEL.to_string()],
            }]
        } else {
            self.accounts.clone()
        }
    }

    fn accounts_mut(&mut self) -> &mut Vec<AccountMeta> {
        if self.accounts.is_empty() {
            self.accounts = self.accounts();
        }
        &mut self.accounts
    }

    pub fn account(&self, index: u32) -> Result<AccountMeta, String> {
        self.accounts().get(index as usize).cloned().ok_or_else(|| {
            format!(
                "specify an index between 0 and {}",
                self.accounts().len() - 1
            )
        })
    }

    /// `account new <label>`: adds an account with its own address 0 and
    /// returns its index.
    pub fn add_account(&mut self, label: &str) -> u32 {
        let accounts = self.accounts_mut();
        accounts.push(AccountMeta {
            label: label.to_string(),
            tag: None,
            subaddress_labels: vec![label.to_string()],
        });
        (accounts.len() - 1) as u32
    }

    pub fn label_account(&mut self, index: u32, label: &str) -> Result<(), String> {
        self.account(index)?;
        let account = &mut self.accounts_mut()[index as usize];
        account.label = label.to_string();
        // The reference wallet keeps an account's label and its address 0's
        // label as one and the same.
        match account.subaddress_labels.first_mut() {
            Some(first) => *first = label.to_string(),
            None => account.subaddress_labels.push(label.to_string()),
        }
        Ok(())
    }

    pub fn tag_accounts(&mut self, tag: Option<&str>, indexes: &[u32]) -> Result<(), String> {
        for &index in indexes {
            self.account(index)?;
        }
        for &index in indexes {
            self.accounts_mut()[index as usize].tag = tag.map(str::to_string);
        }
        Ok(())
    }

    /// `address new <label>`: adds a subaddress to `account` and returns
    /// its index within it.
    pub fn add_subaddress(&mut self, account: u32, label: &str) -> Result<u32, String> {
        self.account(account)?;
        let labels = &mut self.accounts_mut()[account as usize].subaddress_labels;
        labels.push(label.to_string());
        Ok((labels.len() - 1) as u32)
    }

    pub fn label_subaddress(
        &mut self,
        account: u32,
        index: u32,
        label: &str,
    ) -> Result<(), String> {
        let count = self.account(account)?.subaddress_labels.len();
        if index as usize >= count {
            return Err(format!("specify an index between 0 and {}", count - 1));
        }
        if index == 0 {
            return self.label_account(account, label);
        }
        self.accounts_mut()[account as usize].subaddress_labels[index as usize] = label.to_string();
        Ok(())
    }

    /// Every `(account, subaddress)` this wallet has created, the primary
    /// address `(0, 0)` included.
    pub fn subaddress_indexes(&self) -> Vec<(u32, u32)> {
        self.accounts()
            .iter()
            .enumerate()
            .flat_map(|(account, meta)| {
                (0..meta.subaddress_labels.len() as u32).map(move |index| (account as u32, index))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wallet_without_metadata_has_just_the_primary_account() {
        let meta: WalletMeta = serde_json::from_str("{}").unwrap();
        assert_eq!(meta.accounts().len(), 1);
        assert_eq!(meta.subaddress_indexes(), [(0, 0)]);
        assert_eq!(
            serde_json::to_string(&meta).unwrap(),
            "{}",
            "nothing set writes nothing"
        );
    }

    #[test]
    fn accounts_and_subaddresses_are_numbered_in_creation_order() {
        let mut meta = WalletMeta::default();
        assert_eq!(meta.add_subaddress(0, "shop").unwrap(), 1);
        assert_eq!(meta.add_account("savings"), 1);
        assert_eq!(meta.add_subaddress(1, "cold").unwrap(), 1);
        assert_eq!(meta.subaddress_indexes(), [(0, 0), (0, 1), (1, 0), (1, 1)]);
        meta.label_account(1, "rainy day").unwrap();
        assert_eq!(meta.account(1).unwrap().subaddress_labels[0], "rainy day");
        assert!(meta.add_subaddress(2, "x").is_err());
        assert!(meta.label_subaddress(0, 5, "x").is_err());

        let reloaded: WalletMeta =
            serde_json::from_value(serde_json::to_value(&meta).unwrap()).unwrap();
        assert_eq!(reloaded.subaddress_indexes(), meta.subaddress_indexes());
    }
}
