//! The bodies of the engine's admin API and its `/status` report, defined
//! once: the engine builds and reads them, and monokulo's client sends and
//! parses the same types, so a field can't be renamed, retyped or dropped
//! on one side only.
//!
//! Requests are what monokulo sends: a field left `None` is sent as it
//! always was (left out, or `null`, which the engine reads the same), and
//! the engine ignores fields it doesn't know. Responses are what the engine
//! sends: a field an older engine doesn't send is `#[serde(default)]`.

use serde::{Deserialize, Serialize};

use crate::amount::Piconero;
use crate::announcements::Announcements;
use crate::custody::{CustodyBackend, RequestedBackend};
use crate::ids::{EngineWalletId, OrderId, TenantId};
use crate::order_status::OrderStatus;
use crate::proof::ProofStatus;
use crate::scaling::{EndpointStats, LinkSnapshot, NetworkScaling};

// -- Keys --------------------------------------------------------------------

/// A store's keys as a request carries them: in the clear (for a backend
/// that takes them that way, `plain`), or encrypted to the backend
/// (`encrypted_keys`, an envelope from `key-custody-cli` or the key entry
/// form, for `snp`), never both ([`StoreKeys::read`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreKeys {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub view_key_hex: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub spend_pubkey_hex: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_keys: Option<String>,
}

/// The keys a request gave, in the one form it gave them, as given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keys<'a> {
    /// In the clear, as hex (either may be blank: the backend says what is
    /// missing).
    Plain {
        view_key_hex: &'a str,
        spend_pubkey_hex: &'a str,
    },
    /// Encrypted to the backend.
    Encrypted(&'a str),
}

impl Keys<'_> {
    /// Whether nothing was given at all.
    pub fn is_none(&self) -> bool {
        match self {
            Keys::Plain {
                view_key_hex,
                spend_pubkey_hex,
            } => view_key_hex.trim().is_empty() && spend_pubkey_hex.trim().is_empty(),
            Keys::Encrypted(_) => false,
        }
    }
}

impl StoreKeys {
    /// The keys in the one form they came in, blank fields counting as
    /// absent. Keys in the clear beside encrypted ones are refused: one of
    /// the two would be ignored.
    pub fn read(&self) -> Result<Keys<'_>, &'static str> {
        let plain = Keys::Plain {
            view_key_hex: &self.view_key_hex,
            spend_pubkey_hex: &self.spend_pubkey_hex,
        };
        match self
            .encrypted_keys
            .as_deref()
            .filter(|e| !e.trim().is_empty())
        {
            None => Ok(plain),
            Some(encrypted) if plain.is_none() => Ok(Keys::Encrypted(encrypted)),
            Some(_) => Err(
                "give a store's keys either in the clear (view_key_hex, spend_pubkey_hex) or encrypted (encrypted_keys), not both",
            ),
        }
    }
}

// -- Stores and wallets ------------------------------------------------------

/// `POST /api/v1/admin/tenants`: a new store, on new keys or on an existing
/// wallet (`wallet_id`, instead of the keys). There is no origin list:
/// embedding policy is monokulo's alone.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateTenantRequest {
    #[serde(flatten)]
    pub keys: StoreKeys,
    /// An existing wallet the new store takes payments into
    /// (`POST /api/v1/admin/wallets`), instead of `keys`: its keys are not
    /// sent again, and it shares the wallet's address counter with the
    /// wallet's other stores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet_id: Option<EngineWalletId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default)]
    pub confirmations_required: Option<u64>,
    #[serde(default)]
    pub order_expiry_seconds: Option<i64>,
    /// Which key custody backend holds the store's keys; the engine's
    /// default when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_custody_backend: Option<RequestedBackend>,
}

/// The new store, with its secret key (`sk_...`), handed out this once.
/// `Secret` is what each side keeps it in: the text the engine writes, and
/// monokulo's own token type (`credentials::auth::RawToken`, never shown in
/// logs), which it reads it straight into.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTenantResponse<Secret = String> {
    pub tenant_id: TenantId,
    pub public_key: String,
    pub secret_token: Secret,
}

/// `POST /api/v1/admin/tenant/rotate-secret`: the store's new secret key,
/// kept as in [`CreateTenantResponse`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotateSecretResponse<Secret = String> {
    pub secret_token: Secret,
}

/// `POST /api/v1/admin/wallets`: one wallet's keys, registered once, for any
/// number of stores to take payments into. Keys come in as for a store.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateWalletRequest {
    #[serde(flatten)]
    pub keys: StoreKeys,
    #[serde(default)]
    pub network: Option<String>,
    /// The engine's default when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_custody_backend: Option<RequestedBackend>,
}

