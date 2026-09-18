//! Request/response envelope and message framing for the `KeyCustody` socket
//! protocol (WBS 2.1.2).
//!
//! `lib.rs` (WBS 2.1.1) settled the wire *shape* of every `KeyCustody` method's
//! arguments and result but deliberately didn't decide how those shapes travel
//! over an actual socket - a Unix `SOCK_STREAM` socket is a plain byte stream
//! with no message boundaries of its own, so two things are still needed before
//! any of those DTOs can cross one for real: a way to say *which* of the six
//! methods a given request is for, and a way to know where one message ends and
//! the next begins.
//!
//! **Envelope**: [`KeyCustodyRequest`]/[`KeyCustodyResponse`] are plain enums,
//! one variant per trait method, each wrapping that method's existing
//! `{Name}Request` struct or `{Name}Response` type alias from `lib.rs` verbatim
//! - no new per-method wire shape is invented here, only a tag saying which one
//! applies. `serde`'s default (externally-tagged) enum representation gives each
//! encoded message a `{"RegisterWallet": {...}}`-shaped outer key, which is
//! exactly the dispatch tag [`crate::server::dispatch`] and
//! [`crate::client::SocketKeyCustody`] both need and costs nothing extra to add.
//!
//! **Framing**: a 4-byte big-endian `u32` length prefix followed by that many
//! bytes of `serde_json`-encoded payload, the same framing in both directions.
//! Big-endian because that's "network byte order" by convention (nothing here
//! depends on it beyond readability in a hex dump); `serde_json` because this
//! whole workspace already depends on it pervasively (every HTTP handler in the
//! engine and monokulo crates encodes/decodes JSON) and because the wire
//! DTOs it's encoding are already designed to be plain, human-inspectable hex
//! strings and small structs - there's no performance-sensitive hot path here
//! (`KeyCustody` calls are bounded by the same scalar-multiplication costs
//! `src/key_custody/plain.rs` already documents, not by serialization), so a
//! faster binary format would add a second encoding convention to this codebase
//! for no measurable benefit. A length prefix (rather than e.g. newline-
//! delimited JSON) is needed because a `TransactionWire`'s hex string is
//! arbitrary-length binary-derived text with no character `serde_json` promises
//! never to emit, so scanning for a delimiter byte in the payload itself isn't
//! actually safe the way it would be for a strictly-controlled request format.
//!
//! [`MAX_FRAME_BYTES`] bounds how large a single frame's declared length is
//! allowed to be, checked *before* attempting to allocate a buffer for it - a
//! corrupted or hostile 4-byte prefix can claim up to 4 GiB, and allocating that
//! much per connection on nothing more than an attacker's say-so is exactly the
//! kind of unbounded-resource-consumption bug `src/key_custody/plain.rs`'s own
//! `MAX_SCAN_TABLE_ENTRIES` guards against for scan ranges - same shape of
//! problem, same "refuse loudly, up front" answer, applied here to the framing
//! layer instead of the scan-table layer.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{
    DeriveSubaddressRequest, DeriveSubaddressResponse, RegisterWalletRequest,
    RegisterWalletResponse, RemoveWalletRequest, RemoveWalletResponse, ScanTxOutputsRequest,
    ScanTxOutputsResponse, SealRequest, SealResponse, UnsealAndRegisterRequest,
    UnsealAndRegisterResponse,
};

/// One request per `KeyCustody` trait method, in the same order the trait
/// declares them (matching `lib.rs`'s own per-method DTO section).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KeyCustodyRequest {
    RegisterWallet(RegisterWalletRequest),
    RemoveWallet(RemoveWalletRequest),
    Seal(SealRequest),
    UnsealAndRegister(UnsealAndRegisterRequest),
    DeriveSubaddress(DeriveSubaddressRequest),
    ScanTxOutputs(ScanTxOutputsRequest),
}

