//! A wallet file's SQLite schema, and reading and writing a whole
//! [`WalletData`] through it - [`crate::file::WalletFile`]'s storage.
//!
//! One database per wallet. The `wallet` table holds its one row (keys,
//! description, `set` options); everything a wallet has many of gets a
//! table of its own. `PRAGMA user_version` is the format version
//! ([`FORMAT_VERSION`]).
//!
//! A write replaces every row in one transaction, so a reader sees the
//! wallet entirely as it was or entirely as it now is, and a crash
//! mid-write leaves it as it was (the rollback journal, synced:
//! `synchronous = FULL`). The journal is the default `DELETE` mode, not
//! WAL, so a wallet at rest is the one `.db` file and nothing beside it.
//! The wallet file's own lock (see [`crate::file::WalletFile::lock`])
//! still decides who may change it: it spans a whole transfer, network
//! calls included, which no database transaction should.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use rusqlite::{params, Connection, OpenFlags, Transaction};

use crate::amount::Unit;
use crate::file::{OutputRecord, PendingTx, SentDestination, SentRecord, WalletData};
use crate::meta::{AccountMeta, AddressBookEntry, Settings, WalletMeta};
use crate::WalletError;

/// `PRAGMA user_version` of a wallet file this build reads and writes.
/// Bumped whenever [`SCHEMA`] changes incompatibly.
pub const FORMAT_VERSION: u32 = 1;

/// How long a reader waits for another process's write to commit before
/// giving up. A write is one short transaction, so this is never reached
/// in practice.
const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// Every table a wallet file has. Amounts are piconero; heights and
/// timestamps are as the chain records them. `position` columns keep each
/// list in the order the wallet recorded it.
const SCHEMA: &str = "
CREATE TABLE wallet (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    network TEXT NOT NULL CHECK (network IN ('stagenet', 'testnet')),
    address TEXT NOT NULL,
    private_spend_key TEXT NOT NULL,
    private_view_key TEXT NOT NULL,
    -- The seed phrase the keys came from, when known.
    mnemonic TEXT,
    description TEXT,
    current_account INTEGER NOT NULL DEFAULT 0 CHECK (current_account >= 0),
    -- `set` options.
    priority INTEGER NOT NULL DEFAULT 0 CHECK (priority BETWEEN 0 AND 4),
    unit TEXT NOT NULL DEFAULT 'monero'
        CHECK (unit IN ('monero', 'millinero', 'micronero', 'nanonero', 'piconero')),
    skip_transfer_confirmation INTEGER NOT NULL DEFAULT 0
        CHECK (skip_transfer_confirmation IN (0, 1))
) STRICT;

-- No rows means just the primary account.
CREATE TABLE accounts (
    account_index INTEGER PRIMARY KEY CHECK (account_index >= 0),
    label TEXT NOT NULL,
    tag TEXT
) STRICT;

-- Every subaddress an account has created, its address 0 included: these
-- are the addresses the wallet watches for payments.
CREATE TABLE subaddresses (
    account_index INTEGER NOT NULL REFERENCES accounts (account_index) ON DELETE CASCADE,
    address_index INTEGER NOT NULL CHECK (address_index >= 0),
    label TEXT NOT NULL,
    PRIMARY KEY (account_index, address_index)
) STRICT;

-- `account tag_description`.
CREATE TABLE account_tags (
    tag TEXT PRIMARY KEY,
    description TEXT NOT NULL
) STRICT;

CREATE TABLE address_book (
    position INTEGER PRIMARY KEY,
    address TEXT NOT NULL,
    description TEXT NOT NULL
) STRICT;

-- Every output the wallet has resolved, each whole (WalletOutput's own
-- serialization), so reading one back never touches the chain.
CREATE TABLE outputs (
    position INTEGER PRIMARY KEY,
    txid TEXT NOT NULL,
    height INTEGER NOT NULL CHECK (height >= 0),
    -- The confirming block's timestamp; NULL for outputs resolved before
    -- it was recorded.
    timestamp INTEGER,
    serialized_output BLOB NOT NULL,
    -- Informational: the authoritative amount is inside the output.
    amount_piconero INTEGER NOT NULL CHECK (amount_piconero >= 0),
    spent INTEGER NOT NULL CHECK (spent IN (0, 1)),
    frozen INTEGER NOT NULL DEFAULT 0 CHECK (frozen IN (0, 1))
) STRICT;
CREATE INDEX outputs_by_txid ON outputs (txid);

