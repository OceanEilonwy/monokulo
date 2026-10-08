//! What the browser calls, JSON in and JSON out through linear memory, like
//! `key-custody`'s module: no wasm-bindgen, the page's script
//! (`monokulo`'s `static/wallet-setup.js`) is short. The module needs nothing
//! from the page but what it is given: the randomness comes in with the
//! request (`crypto.getRandomValues`), so there is no random source to wire.
//!
//! `generate`: `{"entropy": hex (32 bytes), "birthday": unix seconds,
//! "network": "mainnet" | "stagenet" | "testnet"}` →
//! `{"phrase", "legacy_phrase", "birthday", "address", "view_key",
//! "spend_public_key"}`.
//!
//! `qr`: the text to encode, as UTF-8 → `{"svg"}`.
//!
//! Either fails as `{"error": "<what went wrong>"}`.

use std::cell::RefCell;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{generate as make_wallet, qr_svg, Network, NewWallet};

thread_local! {
    static OUTPUT: RefCell<Zeroizing<Vec<u8>>> = RefCell::new(Zeroizing::new(Vec::new()));
}

/// Room for `len` bytes of input, for the page to write into.
#[no_mangle]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    let mut buffer = Vec::<u8>::with_capacity(len);
    let ptr = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    ptr
}

/// Frees what `alloc` gave, wiping it first (it held randomness).
///
/// # Safety
/// `ptr` and `len` are exactly what `alloc` was given and returned.
#[no_mangle]
pub unsafe extern "C" fn dealloc(ptr: *mut u8, len: usize) {
    // SAFETY: as the function's contract says.
    let buffer = unsafe { Vec::from_raw_parts(ptr, len, len) };
    drop(Zeroizing::new(buffer));
}

/// Where the last output is; `output_len` says how long it is.
#[no_mangle]
pub extern "C" fn output_ptr() -> *const u8 {
    OUTPUT.with(|out| out.borrow().as_ptr())
}

#[no_mangle]
pub extern "C" fn output_len() -> usize {
    OUTPUT.with(|out| out.borrow().len())
}

/// Wipes the last output (the page calls it once it has read a wallet out).
#[no_mangle]
pub extern "C" fn clear_output() {
    OUTPUT.with(|out| *out.borrow_mut() = Zeroizing::new(Vec::new()));
}

#[derive(Deserialize)]
struct GenerateInput {
    entropy: String,
    birthday: u64,
    network: String,
}

impl Drop for GenerateInput {
    fn drop(&mut self) {
        use zeroize::Zeroize as _;
        self.entropy.zeroize();
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum Output<'a> {
    Wallet {
        phrase: &'a str,
        legacy_phrase: &'a str,
        birthday: u64,
        address: &'a str,
        view_key: &'a str,
        spend_public_key: &'a str,
    },
    Qr {
        svg: &'a str,
    },
    Failed {
        error: &'a str,
    },
}

fn generate_from(bytes: &[u8]) -> Result<NewWallet, String> {
    let input = serde_json::from_slice::<GenerateInput>(bytes)
        .map_err(|e| format!("the page sent something unexpected: {e}"))?;
    let decoded = Zeroizing::new(
        hex::decode(input.entropy.trim()).map_err(|_| "the randomness is not hex".to_owned())?,
    );
    let entropy: [u8; 32] = decoded
        .as_slice()
        .try_into()
        .map_err(|_| "the randomness must be 32 bytes".to_owned())?;
    let network = Network::parse(&input.network)
        .ok_or_else(|| format!("unknown network {:?}", input.network))?;
    make_wallet(entropy, input.birthday, network).map_err(|e| e.to_string())
}

fn respond(output: &Output<'_>) -> usize {
    let json = Zeroizing::new(serde_json::to_vec(output).unwrap_or_default());
    OUTPUT.with(|out| {
        *out.borrow_mut() = json;
        out.borrow().len()
    })
}

/// Makes a wallet (see the module docs). Reads `len` bytes of JSON at `ptr`
/// (which the page then frees with `dealloc`) and returns the output's
/// length; the output is at `output_ptr`.
///
/// # Safety
/// `ptr` points at `len` initialized bytes.
#[no_mangle]
pub unsafe extern "C" fn generate(ptr: *const u8, len: usize) -> usize {
    // SAFETY: as the function's contract says.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    match generate_from(bytes) {
        Ok(wallet) => respond(&Output::Wallet {
            phrase: &wallet.phrase,
            legacy_phrase: &wallet.legacy_phrase,
            birthday: wallet.birthday,
            address: &wallet.address,
            view_key: &wallet.view_key_hex,
            spend_public_key: &wallet.spend_pubkey_hex,
        }),
        Err(error) => respond(&Output::Failed { error: &error }),
    }
}

/// Draws `len` bytes of UTF-8 at `ptr` as a QR code (see the module docs).
///
/// # Safety
/// `ptr` points at `len` initialized bytes.
#[no_mangle]
pub unsafe extern "C" fn qr(ptr: *const u8, len: usize) -> usize {
    // SAFETY: as the function's contract says.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    match std::str::from_utf8(bytes).map_err(|_| "the text is not UTF-8".to_owned()) {
        Ok(text) => match qr_svg(text) {
            Ok(svg) => respond(&Output::Qr { svg: &svg }),
            Err(e) => respond(&Output::Failed {
                error: &e.to_string(),
            }),
        },
        Err(error) => respond(&Output::Failed { error: &error }),
    }
}