/// The matching response envelope. Each variant is already a `Result<TWire,
/// KeyCustodyErrorWire>` (the `{Name}Response` type aliases from `lib.rs`) - a
/// *successful* dispatch of a well-formed request always produces the variant
/// matching the request it answers; see [`crate::server::dispatch`] for the one
/// case (a request whose own fields don't decode into real types) that doesn't
/// make it this far at all.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KeyCustodyResponse {
    RegisterWallet(RegisterWalletResponse),
    RemoveWallet(RemoveWalletResponse),
    Seal(SealResponse),
    UnsealAndRegister(UnsealAndRegisterResponse),
    DeriveSubaddress(DeriveSubaddressResponse),
    ScanTxOutputs(ScanTxOutputsResponse),
}

/// Ceiling on one frame's declared payload length. Generous relative to
/// anything this protocol legitimately sends - a `ScanTxOutputsRequest` carries
/// one transaction's consensus-encoded bytes as hex, and even a large,
/// many-output transaction is a few hundred KB at most - while still ruling out
/// a multi-gigabyte allocation from a single 4-byte length prefix. See the
/// module doc comment for the full reasoning.
pub const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

/// Failure reading or writing one frame. Deliberately distinct from
/// [`crate::WireConversionError`]: that type is about a *DTO's own fields*
/// failing to convert back into a real engine type once a message has already
/// been decoded; this one is about the transport - a broken connection, a
/// corrupted or hostile length prefix, or bytes that aren't even valid JSON for
/// the envelope shape being expected. Both sides treat every variant here the
/// same way: log and close the connection, never panic and never guess at a
/// recovery.
#[derive(Debug, thiserror::Error)]
pub enum FramingError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame of {0} bytes exceeds the {MAX_FRAME_BYTES}-byte limit")]
    TooLarge(usize),
    #[error("failed to encode payload as json: {0}")]
    Encode(serde_json::Error),
    #[error("failed to decode payload as json: {0}")]
    Decode(serde_json::Error),
}

/// Encode `value` as `serde_json` and write it as one length-prefixed frame.
/// Flushes before returning, since the caller (both `server.rs`'s per-connection
/// loop and `client.rs`'s `SocketKeyCustody::call`) always expects the peer to
/// actually see these bytes before it goes on to wait for a reply.
pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), FramingError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = serde_json::to_vec(value).map_err(FramingError::Encode)?;
    let len = u32::try_from(payload.len()).map_err(|_| FramingError::TooLarge(payload.len()))?;
    if len > MAX_FRAME_BYTES {
        return Err(FramingError::TooLarge(payload.len()));
    }
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one length-prefixed, `serde_json`-encoded frame.
///
/// Returns `Ok(None)` only for a *clean* close: the peer shut the connection
/// down without sending a single byte of a new frame's length prefix, which is
/// the ordinary way a connection ends between requests (the client is done, or
/// the whole process is exiting) rather than a fault. Anything else - a
/// partial length prefix, a length that exceeds [`MAX_FRAME_BYTES`], a short
/// read while collecting the payload, or a payload that isn't valid JSON for
/// `T` - is a real `Err`, because by that point the peer had already started a
/// message it didn't finish or sent something this side can't make sense of;
/// collapsing that into the same "nothing more is coming" case as a clean EOF
/// would hide a genuinely broken peer behind an ordinary-looking disconnect.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>, FramingError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len_buf = [0u8; 4];
    let mut read_so_far = 0usize;
    while read_so_far < len_buf.len() {
        match reader.read(&mut len_buf[read_so_far..]).await? {
            0 if read_so_far == 0 => return Ok(None), // clean EOF between frames
            0 => {
                return Err(FramingError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed mid-length-prefix",
                )))
            }
            n => read_so_far += n,
        }
    }
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME_BYTES {
        return Err(FramingError::TooLarge(len as usize));
    }
    let mut payload = vec![0u8; len as usize];
    reader.read_exact(&mut payload).await?;
    serde_json::from_slice(&payload).map_err(FramingError::Decode).map(Some)
}