-- Transactions expected to pay the wallet that haven't confirmed yet.
CREATE TABLE pending (
    position INTEGER PRIMARY KEY,
    txid TEXT NOT NULL UNIQUE,
    amount_piconero INTEGER NOT NULL DEFAULT 0 CHECK (amount_piconero >= 0)
) STRICT;

-- Transactions the wallet built and broadcast.
CREATE TABLE sent (
    position INTEGER PRIMARY KEY,
    txid TEXT NOT NULL,
    account INTEGER NOT NULL CHECK (account >= 0),
    fee_piconero INTEGER NOT NULL CHECK (fee_piconero >= 0),
    change_piconero INTEGER NOT NULL CHECK (change_piconero >= 0),
    -- Filled in once the transaction's change output resolves.
    height INTEGER,
    timestamp INTEGER
) STRICT;
CREATE INDEX sent_by_txid ON sent (txid);

CREATE TABLE sent_destinations (
    sent_position INTEGER NOT NULL REFERENCES sent (position) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    address TEXT NOT NULL,
    amount_piconero INTEGER NOT NULL CHECK (amount_piconero >= 0),
    PRIMARY KEY (sent_position, position)
) STRICT;

-- `set_tx_note`.
CREATE TABLE tx_notes (
    txid TEXT PRIMARY KEY,
    note TEXT NOT NULL
) STRICT;

-- Anything else recorded about the wallet (its e2e role, where it was
-- funded from), each value as JSON.
CREATE TABLE extra (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL CHECK (json_valid(value))
) STRICT;
";

fn db_error<'a>(path: &'a Path, what: &str) -> impl Fn(rusqlite::Error) -> WalletError + 'a {
    let what = what.to_string();
    move |e| WalletError::WalletFile(format!("failed to {what} {}: {e}", path.display()))
}

/// Opens `path`'s database, creating an empty one only if `create`.
fn open(path: &Path, create: bool) -> Result<Connection, WalletError> {
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if create {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let conn = Connection::open_with_flags(path, flags).map_err(db_error(path, "open"))?;
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(db_error(path, "open"))?;
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = FULL;")
        .map_err(db_error(path, "open"))?;
    Ok(conn)
}

fn user_version(conn: &Connection, path: &Path) -> Result<u32, WalletError> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(db_error(path, "read"))
}

/// A piconero amount, height or timestamp as SQLite's signed 64 bits.
fn int(value: u64, path: &Path) -> Result<i64, WalletError> {
    i64::try_from(value).map_err(|_| {
        WalletError::WalletFile(format!(
            "{value} is too large to store in {}",
            path.display()
        ))
    })
}

fn unit_name(unit: Unit) -> String {
    match serde_json::to_value(unit).expect("a Unit always serializes") {
        serde_json::Value::String(name) => name,
        other => unreachable!("Unit serializes as a string, not {other}"),
    }
}

/// Reads the whole wallet at `path`. A file that doesn't exist is an
/// error, never created.
pub(crate) fn read(path: &Path) -> Result<WalletData, WalletError> {
    if !path.is_file() {
        return Err(WalletError::WalletFile(format!(
            "failed to read {}: no such wallet file",
            path.display()
        )));
    }
    if !is_database(path) {
        return Err(WalletError::WalletFile(format!(
            "{} isn't a wallet database (a JSON wallet file can be converted with \
             `wallet-cli import_json <json file> <wallet file>`)",
            path.display()
        )));
    }
    let mut conn = open(path, false)?;
    // One read transaction, so every table is read as of the same commit.
    let tx = conn.transaction().map_err(db_error(path, "read"))?;
    let version = user_version(&tx, path)?;
    if version != FORMAT_VERSION {
        return Err(WalletError::WalletFile(format!(
            "{} is format version {version}, this build reads {FORMAT_VERSION}",
            path.display()
        )));
    }
    read_tables(&tx).map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => {
            WalletError::WalletFile(format!("{} has no wallet row", path.display()))
        }
        e => db_error(path, "read")(e),
    })
}

