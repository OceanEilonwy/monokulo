//! `monero-wallet-cli`'s positional argument grammar - optional leading
//! `index=`/priority/ring-size words, `key=value` options, address/amount
//! pairs - which clap can't express, so commands that use it take their
//! words raw and parse them here.

use std::collections::VecDeque;

use cli_wallet::amount::{parse_amount, Unit};
use cli_wallet::RING_LEN;

/// `index=<N1>[,<N2>,...]`, or `index=all` where a command allows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexSelection {
    All,
    Some(Vec<u32>),
}

/// The reference wallet's priority names, 0 to 4.
pub const PRIORITY_NAMES: [&str; 5] = ["default", "unimportant", "normal", "elevated", "priority"];

pub fn parse_priority(word: &str) -> Option<u32> {
    PRIORITY_NAMES
        .iter()
        .position(|name| *name == word)
        .map(|p| p as u32)
        .or_else(|| word.parse().ok().filter(|p| *p <= 4))
}

/// Pops a leading `index=...` word, if there is one.
pub fn take_index(
    args: &mut VecDeque<String>,
    allow_all: bool,
) -> Result<Option<IndexSelection>, String> {
    let Some(list) = args
        .front()
        .and_then(|word| word.strip_prefix("index="))
        .map(str::to_string)
    else {
        return Ok(None);
    };
    args.pop_front();
    if list == "all" {
        return if allow_all {
            Ok(Some(IndexSelection::All))
        } else {
            Err("index=all isn't allowed here".to_string())
        };
    }
    list.split(',')
        .map(|n| n.parse().map_err(|_| format!("failed to parse index: {n}")))
        .collect::<Result<Vec<u32>, _>>()
        .map(|indexes| Some(IndexSelection::Some(indexes)))
}

/// Pops a leading priority word, if there is one. Only names count here:
/// a bare number in that position is a ring size.
pub fn take_priority(args: &mut VecDeque<String>) -> Option<u32> {
    let priority =
        args.front()
            .and_then(|word| PRIORITY_NAMES.iter().position(|name| name == word))? as u32;
    args.pop_front();
    Some(priority)
}

/// Pops a leading ring size, if there is one. This wallet always signs
/// with the only ring size the network accepts, so anything else is an
/// error rather than silently ignored.
pub fn take_ring_size(args: &mut VecDeque<String>) -> Result<(), String> {
    let Some(size) = args
        .front()
        .filter(|word| !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit()))
    else {
        return Ok(());
    };
    let size: u64 = size
        .parse()
        .map_err(|_| format!("ring size {size} is too large"))?;
    if size != RING_LEN as u64 {
        return Err(format!("ring size {size} is not supported: this wallet always uses {RING_LEN}, the only size the network accepts"));
    }
    args.pop_front();
    Ok(())
}

/// Pops a `<prefix><value>` word from anywhere in `args`.
pub fn take_option(args: &mut VecDeque<String>, prefix: &str) -> Option<String> {
    let position = args.iter().position(|word| word.starts_with(prefix))?;
    args.remove(position)
        .map(|word| word[prefix.len()..].to_string())
}

pub const OBSOLETE_PAYMENT_ID: &str =
    "Standalone payment IDs are obsolete. Use subaddresses or integrated addresses instead";

