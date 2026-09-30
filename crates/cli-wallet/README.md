# cli-wallet

A fast, stagenet-only test wallet: the library the real-stagenet e2e suites
pay orders with, and `stagenet-wallet-cli`, which speaks
[`monero-wallet-cli`](https://docs.getmonero.org/interacting/monero-wallet-cli-reference/)'s
commands over the same wallets. See `src/lib.rs` for why it never scans the
chain, and `e2e/README.md` for how the e2e suites use it.

## One file per wallet

Each wallet is one JSON file (`e2e/wallets/<name>.json`) holding
everything about it:

| Field | What it is |
|---|---|
| `version`, `network`, `address`, `private_spend_key`, `private_view_key`, `mnemonic` | Keys, and the seed they came from |
| `description`, `accounts`, `current_account`, `tag_descriptions`, `address_book`, `settings` | What `monero-wallet-cli` keeps in its wallet file (only written once set) |
| `outputs` | Every output received, each fully serialized so it's read back with no RPC |
| `pending` | Txids expected to pay this wallet that haven't confirmed |
| `sent` | Every transaction this wallet broadcast: destinations, fee, change |
| `tx_notes` | `set_tx_note` notes |
| `extra` | Anything else recorded about the wallet, kept as-is |

Every change is a read-modify-write under an exclusive lock on
`<name>.json.lock`, written atomically, so parallel runs never corrupt
the file or lose one another's updates. A transfer holds the lock from input
selection until it's recorded, so two senders can't pick the same output.

## The CLI

```sh
cargo run -p cli-wallet --bin stagenet-wallet-cli -- --wallet-file spender          # prompt
cargo run -p cli-wallet --bin stagenet-wallet-cli -- --wallet-file spender balance  # one command
```

With no command it opens the wallet and prompts (`[wallet 5648a3]: `)
until `exit`, keeping one node connection for the session; commands that
don't need a node (addresses, keys, labels, address book, notes, settings)
never open one. Amounts are in XMR unless `set unit` says otherwise.
`transfer` and the sweeps ask "Is this okay?" at a terminal (not when input
is piped, and not after `set always-confirm-transfers 0`).

### At the prompt

At a terminal the prompt is a full line editor
([reedline](https://github.com/nushell/reedline)); piped input is read
plainly, so scripts behave the same as ever.

| Keys | Does |
|---|---|
| Tab / Shift+Tab | Completion menu, with a description beside each candidate: commands, their sub-words and options, and the wallet's own txids, key images (with amounts), outputs, address book and accounts |
| → (at the end of the line) | Accept the grey hint: the rest of a matching history entry, or of a command name |
| ↑ / ↓, Ctrl+R | History (kept across sessions in `~/.local/state/stagenet-wallet-cli/history`; start a line with a space to keep it out) and reverse search |
| Alt+← / Alt+→ (or Ctrl) | Jump a word |
| Shift+arrows, Alt+Shift+← / →, Shift+Home / End, Alt+A | Select a character, a word, to either end, everything; typing or Backspace replaces the selection |
| Alt+Backspace / Alt+Delete, Ctrl+W | Delete a word |
| Ctrl+Shift+X / C / V | Cut, copy, paste with the system clipboard |
| Ctrl+A / Ctrl+E, Ctrl+U / Ctrl+K | Line start / end, delete to start / end (Emacs keys) |
| Ctrl+C / Ctrl+D | Clear the line / leave |

Once a command is typed, its usage shows in grey italics after the cursor,
narrowed to the forms that fit what's typed (`account switch ` shows
`account switch <index>`). The command word is green when it's a known
command, red when not; the current account shows on the right.

Mouse: clicking in the line moves the cursor in terminals that send
shell-integration click events (kitty, WezTerm, Ghostty). Dragging selects
and copies with the terminal's own selection, everywhere; a line editor
can't take over drag-selection without also taking over the terminal's
scrollback and copy.

If another process has the wallet file locked (another prompt mid-transfer,
an e2e run), the prompt says who and asks `[R]etry, [w]ait for it,
[c]ancel?`. Scripts and the e2e suites print a warning and wait. A lock
never outlives the process holding it, even if it crashes.

### What's supported

The limits applied: a JSON file per wallet as the only wallet state, and no
chain scanning. Single node calls that aren't scans are fine (chain height,
fee rate, broadcast, one transaction's block, whether key images are spent).
Stagenet only; ring size 16.

#### Help and status
| Command | Supported | Notes |
|---|---|---|
| `help`, `help <command>`, `version` | Yes | |
| `status` | Yes | "synced" means nothing is pending |
| `fee` | Partly | Fee per byte at each priority; no mempool backlog |
| `wallet_info`, `bc_height` | Yes | |

#### Balance, accounts, addresses
| Command | Supported | Notes |
|---|---|---|
| `balance [detail]` | Yes | Unconfirmed change counts towards balance, as in the reference wallet |
| `refresh` | Yes | Resolves pending txids: one block lookup each |
| `account`, `account new/switch/label/tag/untag/tag_description` | Yes | |
| `address`, `address all/new/label/<min> [<max>]/one-off` | Yes | Created subaddresses are watched for incoming payments |
| `address device` | No | No hardware wallet |
| `integrated_address`, `payment_id`, `payments` | Yes | |

#### Sending
| Command | Supported | Notes |
|---|---|---|
| `transfer` | Yes | `index=`, priority, several destinations, `monero:` URIs, `subtractfeefrom=`. Ring size must be 16; standalone payment IDs are refused, as in the reference wallet |
| `sweep_all`, `sweep_account`, `sweep_below`, `sweep_single` | Yes | `sweep_single` takes a key image |
| `--do-not-relay` | Yes | Signed transaction written to `raw_monero_tx`; the wallet file is untouched |
| `locked_transfer`, `locked_sweep_all` | No | monero-wallet always builds transactions with no unlock time |
| `sweep_unmixable` | No | No pre-RingCT outputs exist on stagenet |

#### History
| Command | Supported | Notes |
|---|---|---|
| `show_transfers`, `show_transfer`, `export_transfers` | Yes | `out` rows exist for sends made after the move to wallet files; `failed` and `coinbase` are always empty. Outputs resolved before then have no timestamp |
| `incoming_transfers [available\|unavailable] [verbose] [index=]` | Yes | `uses` needs a chain scan |
| `unspent_outputs` | Yes | |
| `set_tx_note`, `get_tx_note` | Yes | |

#### Outputs and key images
| Command | Supported | Notes |
|---|---|---|
| `freeze`, `thaw`, `frozen` | Yes | Key images are computed locally |
| `mark_output_spent`, `mark_output_unspent`, `is_output_spent` | Yes | `0/<global index>` |
| `rescan_spent` | Yes | One `is_key_image_spent` call |
| `export_outputs`, `import_outputs`, `export_key_images`, `import_key_images` | No | wallet2's binary signed file format |
| `rescan_bc` | No | It's a chain scan |
| `print_ring`, `set_ring`, blackball | No | Rings aren't stored |

#### Keys
| Command | Supported | Notes |
|---|---|---|
| `seed`, `spendkey`, `viewkey` | Yes | `seed` derives the 25-word seed when none is recorded |
| `encrypted_seed` | No | Needs cn_slow_hash |
| `password` | No | Wallet files are plaintext by design (stagenet only) |

#### Proofs and transaction keys
| Command | Supported | Notes |
|---|---|---|
| `get_tx_key`, `check_tx_key`, `set_tx_key`, `get_tx_proof`, `check_tx_proof` | No | monero-wallet keeps transaction keys crate-private |
| `get_spend_proof`, `get_reserve_proof` and their checks | No | Signature schemes monero-wallet doesn't provide |
| `sign`, `verify` | No | Possible, but left out |

#### Everything else
| Command | Supported | Notes |
|---|---|---|
| `address_book`, `set_description`, `get_description` | Yes | |
| `set` | Partly | `priority`, `unit`, `always-confirm-transfers`, `default-ring-size` (16 only) |
| `save` | Yes | A no-op: every change is saved as it happens |
| Multisig, hardware wallets, mining, `donate`, `net_stats`, `public_nodes` | No | |

#### Startup flags
| Flag | Supported | Notes |
|---|---|---|
| `--wallet-file`, `--daemon-address`, `--stagenet`, `--do-not-relay` | Yes | `--wallet-file` takes a name in `--wallet-dir` or a path |
| `--generate-new-wallet`, `--restore-deterministic-wallet`, `--electrum-seed`, `--generate-from-spend-key`, `--mnemonic-language` | Yes | Restores 16-word Polyseed and 25-word seeds |
| `--generate-from-view-key` | No | Every wallet here holds its spend key |
| `--testnet`, mainnet, `--password*`, `--restore-height`, logging, `--generate-from-device` | No | |

Not in the reference wallet:

- `pocketchange [<pieces>] [inputs=<N>] [<priority>]` splits the largest
  unlocked output (or the `N` largest, merged) into equal outputs of the
  account's own - by default 16, the most one transaction can hold, with
  the change output as one of them. It keeps the e2e suites supplied with
  enough separate mature outputs to run fast; see `e2e/README.md`, "Keeping
  enough outputs". (`split` still works as an alias.)
- `add_output <txid>` records a payment received, e.g. from the faucet.
- `completions <shell>` prints a shell completion script.