fn read_tables(tx: &Transaction) -> rusqlite::Result<WalletData> {
    let row = tx.query_row(
        "SELECT network, address, private_spend_key, private_view_key, mnemonic, description,
                current_account, priority, unit, skip_transfer_confirmation
         FROM wallet",
        [],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, u32>(6)?,
                row.get::<_, u32>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, bool>(9)?,
            ))
        },
    )?;
    let (
        network,
        address,
        private_spend_key,
        private_view_key,
        mnemonic,
        description,
        current_account,
        priority,
        unit,
        skip_transfer_confirmation,
    ) = row;
    let unit: Unit = serde_json::from_value(serde_json::Value::String(unit)).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e))
    })?;

    let mut accounts: Vec<AccountMeta> = tx
        .prepare("SELECT label, tag FROM accounts ORDER BY account_index")?
        .query_map([], |row| {
            Ok(AccountMeta {
                label: row.get(0)?,
                tag: row.get(1)?,
                subaddress_labels: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut statement = tx.prepare(
        "SELECT account_index, label FROM subaddresses ORDER BY account_index, address_index",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let account: usize = row.get(0)?;
        if let Some(meta) = accounts.get_mut(account) {
            meta.subaddress_labels.push(row.get(1)?);
        }
    }
    drop(rows);
    drop(statement);

    let tag_descriptions: BTreeMap<String, String> = tx
        .prepare("SELECT tag, description FROM account_tags")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let address_book: Vec<AddressBookEntry> = tx
        .prepare("SELECT address, description FROM address_book ORDER BY position")?
        .query_map([], |row| {
            Ok(AddressBookEntry {
                address: row.get(0)?,
                description: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    let outputs: Vec<OutputRecord> = tx
        .prepare(
            "SELECT txid, height, timestamp, serialized_output, amount_piconero, spent, frozen
             FROM outputs ORDER BY position",
        )?
        .query_map([], |row| {
            Ok(OutputRecord {
                txid: row.get(0)?,
                height: row.get(1)?,
                timestamp: row.get(2)?,
                serialized_output_hex: hex::encode(row.get::<_, Vec<u8>>(3)?),
                amount_piconero: row.get(4)?,
                spent: row.get(5)?,
                frozen: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let pending: Vec<PendingTx> = tx
        .prepare("SELECT txid, amount_piconero FROM pending ORDER BY position")?
        .query_map([], |row| {
            Ok(PendingTx {
                txid: row.get(0)?,
                amount_piconero: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut sent: Vec<(i64, SentRecord)> = tx
        .prepare(
            "SELECT position, txid, account, fee_piconero, change_piconero, height, timestamp
             FROM sent ORDER BY position",
        )?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                SentRecord {
                    txid: row.get(1)?,
                    account: row.get(2)?,
                    destinations: Vec::new(),
                    fee_piconero: row.get(3)?,
                    change_piconero: row.get(4)?,
                    height: row.get(5)?,
                    timestamp: row.get(6)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut statement = tx.prepare(
        "SELECT sent_position, address, amount_piconero FROM sent_destinations
         ORDER BY sent_position, position",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let sent_position: i64 = row.get(0)?;
        if let Some((_, record)) = sent.iter_mut().find(|(p, _)| *p == sent_position) {
            record.destinations.push(SentDestination {
                address: row.get(1)?,
                amount_piconero: row.get(2)?,
            });
        }
    }
    drop(rows);
    drop(statement);

    let tx_notes: BTreeMap<String, String> = tx
        .prepare("SELECT txid, note FROM tx_notes")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let extra: serde_json::Map<String, serde_json::Value> = tx
        .prepare("SELECT key, value FROM extra ORDER BY key")?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, serde_json::Value>(1)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;

    Ok(WalletData {
        version: FORMAT_VERSION,
        network,
        address,
        private_spend_key,
        private_view_key,
        mnemonic,
        meta: WalletMeta::from_parts(
            description,
            accounts,
            current_account,
            tag_descriptions,
            address_book,
            Settings {
                priority,
                unit,
                skip_transfer_confirmation,
            },
        ),
        outputs,
        pending,
        sent: sent.into_iter().map(|(_, record)| record).collect(),
        tx_notes,
        extra,
    })
}

/// Replaces everything stored at `path` with `data`, in one transaction,
/// creating the database (and its tables) if it's new. Its permissions are
/// set to its owner's alone first: it holds spend keys.
pub(crate) fn write(path: &Path, data: &WalletData) -> Result<(), WalletError> {
    // Checked here as well as by the schema, for a clear message.
    data.network()?;
    let mut conn = open(path, true)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
            WalletError::WalletFile(format!("failed to restrict {}: {e}", path.display()))
        })?;
    }
    // IMMEDIATE: take the write lock before reading the version, so two
    // first writes can't both create the tables.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(db_error(path, "write"))?;
    match user_version(&tx, path)? {
        0 => {
            tx.execute_batch(SCHEMA)
                .map_err(db_error(path, "create the tables of"))?;
            tx.pragma_update(None, "user_version", FORMAT_VERSION)
                .map_err(db_error(path, "write"))?;
        }
        FORMAT_VERSION => {}
        other => {
            return Err(WalletError::WalletFile(format!(
                "{} is format version {other}, this build writes {FORMAT_VERSION}",
                path.display()
            )))
        }
    }
    write_tables(&tx, data, path)?;
    tx.commit().map_err(db_error(path, "write"))
}

fn write_tables(tx: &Transaction, data: &WalletData, path: &Path) -> Result<(), WalletError> {
    let failed = db_error(path, "write");
    // Children before parents, for the foreign keys.
    tx.execute_batch(
        "DELETE FROM sent_destinations; DELETE FROM sent; DELETE FROM subaddresses;
         DELETE FROM accounts; DELETE FROM account_tags; DELETE FROM address_book;
         DELETE FROM outputs; DELETE FROM pending; DELETE FROM tx_notes; DELETE FROM extra;
         DELETE FROM wallet;",
    )
    .map_err(&failed)?;

    let meta = &data.meta;
    tx.execute(
        "INSERT INTO wallet (id, network, address, private_spend_key, private_view_key, mnemonic,
                             description, current_account, priority, unit,
                             skip_transfer_confirmation)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            data.network,
            data.address,
            data.private_spend_key,
            data.private_view_key,
            data.mnemonic,
            meta.description,
            meta.current_account,
            meta.settings.priority,
            unit_name(meta.settings.unit),
            meta.settings.skip_transfer_confirmation,
        ],
    )
    .map_err(&failed)?;

    for (index, account) in meta.stored_accounts().iter().enumerate() {
        tx.execute(
            "INSERT INTO accounts (account_index, label, tag) VALUES (?1, ?2, ?3)",
            params![index, account.label, account.tag],
        )
        .map_err(&failed)?;
        for (address_index, label) in account.subaddress_labels.iter().enumerate() {
            tx.execute(
                "INSERT INTO subaddresses (account_index, address_index, label)
                 VALUES (?1, ?2, ?3)",
                params![index, address_index, label],
            )
            .map_err(&failed)?;
        }
    }
    for (tag, description) in &meta.tag_descriptions {
        tx.execute(
            "INSERT INTO account_tags (tag, description) VALUES (?1, ?2)",
            params![tag, description],
        )
        .map_err(&failed)?;
    }
    for (position, entry) in meta.address_book.iter().enumerate() {
        tx.execute(
            "INSERT INTO address_book (position, address, description) VALUES (?1, ?2, ?3)",
            params![position, entry.address, entry.description],
        )
        .map_err(&failed)?;
    }

    let mut insert = tx
        .prepare(
            "INSERT INTO outputs (position, txid, height, timestamp, serialized_output,
                                  amount_piconero, spent, frozen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .map_err(&failed)?;
    for (position, output) in data.outputs.iter().enumerate() {
        let serialized = hex::decode(&output.serialized_output_hex).map_err(|e| {
            WalletError::WalletFile(format!(
                "output of {} has invalid serialized_output_hex: {e}",
                output.txid
            ))
        })?;
        insert
            .execute(params![
                position,
                output.txid,
                int(output.height, path)?,
                output.timestamp.map(|t| int(t, path)).transpose()?,
                serialized,
                int(output.amount_piconero, path)?,
                output.spent,
                output.frozen,
            ])
            .map_err(&failed)?;
    }
    drop(insert);

    for (position, pending) in data.pending.iter().enumerate() {
        tx.execute(
            "INSERT INTO pending (position, txid, amount_piconero) VALUES (?1, ?2, ?3)",
            params![position, pending.txid, int(pending.amount_piconero, path)?],
        )
        .map_err(&failed)?;
    }

    for (position, sent) in data.sent.iter().enumerate() {
        tx.execute(
            "INSERT INTO sent (position, txid, account, fee_piconero, change_piconero, height,
                               timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                position,
                sent.txid,
                sent.account,
                int(sent.fee_piconero, path)?,
                int(sent.change_piconero, path)?,
                sent.height.map(|h| int(h, path)).transpose()?,
                sent.timestamp.map(|t| int(t, path)).transpose()?,
            ],
        )
        .map_err(&failed)?;
        for (destination_position, destination) in sent.destinations.iter().enumerate() {
            tx.execute(
                "INSERT INTO sent_destinations (sent_position, position, address,
                                                amount_piconero)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    position,
                    destination_position,
                    destination.address,
                    int(destination.amount_piconero, path)?,
                ],
            )
            .map_err(&failed)?;
        }
    }

    for (txid, note) in &data.tx_notes {
        tx.execute(
            "INSERT INTO tx_notes (txid, note) VALUES (?1, ?2)",
            params![txid, note],
        )
        .map_err(&failed)?;
    }
    for (key, value) in &data.extra {
        tx.execute(
            "INSERT INTO extra (key, value) VALUES (?1, ?2)",
            params![key, value],
        )
        .map_err(&failed)?;
    }
    Ok(())
}

/// Whether `path` is a wallet database rather than something else (a
/// JSON wallet file from before), by SQLite's file header.
pub(crate) fn is_database(path: &Path) -> bool {
    use std::io::Read;
    let mut header = [0u8; 16];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .is_ok_and(|()| &header == b"SQLite format 3\0")
}

/// The tables a wallet file has, by name - for tests and diagnostics.
#[cfg(test)]
pub(crate) fn tables(path: &Path) -> Vec<String> {
    let conn = open(path, false).unwrap();
    let names = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<String>>>()
        .unwrap();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Network, WalletCredentials};

    fn temp_path(test: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cli-wallet-store-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("w.db")
    }

    /// A wallet with something in every table.
    fn full_wallet() -> WalletData {
        let mut data = WalletData::new(
            Network::Testnet,
            WalletCredentials {
                address: "9address".into(),
                private_spend_key_hex: "11".repeat(32),
                private_view_key_hex: "22".repeat(32),
                mnemonic: Some("abbey ability able".into()),
            },
        );
        data.meta.description = Some("test wallet".into());
        data.meta.add_subaddress(0, "shop").unwrap();
        let savings = data.meta.add_account("savings");
        data.meta.add_subaddress(savings, "cold").unwrap();
        data.meta.tag_accounts(Some("rainy"), &[savings]).unwrap();
        data.meta.current_account = savings;
        data.meta
            .tag_descriptions
            .insert("rainy".into(), "for later".into());
        data.meta.address_book.push(AddressBookEntry {
            address: "9friend".into(),
            description: "a friend".into(),
        });
        data.meta.settings = Settings {
            priority: 3,
            unit: Unit::Millinero,
            skip_transfer_confirmation: true,
        };
        for (n, spent) in [(1u8, true), (2, false)] {
            data.outputs.push(OutputRecord {
                txid: hex::encode([n; 32]),
                height: 100 + n as u64,
                timestamp: (n == 2).then_some(1_700_000_000),
                serialized_output_hex: hex::encode([n, 0xff, 0]),
                amount_piconero: 1_000_000_000 * n as u64,
                spent,
                frozen: n == 2,
            });
        }
        data.add_pending(&"ab".repeat(32), 5);
        data.add_pending(&"cd".repeat(32), 0);
        data.sent.push(SentRecord {
            txid: hex::encode([1; 32]),
            account: 0,
            destinations: vec![
                SentDestination {
                    address: "9friend".into(),
                    amount_piconero: 7,
                },
                SentDestination {
                    address: "9other".into(),
                    amount_piconero: 8,
                },
            ],
            fee_piconero: 30_000_000,
            change_piconero: 12,
            height: Some(101),
            timestamp: None,
        });
        data.tx_notes.insert(hex::encode([1; 32]), "rent".into());
        data.extra.insert(
            "role".into(),
            serde_json::json!({ "funded_by": ["faucet", 1] }),
        );
        data
    }

    #[test]
    fn every_table_round_trips_in_order() {
        let path = temp_path("round-trip");
        let data = full_wallet();
        write(&path, &data).unwrap();
        assert!(is_database(&path));
        let read_back = read(&path).unwrap();
        assert_eq!(
            serde_json::to_value(&read_back).unwrap(),
            serde_json::to_value(&data).unwrap()
        );

        // A second write replaces, never appends.
        let mut fewer = data.clone();
        fewer.outputs.truncate(1);
        fewer.sent.clear();
        fewer.meta.address_book.clear();
        write(&path, &fewer).unwrap();
        assert_eq!(
            serde_json::to_value(read(&path).unwrap()).unwrap(),
            serde_json::to_value(&fewer).unwrap()
        );
        assert_eq!(
            tables(&path),
            [
                "account_tags",
                "accounts",
                "address_book",
                "extra",
                "outputs",
                "pending",
                "sent",
                "sent_destinations",
                "subaddresses",
                "tx_notes",
                "wallet"
            ]
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_wallet_with_nothing_set_stores_just_its_keys() {
        let path = temp_path("bare");
        let data = WalletData::new(
            Network::Stagenet,
            WalletCredentials {
                address: "5address".into(),
                private_spend_key_hex: "11".repeat(32),
                private_view_key_hex: "22".repeat(32),
                mnemonic: None,
            },
        );
        write(&path, &data).unwrap();
        let read_back = read(&path).unwrap();
        assert_eq!(
            serde_json::to_value(&read_back).unwrap(),
            serde_json::to_value(&data).unwrap()
        );
        assert_eq!(
            read_back.meta.accounts().len(),
            1,
            "just the primary account"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_missing_file_is_never_created_by_reading_it() {
        let path = temp_path("missing");
        let error = read(&path).unwrap_err();
        assert!(error.to_string().contains("no such wallet file"), "{error}");
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn another_format_version_is_refused() {
        let path = temp_path("version");
        write(&path, &full_wallet()).unwrap();
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", FORMAT_VERSION + 1)
            .unwrap();
        let error = read(&path).unwrap_err();
        assert!(
            error.to_string().contains(&format!(
                "is format version {}, this build reads {FORMAT_VERSION}",
                FORMAT_VERSION + 1
            )),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The schema itself refuses a wallet for a network this wallet
    /// doesn't work on, whoever writes it.
    #[test]
    fn the_schema_refuses_mainnet() {
        let path = temp_path("mainnet");
        write(&path, &full_wallet()).unwrap();
        let conn = Connection::open(&path).unwrap();
        assert!(conn
            .execute("UPDATE wallet SET network = 'mainnet'", [])
            .is_err());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_json_file_is_not_a_database() {
        let path = temp_path("json");
        std::fs::write(&path, "{}").unwrap();
        assert!(!is_database(&path));
        assert!(read(&path).is_err());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