/// `POST /api/v1/admin/wallets/{id}/restore`: a retired wallet back, with
/// its keys entered again.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RestoreWalletRequest {
    #[serde(flatten)]
    pub keys: StoreKeys,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_custody_backend: Option<RequestedBackend>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletView {
    pub wallet_id: EngineWalletId,
    pub primary_address: String,
    pub network: String,
    pub key_custody_backend: CustodyBackend,
}

/// `GET /api/v1/admin/wallets/{id}`: the wallet, and what its retirement
/// would wait for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletStatusView {
    pub wallet_id: EngineWalletId,
    pub primary_address: String,
    pub network: String,
    pub key_custody_backend: CustodyBackend,
    /// When it was retired; its keys were deleted then.
    pub retired_at: Option<i64>,
    /// Enabled stores taking payments into it.
    pub stores: u64,
    /// Orders on it that can still be paid.
    pub payable_orders: u64,
    /// The latest an order on it could still be paid, as far as the
    /// engine can tell.
    pub payable_until: Option<i64>,
    /// Rows scanning it still behind the chain: a block not yet read for
    /// them could hold a payment to an order on it.
    pub scans_behind: u64,
}

/// `POST /api/v1/admin/wallets/{id}/retire`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetiredView {
    pub retired_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantView {
    pub tenant_id: TenantId,
    pub public_key: String,
    /// The engine's wallet this store takes payments into. Always sent;
    /// absent only from an engine older than shared wallets.
    #[serde(default)]
    pub wallet_id: Option<EngineWalletId>,
    pub primary_address: String,
    pub network: String,
    pub confirmations_required: u64,
    pub order_expiry_seconds: i64,
    /// Which key custody backend holds this store's keys. Always sent;
    /// absent only from an engine older than per-store backends.
    #[serde(default)]
    pub key_custody_backend: Option<CustodyBackend>,
}

/// `PATCH /api/v1/admin/tenant`: each field left `None` stays as it is.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatchTenantRequest {
    #[serde(default)]
    pub confirmations_required: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_expiry_seconds: Option<i64>,
}

/// `PUT /api/v1/admin/tenant/key-custody`: the store's keys again, for
/// another backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchKeyCustodyRequest {
    pub backend: RequestedBackend,
    #[serde(flatten)]
    pub keys: StoreKeys,
}

/// `POST /api/v1/admin/key-custody/bundle` and its move form.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeyBundleRequest {
    /// The backend the keys are for; the default when absent.
    #[serde(default)]
    pub backend: Option<RequestedBackend>,
}

/// The bundle a merchant's client checks and encrypts their keys against,
/// passed on to the browser or `key-custody-cli` as it is
/// (`key_custody::transport::Bundle`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyBundleResponse {
    pub bundle: serde_json::Value,
}

/// `PUT /api/v1/admin/tenant/wallet`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeWalletRequest {
    pub wallet_id: EngineWalletId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletChangeView {
    pub tenant: TenantView,
    /// The store's orders on the wallet it left, still watched with that
    /// wallet's keys.
    pub orders_on_previous_wallet: u64,
}

// -- Orders --------------------------------------------------------------------

/// The longest `idempotency_key` the engine takes.
pub const MAX_IDEMPOTENCY_KEY_CHARS: usize = 128;

/// `POST /api/v1/admin/tenant/orders`. XMR only: any fiat price is
/// monokulo's, already turned into `xmr_amount_piconero`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateOrderRequest {
    pub xmr_amount_piconero: Piconero,
    #[serde(default)]
    pub merchant_order_id: Option<String>,
    /// What the order is for, shown to the payer on monokulo's checkout;
    /// kept as given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A per-order confirmation count, locked in at creation (monokulo's
    /// amount-tiered thresholds); the store's own applies when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmations_required: Option<u64>,
    /// The caller's key for this one purchase: a request repeating it gets
    /// the order already made with it, never a second one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateOrderResponse {
    pub order_id: OrderId,
    pub address: String,
    pub xmr_amount_piconero: Piconero,
    pub expires_at: i64,
}

/// One order, as every order route gives it. No fiat: monokulo keeps any
/// fiat price of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderView {
    pub order_id: OrderId,
    pub merchant_order_id: Option<String>,
    /// What the order is for, as it was created; `None` when it was given
    /// none.
    #[serde(default)]
    pub description: Option<String>,
    pub address: String,
    pub xmr_amount_piconero: Piconero,
    pub amount_received_piconero: Piconero,
    pub status: OrderStatus,
    pub confirmations: u64,
    pub double_spend_detected_at: Option<i64>,
    /// When the merchant cancelled it (the POS's Cancel), if it was.
    pub cancelled_at: Option<i64>,
    pub refund_address: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    pub updated_at: i64,
    /// The block range scanned for this order so far.
    pub first_scanned_height: Option<i64>,
    pub last_scanned_height: Option<i64>,
    /// Whether anything still examines the order: the live scanner (open,
    /// or closed within its grace window) or a rescan.
    pub currently_scanning: bool,
}

