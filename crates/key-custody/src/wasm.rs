//! What the browser calls: check a bundle and encrypt keys to it, in one call
//! (`seal`), JSON in and JSON out through linear memory. No wasm-bindgen: the
//! page's script (`monokulo`'s `static/key-custody.js`) is a few lines, and
//! the module needs nothing from the page but random bytes (`env.fill_random`,
//! from `crypto.getRandomValues`).
//!
//! Input: `{"bundle": {...}, "view_key": hex, "spend_public_key": hex,
//! "id_key_digest": hex or null, "min_guest_svn": n, "now": unix seconds}`.
//! An `id_key_digest` of null means the official one built in.
//!
//! Output: `{"envelope": "<text to submit>", "measurement": hex,
//! "guest_svn": n}` or `{"error": "<what went wrong>"}`.

use std::cell::RefCell;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::transport::{self, Anchor, Bundle, TrustPolicy, KEYS_LEN};

#[link(wasm_import_module = "env")]
extern "C" {
    fn fill_random(ptr: *mut u8, len: usize);
}

/// `getrandom`'s source of bytes on this target (`.cargo/config.toml` selects
/// the custom backend): the page's `crypto.getRandomValues`.
///
/// # Safety
/// `dest` points at `len` writable bytes, as `getrandom` guarantees.
#[no_mangle]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    // SAFETY: the page writes exactly `len` bytes at `dest`.
    unsafe { fill_random(dest, len) };
    Ok(())
}

thread_local! {
    static OUTPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Room for `len` bytes of input, for the page to write into.
#[no_mangle]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    let mut buffer = Vec::<u8>::with_capacity(len);
    let ptr = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    ptr
}

/// Frees what `alloc` gave, wiping it first (it held keys).
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

#[derive(Deserialize)]
struct Input {
    bundle: Bundle,
    view_key: String,
    spend_public_key: String,
    id_key_digest: Option<String>,
    #[serde(default)]
    min_guest_svn: u32,
    now: i64,
}

impl Drop for Input {
    fn drop(&mut self) {
        use zeroize::Zeroize as _;
        self.view_key.zeroize();
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum Output {
    Sealed {
        envelope: String,
        measurement: String,
        guest_svn: u32,
    },
    Failed {
        error: String,
    },
}

fn key(what: &str, text: &str) -> Result<[u8; 32], String> {
    let bytes =
        Zeroizing::new(hex::decode(text.trim()).map_err(|_| format!("the {what} is not hex"))?);
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("the {what} is 64 hex characters"))
}

fn seal_input(input: &Input) -> Result<Output, String> {
    transport::check_version(input.bundle.v).map_err(|e| e.to_string())?;
    let id_key_digest = match &input.id_key_digest {
        Some(text) => transport::parse_id_key_digest(text).map_err(|e| e.to_string())?,
        None => transport::official_id_key_digest().ok_or(
            "this build has no official engine ID key; the operator has to say which one to trust",
        )?,
    };
    let policy = TrustPolicy {
        id_key_digest,
        min_guest_svn: input.min_guest_svn,
    };
    let verified = transport::verify_bundle(&input.bundle, &policy, &Anchor::Amd, input.now)
        .map_err(|e| e.to_string())?;
    let mut keys = Zeroizing::new([0u8; KEYS_LEN]);
    keys[..32].copy_from_slice(&key("private view key", &input.view_key)?);
    keys[32..].copy_from_slice(&key("public spend key", &input.spend_public_key)?);
    let envelope = transport::seal(&verified, keys.as_slice()).map_err(|e| e.to_string())?;
    Ok(Output::Sealed {
        envelope: envelope.to_text(),
        measurement: hex::encode(verified.measurement),
        guest_svn: verified.guest_svn,
    })
}

/// Checks the bundle and encrypts the keys (see the module docs). Reads
/// `len` bytes of JSON at `ptr` (which the page then frees with `dealloc`)
/// and returns the output's length; the output is at `output_ptr`.
///
/// # Safety
/// `ptr` points at `len` initialized bytes.
#[no_mangle]
pub unsafe extern "C" fn seal(ptr: *const u8, len: usize) -> usize {
    // SAFETY: as the function's contract says.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    let output = match serde_json::from_slice::<Input>(bytes) {
        Ok(input) => seal_input(&input).unwrap_or_else(|error| Output::Failed { error }),
        Err(e) => Output::Failed {
            error: format!("the key entry form sent something unexpected: {e}"),
        },
    };
    let json = serde_json::to_vec(&output).unwrap_or_default();
    OUTPUT.with(|out| {
        *out.borrow_mut() = json;
        out.borrow().len()
    })
}