fn looks_like_payment_id(word: &str) -> bool {
    (word.len() == 16 || word.len() == 64) && word.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Which destinations pay the fee (`subtractfeefrom=`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubtractFee {
    None,
    All,
    Some(Vec<usize>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferArgs {
    pub indexes: Option<IndexSelection>,
    pub priority: Option<u32>,
    pub destinations: Vec<(String, u64)>,
    pub subtract_fee: SubtractFee,
}

/// `transfer [index=<N1>[,<N2>,...]] [<priority>] [<ring_size>] (<URI> |
/// <address> <amount>) [<address> <amount> ...] [subtractfeefrom=<D0>[,<D1>,all,...]]`
pub fn parse_transfer(words: &[String], unit: Unit) -> Result<TransferArgs, String> {
    let mut args: VecDeque<String> = words.iter().cloned().collect();
    let indexes = take_index(&mut args, false)?;
    let priority = take_priority(&mut args);
    take_ring_size(&mut args)?;
    let subtract_fee = match take_option(&mut args, "subtractfeefrom=") {
        None => SubtractFee::None,
        Some(list) if list.split(',').any(|d| d == "all") => SubtractFee::All,
        Some(list) => SubtractFee::Some(
            list.split(',')
                .map(|d| {
                    d.parse()
                        .map_err(|_| format!("failed to parse subtractfeefrom index: {d}"))
                })
                .collect::<Result<_, _>>()?,
        ),
    };

    let mut destinations = Vec::new();
    if args.len() == 1 && args[0].starts_with("monero:") {
        destinations.push(parse_uri(&args[0])?);
    } else {
        if args.len() % 2 == 1 {
            if args.back().is_some_and(|last| looks_like_payment_id(last)) {
                return Err(OBSOLETE_PAYMENT_ID.to_string());
            }
            return Err("wrong number of arguments: expected <address> <amount> pairs".to_string());
        }
        while let (Some(address), Some(amount)) = (args.pop_front(), args.pop_front()) {
            destinations.push((address, parse_amount(&amount, unit)?));
        }
    }
    if destinations.is_empty() {
        return Err("wrong number of arguments: expected <address> <amount> pairs".to_string());
    }
    Ok(TransferArgs {
        indexes,
        priority,
        destinations,
        subtract_fee,
    })
}

/// `monero:<address>?tx_amount=<amount>` - a Monero payment URI. Its
/// amount is always in monero, whatever `set unit` says.
pub fn parse_uri(uri: &str) -> Result<(String, u64), String> {
    let rest = uri
        .strip_prefix("monero:")
        .ok_or_else(|| format!("not a monero: URI: {uri}"))?;
    let (address, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut amount = None;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        match pair.split_once('=').unwrap_or((pair, "")) {
            ("tx_amount", value) => amount = Some(parse_amount(value, Unit::Monero)?),
            ("tx_payment_id", _) => return Err(OBSOLETE_PAYMENT_ID.to_string()),
            // recipient_name, tx_description and the like are for display.
            _ => {}
        }
    }
    let amount = amount.ok_or_else(|| format!("URI has no tx_amount: {uri}"))?;
    Ok((address.to_string(), amount))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepArgs {
    pub indexes: Option<IndexSelection>,
    pub priority: Option<u32>,
    pub outputs: usize,
    pub address: String,
}

/// The shared tail of every sweep: `[<priority>] [<ring_size>]
/// [outputs=<N>] <address>`, after whatever each sweep takes first.
fn parse_sweep_tail(
    mut args: VecDeque<String>,
    indexes: Option<IndexSelection>,
) -> Result<SweepArgs, String> {
    let priority = take_priority(&mut args);
    take_ring_size(&mut args)?;
    let outputs = match take_option(&mut args, "outputs=") {
        Some(n) => n
            .parse()
            .ok()
            .filter(|n| *n >= 1)
            .ok_or_else(|| format!("amount of outputs should be greater than 0: {n}"))?,
        None => 1,
    };
    match (args.pop_front(), args.pop_front()) {
        (Some(address), None) => Ok(SweepArgs {
            indexes,
            priority,
            outputs,
            address,
        }),
        (Some(_), Some(last)) if args.is_empty() && looks_like_payment_id(&last) => {
            Err(OBSOLETE_PAYMENT_ID.to_string())
        }
        _ => Err("wrong number of arguments: expected exactly one address".to_string()),
    }
}

/// `sweep_all [index=<N1>[,<N2>,...] | index=all] [<priority>] [<ring_size>] [outputs=<N>] <address>`
pub fn parse_sweep_all(words: &[String]) -> Result<SweepArgs, String> {
    let mut args: VecDeque<String> = words.iter().cloned().collect();
    let indexes = take_index(&mut args, true)?;
    parse_sweep_tail(args, indexes)
}

/// `sweep_account <account> [index=...] [<priority>] [<ring_size>] [outputs=<N>] <address>`
pub fn parse_sweep_account(words: &[String]) -> Result<(u32, SweepArgs), String> {
    let mut args: VecDeque<String> = words.iter().cloned().collect();
    let account = args.pop_front().ok_or("missing account index")?;
    let account = account
        .parse()
        .map_err(|_| format!("failed to parse account index: {account}"))?;
    let indexes = take_index(&mut args, true)?;
    Ok((account, parse_sweep_tail(args, indexes)?))
}

/// `sweep_below <amount_threshold> [index=...] [<priority>] [<ring_size>] <address>`
pub fn parse_sweep_below(words: &[String], unit: Unit) -> Result<(u64, SweepArgs), String> {
    let mut args: VecDeque<String> = words.iter().cloned().collect();
    let threshold = parse_amount(&args.pop_front().ok_or("missing amount threshold")?, unit)?;
    let indexes = take_index(&mut args, false)?;
    Ok((threshold, parse_sweep_tail(args, indexes)?))
}

/// `sweep_single [<priority>] [<ring_size>] [outputs=<N>] <key_image> <address>`
pub fn parse_sweep_single(words: &[String]) -> Result<([u8; 32], SweepArgs), String> {
    let mut args: VecDeque<String> = words.iter().cloned().collect();
    let priority = take_priority(&mut args);
    take_ring_size(&mut args)?;
    let outputs = take_option(&mut args, "outputs=");
    let key_image = parse_key_image(&args.pop_front().ok_or("missing key image")?)?;
    let mut rest: VecDeque<String> = args;
    if let Some(outputs) = outputs {
        rest.push_front(format!("outputs={outputs}"));
    }
    let tail = parse_sweep_tail(rest, None)?;
    Ok((key_image, SweepArgs { priority, ..tail }))
}

pub fn parse_key_image(word: &str) -> Result<[u8; 32], String> {
    hex::decode(word)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| format!("failed to parse key image: {word}"))
}

/// Which `show_transfers`/`export_transfers` rows to include.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryArgs {
    pub incoming: bool,
    pub outgoing: bool,
    pub pending: bool,
    pub pool: bool,
    pub indexes: Option<IndexSelection>,
    pub min_height: u64,
    pub max_height: u64,
    /// `export_transfers`' `output=<path>`.
    pub output: Option<String>,
}

/// `show_transfers [in|out|all|pending|failed|pool|coinbase] [index=<N1>[,<N2>,...]] [<min_height> [<max_height>]]`
/// (and `export_transfers`, which adds `[output=<filepath>]`).
pub fn parse_history(words: &[String]) -> Result<HistoryArgs, String> {
    let mut args: VecDeque<String> = words.iter().cloned().collect();
    let output = take_option(&mut args, "output=");
    let mut kinds = Vec::new();
    while let Some(word) = args.front().filter(|w| {
        [
            "in", "incoming", "out", "outgoing", "all", "pending", "failed", "pool", "coinbase",
        ]
        .contains(&w.as_str())
    }) {
        kinds.push(word.clone());
        args.pop_front();
    }
    let all = kinds.is_empty() || kinds.iter().any(|k| k == "all");
    let has = |names: &[&str]| all || kinds.iter().any(|k| names.contains(&k.as_str()));
    let indexes = take_index(&mut args, false)?;
    let mut height = |what: &str, default: u64| -> Result<u64, String> {
        match args.pop_front() {
            Some(word) => word
                .parse()
                .map_err(|_| format!("bad {what} parameter: {word}")),
            None => Ok(default),
        }
    };
    let min_height = height("min_height", 0)?;
    let max_height = height("max_height", u64::MAX)?;
    if let Some(extra) = args.front() {
        return Err(format!("unexpected argument: {extra}"));
    }
    // `failed` and `coinbase` rows never exist here: failed broadcasts
    // aren't recorded, and these wallets don't mine.
    Ok(HistoryArgs {
        incoming: has(&["in", "incoming"]),
        outgoing: has(&["out", "outgoing"]),
        pending: has(&["pending"]),
        pool: has(&["pool"]),
        indexes,
        min_height,
        max_height,
        output,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PocketchangeArgs {
    pub pieces: usize,
    pub inputs: usize,
    pub priority: Option<u32>,
}

/// `pocketchange [<pieces>] [inputs=<N>] [<priority>]`, in any order.
/// Defaults: the most pieces one transaction holds, from the single
/// largest output.
pub fn parse_pocketchange(words: &[String]) -> Result<PocketchangeArgs, String> {
    let mut parsed = PocketchangeArgs {
        pieces: cli_wallet::MAX_OUTPUTS,
        inputs: 1,
        priority: None,
    };
    for word in words {
        if let Some(n) = word.strip_prefix("inputs=") {
            parsed.inputs =
                n.parse().ok().filter(|n| *n >= 1).ok_or_else(|| {
                    format!("inputs should be a number of outputs, at least 1: {n}")
                })?;
        } else if let Some(priority) = PRIORITY_NAMES.iter().position(|name| name == word) {
            parsed.priority = Some(priority as u32);
        } else {
            parsed.pieces = word
                .parse()
                .ok()
                .filter(|n| (2..=cli_wallet::MAX_OUTPUTS).contains(n))
                .ok_or_else(|| {
                    format!("pieces should be 2 to {}: {word}", cli_wallet::MAX_OUTPUTS)
                })?;
        }
    }
    Ok(parsed)
}

/// `<amount>/<offset>` for `mark_output_spent` and friends. RingCT
/// outputs all have amount 0 there; the offset is the global index.
pub fn parse_output_spec(word: &str) -> Result<u64, String> {
    let (amount, offset) = word
        .split_once('/')
        .ok_or_else(|| format!("expected <amount>/<offset>: {word}"))?;
    if amount != "0" {
        return Err(format!(
            "only RingCT outputs (amount 0) exist in this wallet: {word}"
        ));
    }
    offset
        .parse()
        .map_err(|_| format!("failed to parse offset: {offset}"))
}

/// Splits a prompt line into words the way a shell would for simple
/// cases: whitespace separates, and single or double quotes group.
pub fn split_line(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            (None, c) => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return Err("unterminated quote".to_string());
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        split_line(line).unwrap()
    }

    const ADDR: &str = "5AAAA";

    #[test]
    fn transfer_takes_its_optional_leading_words_in_order() {
        let parsed = parse_transfer(
            &words(&format!(
                "index=0,2 elevated 16 {ADDR} 1.5 {ADDR}x 0.25 subtractfeefrom=1"
            )),
            Unit::Monero,
        )
        .unwrap();
        assert_eq!(parsed.indexes, Some(IndexSelection::Some(vec![0, 2])));
        assert_eq!(parsed.priority, Some(3));
        assert_eq!(
            parsed.destinations,
            vec![
                (ADDR.to_string(), 1_500_000_000_000),
                (format!("{ADDR}x"), 250_000_000_000)
            ]
        );
        assert_eq!(parsed.subtract_fee, SubtractFee::Some(vec![1]));

        let plain = parse_transfer(&words(&format!("{ADDR} 1")), Unit::Monero).unwrap();
        assert_eq!(
            (plain.indexes, plain.priority, plain.subtract_fee),
            (None, None, SubtractFee::None)
        );
        assert_eq!(
            parse_transfer(
                &words(&format!("{ADDR} 1 subtractfeefrom=all")),
                Unit::Monero
            )
            .unwrap()
            .subtract_fee,
            SubtractFee::All
        );
    }

    #[test]
    fn transfer_amounts_follow_the_display_unit() {
        let parsed = parse_transfer(&words(&format!("{ADDR} 250")), Unit::Millinero).unwrap();
        assert_eq!(parsed.destinations[0].1, 250_000_000_000);
    }

    #[test]
    fn transfer_rejects_what_the_reference_wallet_rejects() {
        assert!(
            parse_transfer(&words(&format!("11 {ADDR} 1")), Unit::Monero)
                .unwrap_err()
                .contains("ring size 11")
        );
        assert_eq!(
            parse_transfer(&words(&format!("{ADDR} 1 0123456789abcdef")), Unit::Monero)
                .unwrap_err(),
            OBSOLETE_PAYMENT_ID
        );
        assert!(parse_transfer(&words(ADDR), Unit::Monero).is_err());
        assert!(parse_transfer(&words(&format!("{ADDR} -1")), Unit::Monero).is_err());
        assert!(parse_transfer(&words(&format!("index=all {ADDR} 1")), Unit::Monero).is_err());
    }

    #[test]
    fn payment_uris_carry_their_amount_in_monero() {
        let parsed = parse_transfer(
            &words(&format!("monero:{ADDR}?tx_amount=0.5&recipient_name=shop")),
            Unit::Piconero,
        )
        .unwrap();
        assert_eq!(
            parsed.destinations,
            vec![(ADDR.to_string(), 500_000_000_000)]
        );
        assert!(parse_uri(&format!("monero:{ADDR}")).is_err());
        assert_eq!(
            parse_uri(&format!("monero:{ADDR}?tx_amount=1&tx_payment_id=00")).unwrap_err(),
            OBSOLETE_PAYMENT_ID
        );
    }

    #[test]
    fn sweeps_parse_their_own_leading_arguments() {
        let all = parse_sweep_all(&words(&format!("index=all priority outputs=3 {ADDR}"))).unwrap();
        assert_eq!(
            all,
            SweepArgs {
                indexes: Some(IndexSelection::All),
                priority: Some(4),
                outputs: 3,
                address: ADDR.to_string()
            }
        );

        let (account, account_args) = parse_sweep_account(&words(&format!("2 {ADDR}"))).unwrap();
        assert_eq!((account, account_args.outputs), (2, 1));

        let (threshold, _) =
            parse_sweep_below(&words(&format!("0.01 index=1 {ADDR}")), Unit::Monero).unwrap();
        assert_eq!(threshold, 10_000_000_000);

        let key_image = "11".repeat(32);
        let (image, single) =
            parse_sweep_single(&words(&format!("normal outputs=2 {key_image} {ADDR}"))).unwrap();
        assert_eq!(
            (image, single.priority, single.outputs),
            ([0x11; 32], Some(2), 2)
        );

        assert!(parse_sweep_all(&words(&format!("outputs=0 {ADDR}"))).is_err());
        assert!(parse_sweep_all(&words(&format!("{ADDR} {ADDR}"))).is_err());
    }

    #[test]
    fn history_filters_default_to_everything() {
        let all = parse_history(&[]).unwrap();
        assert!(all.incoming && all.outgoing && all.pending && all.pool);
        assert_eq!((all.min_height, all.max_height), (0, u64::MAX));

        let some = parse_history(&words("in pool index=1 100 200 output=x.csv")).unwrap();
        assert!(some.incoming && some.pool && !some.outgoing && !some.pending);
        assert_eq!(
            (some.min_height, some.max_height, some.output.as_deref()),
            (100, 200, Some("x.csv"))
        );
        assert!(parse_history(&words("in 1 2 3")).is_err());
    }

    #[test]
    fn pocketchange_defaults_to_the_biggest_split_of_the_biggest_output() {
        assert_eq!(
            parse_pocketchange(&[]),
            Ok(PocketchangeArgs {
                pieces: 16,
                inputs: 1,
                priority: None
            })
        );
        assert_eq!(
            parse_pocketchange(&words("inputs=3 8 normal")),
            Ok(PocketchangeArgs {
                pieces: 8,
                inputs: 3,
                priority: Some(2)
            })
        );
        for bad in ["1", "17", "inputs=0", "lots"] {
            assert!(parse_pocketchange(&words(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn output_specs_are_ringct_amount_and_global_index() {
        assert_eq!(parse_output_spec("0/12345"), Ok(12345));
        assert!(parse_output_spec("5/1").is_err());
        assert!(parse_output_spec("12345").is_err());
    }

    #[test]
    fn prompt_lines_split_like_a_shell_for_simple_quoting() {
        assert_eq!(
            words(r#"address new "my shop"  x"#),
            ["address", "new", "my shop", "x"]
        );
        assert_eq!(
            words("set_description 'a b' c"),
            ["set_description", "a b", "c"]
        );
        assert_eq!(words(r#"set_tx_note abc """#), ["set_tx_note", "abc", ""]);
        assert!(split_line(r#"say "unfinished"#).is_err());
    }
}