impl OrderView {
    /// Whether whatever shows the order may stop following it. Paid and
    /// overpaid are final. Expired and cancelled are only once the engine
    /// stops watching the order (`currently_scanning`): until then a
    /// payment that arrives late is still found, and turns the order paid.
    pub fn is_final(&self) -> bool {
        match self.status {
            OrderStatus::Paid | OrderStatus::Overpaid => true,
            OrderStatus::Expired | OrderStatus::Cancelled => !self.currently_scanning,
            OrderStatus::Pending
            | OrderStatus::Unconfirmed
            | OrderStatus::Confirming
            | OrderStatus::Partial => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentView {
    pub txid: String,
    pub output_index: i64,
    pub amount_piconero: Piconero,
    pub first_seen_at: i64,
    pub block_height: Option<i64>,
    pub voided_at: Option<i64>,
    /// Voided because another payment of the same output (its one-time
    /// key) is the one credited, not as a double spend.
    #[serde(default)]
    pub superseded: bool,
}

/// `GET /api/v1/admin/tenant/orders/{order_id}`: the order, flattened, and
/// its payments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderDetailResponse {
    #[serde(flatten)]
    pub order: OrderView,
    pub payments: Vec<PaymentView>,
}

/// `GET /api/v1/admin/tenant/orders/totals?since=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderTotalsView {
    pub received_piconero: Piconero,
    /// Orders made at or after the time asked about.
    pub orders_since: u64,
}

/// `POST /api/v1/admin/tenant/orders/{order_id}/refund-address`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetRefundAddressRequest {
    pub refund_address: String,
}

/// `POST /api/v1/admin/tenant/payments/lookup`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LookupPaymentRequest {
    pub txid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PaymentLookupView {
    /// The node has no record of this txid at all, in a block or the pool.
    NotFoundOnChain,
    /// The transaction is real and was decoded, but none of its outputs
    /// decrypt against this store's wallet - a genuine, honest "not yours"
    /// rather than a guess.
    NoMatchingOrder,
    /// At least one output matched one of this store's orders and was
    /// recorded (idempotently - looking up an already-applied txid a second
    /// time is a safe no-op that still reports the same match). A list, not
    /// a single id: a transaction can in principle pay more than one of a
    /// store's subaddresses in one output set.
    Matched { order_ids: Vec<OrderId> },
}

// -- /status -------------------------------------------------------------------

/// `GET /status`: the engine's nodes, scanning and key custody, for the
/// whole instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineStatusResponse {
    pub networks: Vec<NetworkStatus>,
    pub poll_interval_secs: u64,
    pub generated_at: i64,
    /// How many times each background loop has been restarted after a
    /// panic or an unexpected return since the engine started. Empty while
    /// nothing has failed.
    #[serde(default)]
    pub loop_restarts: Vec<LoopRestarts>,
    /// Every store that can't be scanned right now, and why: monokulo
    /// shows each one's owner an alert.
    #[serde(default)]
    pub unserved_tenants: Vec<UnservedTenant>,
    /// Each enabled key custody backend and whether it answers. Empty with
    /// a single backend.
    #[serde(default)]
    pub key_custody: Vec<CustodyBackendStatus>,
    /// The backend a new store's keys go to unless it asks for another;
    /// `None` when there's no choice (a single backend).
    #[serde(default)]
    pub key_custody_default: Option<CustodyBackend>,
    /// Which engine images the `snp` backend trusts with keys, when it is
    /// set up: monokulo's key entry checks bundles against a policy of its
    /// own, and alerts when the two disagree.
    #[serde(default)]
    pub key_custody_snp_trust: Option<SnpTrustStatus>,
    /// The engine process's CPU and memory over the last hour, and the
    /// machine it runs on. Always sent; absent only from an older engine.
    #[serde(default)]
    pub resources: Option<service_runtime::resources::ResourceReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkStatus {
    pub network: String,
    pub nodes: Vec<NodeStatus>,
    pub scanner: ScannerStatusView,
    /// Stores not yet scanned up to this network's latest scanned block:
    /// their payments may be detected late until they catch up.
    #[serde(default)]
    pub lagging_tenants: usize,
    /// How far behind the furthest-behind store is, in blocks.
    #[serde(default)]
    pub max_blocks_behind: u64,
    /// Orders past their deadline that aren't expiring, because blocks
    /// that may hold their payments aren't scanned yet, and why; absent
    /// while there are none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry_held: Option<ExpiryHeldView>,
    /// How the block scan is going and what limits it. Always sent;
    /// absent only from an older engine.
    #[serde(default)]
    pub scaling: Option<NetworkScaling>,
    /// The nodes' ZMQ announcements; absent while no node of this network
    /// has a `zmq_pub`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announcements: Option<Announcements>,
    /// Proof-of-work checking; absent while it is off on this network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<ProofStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeStatus {
    pub label: String,
    pub is_active: bool,
    /// Skipped for now after failing.
    #[serde(default)]
    pub in_cooldown: bool,
    pub height: Option<u64>,
    pub error: Option<String>,
    /// The network the node says it's on (`get_info`'s `nettype`:
    /// `"mainnet"`, `"stagenet"`, `"testnet"` or `"fakechain"`), or `None`
    /// when it didn't say.
    #[serde(default)]
    pub network: Option<String>,
    /// What the engine has asked this node since the node was configured
    /// (or the engine started), by endpoint, busiest first.
    #[serde(default)]
    pub rpc: Vec<EndpointStats>,
    /// What has been measured of the node's link.
    #[serde(default)]
    pub link: Option<LinkSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScannerStatusView {
    pub ever_ticked: bool,
    pub last_tick_started_at: Option<i64>,
    pub last_tick_finished_at: Option<i64>,
    pub tick_count: u64,
    pub tenants_scanned: usize,
    pub last_tick_ok: bool,
    pub last_error: Option<String>,
    /// Too long since the last tick finished (or never ticked at all).
    pub is_stale: bool,
}

/// Orders past their deadline held open, for `/status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpiryHeldView {
    pub orders: u64,
    /// The earliest of their deadlines.
    pub overdue_since: i64,
    /// Some stores are behind the node's tip.
    pub behind_tip: bool,
    /// Since when the node has said it isn't synchronized with its peers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_unsynchronized_since: Option<i64>,
    /// Since when a reorg has been reconciled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reorg_open_since: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyBackendStatus {
    pub backend: CustodyBackend,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnpTrustStatus {
    /// SHA-384 of the ID key images must be signed with, hex.
    pub id_key_digest: String,
    pub min_guest_svn: u32,
    /// `bootloader,tee,snp,microcode`, or empty for no floor.
    #[serde(default)]
    pub min_tcb: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopRestarts {
    pub name: String,
    pub restarts: u64,
}

/// A store that can't be scanned right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnservedTenant {
    pub public_key: String,
    pub network: String,
    /// Sent as `reason` and `blocks_behind`.
    #[serde(flatten)]
    pub reason: UnservedReason,
}

/// Why a store isn't scanned. On the wire, `reason` names it and
/// `blocks_behind` is the count for [`UnservedReason::CatchingUp`] (`null`
/// otherwise).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnservedReason {
    /// Its network has no node configured, or none answers.
    NoReachableNode,
    /// It fell behind and is being caught up, this many blocks behind.
    CatchingUp { blocks_behind: u64 },
    /// Its keys are in a key custody backend that is turned off.
    CustodyDisabled,
    /// Its key custody backend doesn't answer.
    CustodyUnavailable,
    /// A reason this build doesn't know, from a newer engine.
    Unknown,
}

/// `UnservedReason` as the wire spells it.
#[derive(Serialize, Deserialize)]
struct UnservedWire {
    reason: String,
    #[serde(default)]
    blocks_behind: Option<u64>,
}

impl UnservedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            UnservedReason::NoReachableNode => "no_reachable_node",
            UnservedReason::CatchingUp { .. } => "catching_up",
            UnservedReason::CustodyDisabled => "custody_disabled",
            UnservedReason::CustodyUnavailable => "custody_unavailable",
            UnservedReason::Unknown => "unknown",
        }
    }
}

impl Serialize for UnservedReason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        UnservedWire {
            reason: self.as_str().to_owned(),
            blocks_behind: match self {
                UnservedReason::CatchingUp { blocks_behind } => Some(*blocks_behind),
                UnservedReason::NoReachableNode
                | UnservedReason::CustodyDisabled
                | UnservedReason::CustodyUnavailable
                | UnservedReason::Unknown => None,
            },
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for UnservedReason {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = UnservedWire::deserialize(deserializer)?;
        Ok(match wire.reason.as_str() {
            "no_reachable_node" => UnservedReason::NoReachableNode,
            "catching_up" => UnservedReason::CatchingUp {
                blocks_behind: wire.blocks_behind.unwrap_or(0),
            },
            "custody_disabled" => UnservedReason::CustodyDisabled,
            "custody_unavailable" => UnservedReason::CustodyUnavailable,
            _ => UnservedReason::Unknown,
        })
    }
}
