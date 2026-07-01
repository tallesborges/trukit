use crate::dotns;
use crate::env::Env;
use crate::registrar;
use anyhow::{bail, Context, Result};
use cid::Cid;
use futures::StreamExt;
use multihash_codetable::{Code, MultihashDigest};
use rand::RngCore;
use scale_info::PortableRegistry;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use subxt::config::RpcConfigFor;
use subxt::ext::codec::{Decode, Encode};
use subxt::metadata::{ArcMetadata, Metadata};
use subxt::rpcs::{LegacyRpcMethods, RpcClient};
use subxt::utils::{AccountId32, H160};
use subxt::{OnlineClient, PolkadotConfig};
use subxt_signer::{sr25519::Keypair, SecretUri};

/// Per-client cache of runtime metadata keyed by spec version. subxt 0.50 fetches
/// metadata lazily on the first block access and offers the [`subxt::Config`] the
/// chance to cache it via [`subxt::Config::set_metadata_for_spec_version`]. The
/// default trait methods are no-ops, so a bespoke `Config` re-downloads metadata
/// on *every* `at_current_block`/`tx`/`wait_for_success` call; wiring this cache
/// in makes a reused client download each runtime's metadata exactly once.
///
/// Because every `trikit` invocation is a fresh process, this in-memory cache is
/// additionally *pre-seeded* from the persistent on-disk cache (see
/// [`load_cached_metadata`]) before the client is built, so an unchanged runtime
/// spec version is served entirely from disk with no metadata download at all.
#[derive(Debug, Clone, Default)]
struct MetadataCache(Arc<RwLock<HashMap<u32, ArcMetadata>>>);

impl MetadataCache {
    fn get(&self, spec_version: u32) -> Option<ArcMetadata> {
        self.0.read().unwrap().get(&spec_version).cloned()
    }

    fn set(&self, spec_version: u32, metadata: ArcMetadata) {
        self.0.write().unwrap().insert(spec_version, metadata);
    }
}

/// Directory for the cross-run runtime-metadata cache. Honors `TRIKIT_CACHE_DIR`,
/// then `XDG_CACHE_HOME`, then `~/.cache`, filing metadata under `trikit/metadata`.
/// Returns `None` when no location can be resolved — metadata is then fetched
/// fresh every run (still correct, just not cached).
fn metadata_cache_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TRIKIT_CACHE_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if let Some(dir) = std::env::var_os("XDG_CACHE_HOME").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir).join("trikit").join("metadata"));
    }
    let home = std::env::var_os("HOME").filter(|d| !d.is_empty())?;
    Some(PathBuf::from(home).join(".cache").join("trikit").join("metadata"))
}

/// Filesystem-safe token identifying a chain endpoint (scheme stripped, every
/// non-alphanumeric byte mapped to `_`), so cached metadata for different
/// RPCs/envs never collides even when two chains happen to share a spec version.
fn cache_namespace(rpc_url: &str) -> String {
    let host = rpc_url.split_once("://").map_or(rpc_url, |(_, rest)| rest);
    host.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// On-disk path for a chain's metadata at a given spec version, or `None` when no
/// cache directory can be resolved.
fn metadata_cache_path(namespace: &str, spec_version: u32) -> Option<PathBuf> {
    Some(metadata_cache_dir()?.join(format!("{namespace}-v{spec_version}.scale")))
}

/// Fetch the raw `RuntimeMetadataPrefixed` SCALE bytes from the chain, mirroring
/// subxt's own selection: the newest non-unstable version via the
/// `Metadata_metadata_at_version` runtime API, falling back to the legacy
/// `Metadata_metadata`. The bytes are exactly what [`Metadata::decode_from`] (and
/// subxt internally) expect, so they can be persisted and reloaded verbatim —
/// crucially they include runtime-API descriptors (needed by `ReviveApi.*`),
/// unlike the trimmed-down `state_getMetadata` output.
async fn fetch_metadata_bytes<C: subxt::Config>(
    methods: &LegacyRpcMethods<RpcConfigFor<C>>,
) -> Result<Vec<u8>> {
    let latest_version = methods
        .state_call("Metadata_metadata_versions", None, None)
        .await
        .ok()
        .and_then(|res| <Vec<u32>>::decode(&mut &res[..]).ok())
        .and_then(|versions| versions.into_iter().filter(|v| *v != u32::MAX).max());

    if let Some(version) = latest_version {
        let params = version.encode();
        let resp = methods
            .state_call("Metadata_metadata_at_version", Some(params.as_slice()), None)
            .await
            .context("Metadata_metadata_at_version runtime call")?;
        // `Option<OpaqueMetadata>`; `OpaqueMetadata` encodes as a length-prefixed
        // byte blob, so decoding as `Option<Vec<u8>>` yields the inner
        // `RuntimeMetadataPrefixed` bytes directly.
        let bytes = <Option<Vec<u8>>>::decode(&mut &resp[..])
            .context("decoding Metadata_metadata_at_version response")?
            .context("chain returned no metadata for its latest metadata version")?;
        return Ok(bytes);
    }

    let resp = methods
        .state_call("Metadata_metadata", None, None)
        .await
        .context("Metadata_metadata runtime call")?;
    let bytes = <Vec<u8>>::decode(&mut &resp[..]).context("decoding Metadata_metadata response")?;
    Ok(bytes)
}

/// Download the chain's metadata bytes, decode them, and (best-effort) persist
/// them to `path` for reuse on the next run. A failed cache write never fails the
/// command.
async fn fetch_and_cache<C: subxt::Config>(
    methods: &LegacyRpcMethods<RpcConfigFor<C>>,
    path: Option<&Path>,
) -> Result<Metadata> {
    let bytes = fetch_metadata_bytes::<C>(methods).await?;
    let metadata =
        Metadata::decode_from(&bytes).context("decoding runtime metadata fetched from chain")?;
    if let Some(path) = path {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, &bytes);
    }
    Ok(metadata)
}

/// Resolve the runtime metadata for the chain reachable at `rpc`, preferring the
/// persistent on-disk copy keyed by `(endpoint, spec_version)` and only paying
/// for the (large) metadata download when the spec version isn't cached yet —
/// i.e. on first use or after a runtime upgrade. Keying by spec version keeps
/// this correct across upgrades: a bumped runtime is a cache miss and refetches.
/// The returned [`MetadataCache`] is pre-seeded so the subsequent client performs
/// no metadata download of its own.
async fn load_cached_metadata<C: subxt::Config>(
    rpc: &RpcClient,
    rpc_url: &str,
) -> Result<MetadataCache> {
    let methods = LegacyRpcMethods::<RpcConfigFor<C>>::new(rpc.clone());
    let spec_version = methods
        .state_get_runtime_version(None)
        .await
        .context("fetching runtime spec version")?
        .spec_version;

    let path = metadata_cache_path(&cache_namespace(rpc_url), spec_version);

    let metadata = match path.as_deref().and_then(|p| std::fs::read(p).ok()) {
        // A cached blob that no longer decodes (corrupt/partial) is ignored and refetched.
        Some(bytes) => match Metadata::decode_from(&bytes) {
            Ok(metadata) => metadata,
            Err(_) => fetch_and_cache::<C>(&methods, path.as_deref()).await?,
        },
        None => fetch_and_cache::<C>(&methods, path.as_deref()).await?,
    };

    let cache = MetadataCache::default();
    cache.set(spec_version, metadata.arc());
    Ok(cache)
}

/// Connect to `rpc_url` and build an [`OnlineClient`] whose config is pre-seeded
/// with the chain's runtime metadata from the persistent cache. A single RPC
/// connection is reused for the spec-version probe, any metadata fetch, and the
/// client itself.
async fn connect_with_cache<C, F>(rpc_url: &str, make_config: F) -> Result<OnlineClient<C>>
where
    C: subxt::Config,
    F: FnOnce(MetadataCache) -> C,
{
    let rpc = RpcClient::from_url(rpc_url)
        .await
        .with_context(|| format!("connecting to RPC {rpc_url}"))?;
    let cache = load_cached_metadata::<C>(&rpc, rpc_url)
        .await
        .with_context(|| format!("loading runtime metadata from {rpc_url}"))?;
    OnlineClient::from_rpc_client_with_config(make_config(cache), rpc)
        .await
        .with_context(|| format!("connecting to RPC {rpc_url}"))
}

#[subxt::subxt(runtime_metadata_path = "artifacts/paseo_next_v2_asset_hub.scale")]
pub mod asset_hub {}

#[subxt::subxt(runtime_metadata_path = "artifacts/paseo_next_v2_bulletin.scale")]
pub mod bulletin {}

/// The Bulletin chain declares three custom, empty transaction extensions on top
/// of the usual Substrate ones — `AuthorizeCall`, `ValidateStorageCalls` and
/// `AllowanceBasedPriority` — plus `CheckNonZeroSender`, `CheckWeight` and
/// `StorageWeightReclaim`, none of which subxt's `PolkadotConfig` provides.
/// Signing therefore needs a bespoke [`Config`] whose `TransactionExtensions`
/// tuple covers every extension the runtime lists, in declared order. Each of the
/// extensions below encodes nothing for both the value and the implicit payload.
macro_rules! empty_extension {
    ($ext:ident, $name:literal) => {
        pub struct $ext;

        impl<T: subxt::Config> subxt::config::TransactionExtension<T> for $ext {
            type Decoded = ();
            type Params = ();

            fn new(
                _client: &subxt::config::ClientState<T>,
                _params: Self::Params,
            ) -> core::result::Result<Self, subxt::error::TransactionExtensionError> {
                Ok($ext)
            }
        }

        impl subxt::ext::frame_decode::extrinsics::TransactionExtension<PortableRegistry> for $ext {
            const NAME: &str = $name;

            fn encode_value_to(
                &self,
                _type_id: u32,
                _type_resolver: &PortableRegistry,
                _out: &mut Vec<u8>,
            ) -> core::result::Result<
                (),
                subxt::ext::frame_decode::extrinsics::TransactionExtensionError,
            > {
                Ok(())
            }

            fn encode_implicit_to(
                &self,
                _type_id: u32,
                _type_resolver: &PortableRegistry,
                _out: &mut Vec<u8>,
            ) -> core::result::Result<
                (),
                subxt::ext::frame_decode::extrinsics::TransactionExtensionError,
            > {
                Ok(())
            }
        }
    };
}

empty_extension!(AuthorizeCall, "AuthorizeCall");
empty_extension!(CheckNonZeroSender, "CheckNonZeroSender");
empty_extension!(CheckWeight, "CheckWeight");
empty_extension!(ValidateStorageCalls, "ValidateStorageCalls");
empty_extension!(AllowanceBasedPriority, "AllowanceBasedPriority");
empty_extension!(StorageWeightReclaim, "StorageWeightReclaim");
empty_extension!(EthSetOrigin, "EthSetOrigin");

/// Asset Hub declares several custom transaction extensions that carry a real,
/// non-empty value (unlike the empty ones above). For a plain signed call none
/// of the optional behaviours apply, so each encodes its inert default —
/// `Option::None` (one `0x00` byte) or `false` — and nothing for the implicit.
macro_rules! default_value_extension {
    ($ext:ident, $name:literal, $value:expr) => {
        pub struct $ext;

        impl<T: subxt::Config> subxt::config::TransactionExtension<T> for $ext {
            type Decoded = ();
            type Params = ();

            fn new(
                _client: &subxt::config::ClientState<T>,
                _params: Self::Params,
            ) -> core::result::Result<Self, subxt::error::TransactionExtensionError> {
                Ok($ext)
            }
        }

        impl subxt::ext::frame_decode::extrinsics::TransactionExtension<PortableRegistry> for $ext {
            const NAME: &str = $name;

            fn encode_value_to(
                &self,
                _type_id: u32,
                _type_resolver: &PortableRegistry,
                out: &mut Vec<u8>,
            ) -> core::result::Result<
                (),
                subxt::ext::frame_decode::extrinsics::TransactionExtensionError,
            > {
                subxt::ext::codec::Encode::encode_to(&$value, out);
                Ok(())
            }

            fn encode_implicit_to(
                &self,
                _type_id: u32,
                _type_resolver: &PortableRegistry,
                _out: &mut Vec<u8>,
            ) -> core::result::Result<
                (),
                subxt::ext::frame_decode::extrinsics::TransactionExtensionError,
            > {
                Ok(())
            }
        }
    };
}

default_value_extension!(
    AuthorizeValueTransfer,
    "AuthorizeValueTransfer",
    Option::<()>::None
);
default_value_extension!(AsPgas, "AsPgas", Option::<()>::None);
default_value_extension!(AsRingAlias, "AsRingAlias", Option::<()>::None);
default_value_extension!(AsDotnsGateway, "AsDotnsGateway", Option::<()>::None);
default_value_extension!(RestrictOrigins, "RestrictOrigins", false);

use subxt::config::transaction_extensions as tx_ext;

type BulletinTxExtensions = (
    AuthorizeCall,
    CheckNonZeroSender,
    tx_ext::CheckSpecVersion,
    tx_ext::CheckTxVersion,
    tx_ext::CheckGenesis<BulletinConfig>,
    tx_ext::CheckMortality<BulletinConfig>,
    tx_ext::CheckNonce,
    CheckWeight,
    tx_ext::ChargeTransactionPayment,
    ValidateStorageCalls,
    AllowanceBasedPriority,
    tx_ext::CheckMetadataHash,
    StorageWeightReclaim,
);

/// subxt [`Config`] for the Bulletin chain. Account/address/signature/hashing all
/// match a standard Substrate chain; only the transaction-extension set differs.
/// Genesis hash and runtime version are still fetched from the node, but the
/// [`MetadataCache`] keeps each spec version's metadata so a reused client
/// downloads it once instead of on every block access.
#[derive(Debug, Clone, Default)]
pub struct BulletinConfig {
    metadata_cache: MetadataCache,
}

impl subxt::Config for BulletinConfig {
    type AccountId = AccountId32;
    type Address = subxt::utils::MultiAddress<AccountId32, ()>;
    type Signature = subxt::utils::MultiSignature;
    type Hasher = <PolkadotConfig as subxt::Config>::Hasher;
    type Header = <PolkadotConfig as subxt::Config>::Header;
    type AssetId = u32;
    type TransactionExtensions = BulletinTxExtensions;

    fn metadata_for_spec_version(&self, spec_version: u32) -> Option<ArcMetadata> {
        self.metadata_cache.get(spec_version)
    }

    fn set_metadata_for_spec_version(&self, spec_version: u32, metadata: ArcMetadata) {
        self.metadata_cache.set(spec_version, metadata);
    }
}

/// Asset Hub (paseo-next-v2) lists 17 transaction extensions in this exact
/// declared order. subxt matches each by name, so the tuple must name all of
/// them. Six are custom to the individuality/revive runtime — five carry a
/// value (`AuthorizeValueTransfer`, `AsPgas`, `AsRingAlias`, `AsDotnsGateway`,
/// `RestrictOrigins`) and encode their inert default, `AuthorizeCall`/
/// `EthSetOrigin` are empty. `ChargeAssetTxPayment` pays fees in the native
/// token (tip 0, `asset_id: None`).
type AssetHubTxExtensions = (
    AuthorizeValueTransfer,
    AuthorizeCall,
    AsPgas,
    AsRingAlias,
    AsDotnsGateway,
    RestrictOrigins,
    CheckNonZeroSender,
    tx_ext::CheckSpecVersion,
    tx_ext::CheckTxVersion,
    tx_ext::CheckGenesis<AssetHubConfig>,
    tx_ext::CheckMortality<AssetHubConfig>,
    tx_ext::CheckNonce,
    CheckWeight,
    tx_ext::ChargeAssetTxPayment<AssetHubConfig>,
    tx_ext::CheckMetadataHash,
    EthSetOrigin,
    StorageWeightReclaim,
);

/// subxt [`Config`] for Asset Hub. Same account/address/signature/hashing as a
/// standard Substrate chain; only the extension set differs. `AssetId = u32` is
/// only used by `ChargeAssetTxPayment`, which we always call with `None`, so the
/// concrete type never affects the encoded bytes.
#[derive(Debug, Clone, Default)]
pub struct AssetHubConfig {
    metadata_cache: MetadataCache,
}

impl subxt::Config for AssetHubConfig {
    type AccountId = AccountId32;
    type Address = subxt::utils::MultiAddress<AccountId32, ()>;
    type Signature = subxt::utils::MultiSignature;
    type Hasher = <PolkadotConfig as subxt::Config>::Hasher;
    type Header = <PolkadotConfig as subxt::Config>::Header;
    type AssetId = u32;
    type TransactionExtensions = AssetHubTxExtensions;

    fn metadata_for_spec_version(&self, spec_version: u32) -> Option<ArcMetadata> {
        self.metadata_cache.get(spec_version)
    }

    fn set_metadata_for_spec_version(&self, spec_version: u32, metadata: ArcMetadata) {
        self.metadata_cache.set(spec_version, metadata);
    }
}

/// CIDv1 (raw codec `0x55`, sha2-256 multihash) of a blob's bytes — the CID the
/// Bulletin chain assigns to data stored via `store_with_cid_config`.
pub fn raw_cid(data: &[u8]) -> Cid {
    Cid::new_v1(0x55, Code::Sha2_256.digest(data))
}

/// sha2-256 of a blob's bytes; this is the key the chain uses in
/// `TransactionStorage.TransactionByContentHash`.
pub fn content_hash(data: &[u8]) -> [u8; 32] {
    let digest = Code::Sha2_256.digest(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.digest());
    out
}

/// Result of storing a single IPLD block via [`store_block`].
pub enum StoreOutcome {
    Stored { block: u32, index: u32 },
    AlreadyPresent { block: u32, index: u32 },
}

/// A CARv1 block ready to upload: its IPLD `codec`, raw `data`, and the sha2-256
/// `content_hash` the Bulletin chain keys it by in `TransactionByContentHash`.
pub struct PreparedBlock {
    pub codec: u64,
    pub data: Vec<u8>,
    pub content_hash: [u8; 32],
}

/// The `BulletinConfig` transaction-extension params, one slot per extension in
/// [`BulletinTxExtensions`]. Only `CheckMortality`, `CheckNonce` and
/// `ChargeTransactionPayment` take a non-`()` value; the rest are empty.
type StoreParams = (
    (),
    (),
    (),
    (),
    (),
    tx_ext::CheckMortalityParams<BulletinConfig>,
    tx_ext::CheckNonceParams,
    (),
    tx_ext::ChargeTransactionPaymentParams,
    (),
    (),
    (),
    (),
);

/// Params for a store extrinsic pinned to an explicit `nonce`. Immortal era so a
/// signed extrinsic stays valid across the submit/confirm/retry window, and no
/// tip (`AllowanceBasedPriority` gives every store call the same max priority).
fn store_params(nonce: u64) -> StoreParams {
    (
        (),
        (),
        (),
        (),
        (),
        tx_ext::CheckMortalityParams::immortal(),
        tx_ext::CheckNonceParams::with_nonce(nonce),
        (),
        tx_ext::ChargeTransactionPaymentParams::no_tip(),
        (),
        (),
        (),
        (),
    )
}

async fn connect_bulletin(rpc_url: &str) -> Result<OnlineClient<BulletinConfig>> {
    connect_with_cache(rpc_url, |metadata_cache| BulletinConfig { metadata_cache }).await
}

/// Open a Bulletin client using the bespoke [`BulletinConfig`]. The client's
/// metadata cache is pre-seeded from the persistent on-disk cache, so an
/// unchanged runtime is served from disk with no metadata download.
pub async fn bulletin_client(env: &Env) -> Result<OnlineClient<BulletinConfig>> {
    connect_bulletin(env.bulletin_rpc).await
}

async fn is_stored(
    at: &subxt::client::ClientAtBlock<
        BulletinConfig,
        impl subxt::client::OnlineClientAtBlockT<BulletinConfig>,
    >,
    content_hash: [u8; 32],
) -> Result<bool> {
    let existing = at
        .storage()
        .try_fetch(
            bulletin::storage()
                .transaction_storage()
                .transaction_by_content_hash(),
            (content_hash,),
        )
        .await
        .context("reading TransactionStorage.TransactionByContentHash")?;
    Ok(existing.is_some())
}

/// Build a signed, ready-to-submit store extrinsic for `block`, pinned to an
/// explicit `nonce`. Offline (no RPC): the returned [`subxt::tx::SubmittableTransaction`]
/// owns its encoded bytes plus a cheap client handle, so a batch of them can be
/// submitted and watched to inclusion concurrently.
fn build_store_submittable<C>(
    tx_client: &subxt::tx::TransactionsClient<BulletinConfig, C>,
    signer: &Keypair,
    block: &PreparedBlock,
    nonce: u64,
) -> Result<subxt::tx::SubmittableTransaction<BulletinConfig, C>>
where
    C: subxt::client::OnlineClientAtBlockT<BulletinConfig>,
{
    let cid_config =
        bulletin::runtime_types::bulletin_transaction_storage_primitives::cids::CidConfig {
            codec: block.codec,
            hashing:
                bulletin::runtime_types::bulletin_transaction_storage_primitives::cids::HashingAlgorithm::Sha2_256,
        };
    let call = bulletin::tx()
        .transaction_storage()
        .store_with_cid_config(cid_config, block.data.clone());
    tx_client
        .create_signable_offline(&call, store_params(nonce))
        .context(
            "building signed store extrinsic \
             (if this fails after a runtime upgrade the pinned metadata is stale — \
             regenerate artifacts/*.scale)",
        )?
        .sign(signer)
        .context("signing store extrinsic")
}

/// Submit `sub` fire-and-forget: return once the node accepts it into the pool
/// (its first status), without waiting for inclusion. Inclusion is confirmed out
/// of band by reading `TransactionByContentHash` at the best block. A duplicate
/// store (content a concurrent deploy already stored) is a benign success on this
/// runtime — it dedups by content hash and does not emit `ExtrinsicFailed` — so
/// no already-stored special-casing is needed.
async fn submit_store<C>(
    tx_client: &subxt::tx::TransactionsClient<BulletinConfig, C>,
    signer: &Keypair,
    block: &PreparedBlock,
    nonce: u64,
) -> Result<()>
where
    C: subxt::client::OnlineClientAtBlockT<BulletinConfig>,
{
    let sub = build_store_submittable(tx_client, signer, block, nonce)?;
    tokio::time::timeout(FIRE_TIMEOUT, sub.submit())
        .await
        .context("timed out submitting store_with_cid_config")?
        .context("submitting store_with_cid_config")?;
    Ok(())
}

/// Upper bound on store extrinsics submitted concurrently.
const UPLOAD_CONCURRENCY: usize = 20;
/// How many fire/confirm rounds before giving up on the stragglers.
const MAX_ATTEMPTS: usize = 5;
/// Backoff between retry rounds after a transient submit/inclusion failure.
const RETRY_BACKOFF: Duration = Duration::from_secs(2);
/// Cap on how long a single `submit()` may block before we treat it as failed.
const FIRE_TIMEOUT: Duration = Duration::from_secs(20);
/// How long to wait for a fired batch to be *included* (best block) before
/// re-firing the stragglers — a handful of ~6s blocks, well under finalization.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);

/// Store every [`PreparedBlock`] on the Bulletin chain, fast and idempotently.
///
/// A single finalized-block snapshot probes all content hashes concurrently, so
/// blocks a prior run already finalized are skipped up front (cross-run
/// idempotency), and duplicate content within the CAR is stored once. The rest
/// are signed offline with dense nonces (`base + j`), fired at bounded
/// concurrency, and confirmed at *inclusion* by reading `TransactionByContentHash`
/// on each new **best** block (~6–12s) rather than the finalized block (~40–60s)
/// — keyed by content hash, so a block a concurrent deploy stores also counts.
/// Whatever is still missing after the confirm window is re-fired with a freshly
/// fetched nonce, reconnecting on RPC drop; only a full run of failed attempts is
/// fatal. `on_progress(done, stored, skipped)` fires as blocks confirm. Returns
/// `(stored, skipped)`.
pub async fn store_car_blocks(
    client: &OnlineClient<BulletinConfig>,
    rpc_url: &str,
    signer: &Keypair,
    blocks: &[PreparedBlock],
    mut on_progress: impl FnMut(usize, usize, usize),
) -> Result<(usize, usize)> {
    let total = blocks.len();
    let account = account_id(signer);
    let mut client = client.clone();

    let at = client.at_current_block().await?;
    let probes = blocks
        .iter()
        .map(|block| is_stored(&at, block.content_hash));
    let present = futures::future::join_all(probes).await;
    drop(at);

    let mut skipped = 0usize;
    let mut todo = Vec::new();
    let mut seen: HashSet<[u8; 32]> = HashSet::new();
    for (idx, present) in present.into_iter().enumerate() {
        if present? || !seen.insert(blocks[idx].content_hash) {
            skipped += 1;
        } else {
            todo.push(idx);
        }
    }
    let mut stored = 0usize;
    on_progress(stored + skipped, stored, skipped);

    let mut attempt = 0usize;
    let mut last_error: Option<anyhow::Error> = None;
    while !todo.is_empty() {
        attempt += 1;
        if attempt > MAX_ATTEMPTS {
            let detail = last_error
                .map(|e| format!(": last error: {e:#}"))
                .unwrap_or_default();
            bail!(
                "gave up storing {} of {total} blocks after {MAX_ATTEMPTS} attempts{detail}",
                todo.len()
            );
        }
        if attempt > 1 {
            tokio::time::sleep(RETRY_BACKOFF).await;
        }

        let tx_client = match client.tx().await {
            Ok(tx_client) => tx_client,
            Err(_) => {
                client = connect_bulletin(rpc_url).await?;
                client.tx().await?
            }
        };
        let base_nonce = tx_client
            .account_nonce(&account)
            .await
            .context("fetching account nonce")?;

        let fires = todo.iter().enumerate().map(|(offset, &idx)| {
            let tx_client = &tx_client;
            let block = &blocks[idx];
            let nonce = base_nonce + offset as u64;
            async move { submit_store(tx_client, signer, block, nonce).await }
        });
        let outcomes: Vec<Result<()>> = futures::stream::iter(fires)
            .buffer_unordered(UPLOAD_CONCURRENCY)
            .collect()
            .await;
        if let Some(e) = outcomes.into_iter().filter_map(Result::err).next() {
            last_error = Some(e);
        }

        // Confirm at inclusion: read TransactionByContentHash at each new best
        // (not-yet-finalized) block, so stores land in ~6–12s not ~40–60s.
        let deadline = tokio::time::Instant::now() + CONFIRM_TIMEOUT;
        let mut best = match client.stream_best_blocks().await {
            Ok(best) => best,
            Err(_) => {
                client = connect_bulletin(rpc_url).await?;
                client.stream_best_blocks().await?
            }
        };
        while !todo.is_empty() {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let block = match tokio::time::timeout(remaining, best.next()).await {
                Ok(Some(Ok(block))) => block,
                Ok(Some(Err(e))) => {
                    last_error = Some(anyhow::anyhow!("best-block stream error: {e}"));
                    break;
                }
                Ok(None) | Err(_) => break,
            };
            let at = match block.at().await {
                Ok(at) => at,
                Err(_) => continue,
            };
            let checks = todo
                .iter()
                .map(|&idx| is_stored(&at, blocks[idx].content_hash));
            let present = futures::future::join_all(checks).await;
            drop(at);

            let mut still = Vec::with_capacity(todo.len());
            for (&idx, present) in todo.iter().zip(present) {
                match present {
                    Ok(true) => {
                        stored += 1;
                        on_progress(stored + skipped, stored, skipped);
                    }
                    Ok(false) => still.push(idx),
                    Err(e) => {
                        last_error = Some(e);
                        still.push(idx);
                    }
                }
            }
            todo = still;
        }
    }

    Ok((stored, skipped))
}

/// Store one blob (an IPLD block) on the Bulletin chain under its own content
/// hash, using the block's `codec` and sha2-256. Idempotent: if the block is
/// already stored (keyed by `sha256(data)` in `TransactionByContentHash`) it
/// returns [`StoreOutcome::AlreadyPresent`] without submitting. `data` must be
/// no larger than the chain's `MaxTransactionSize`; callers guard that.
pub async fn store_block(
    client: &OnlineClient<BulletinConfig>,
    signer: &Keypair,
    codec: u64,
    data: &[u8],
) -> Result<StoreOutcome> {
    let content_hash = content_hash(data);

    let at = client.at_current_block().await?;
    let existing = at
        .storage()
        .try_fetch(
            bulletin::storage()
                .transaction_storage()
                .transaction_by_content_hash(),
            (content_hash,),
        )
        .await
        .context("reading TransactionStorage.TransactionByContentHash")?;
    if let Some(value) = existing {
        let (block, index) = value.decode().context("decoding stored location")?;
        return Ok(StoreOutcome::AlreadyPresent { block, index });
    }
    drop(at);

    let cid_config =
        bulletin::runtime_types::bulletin_transaction_storage_primitives::cids::CidConfig {
            codec,
            hashing:
                bulletin::runtime_types::bulletin_transaction_storage_primitives::cids::HashingAlgorithm::Sha2_256,
        };
    let call = bulletin::tx()
        .transaction_storage()
        .store_with_cid_config(cid_config, data.to_vec());

    client
        .tx()
        .await?
        .sign_and_submit_then_watch_default(&call, signer)
        .await
        .context("submitting store_with_cid_config")?
        .wait_for_finalized_success()
        .await
        .context("store_with_cid_config did not finalize successfully")?;

    let at = client.at_current_block().await?;
    let (block, index) = at
        .storage()
        .try_fetch(
            bulletin::storage()
                .transaction_storage()
                .transaction_by_content_hash(),
            (content_hash,),
        )
        .await
        .context("re-reading TransactionByContentHash after store")?
        .context("store finalized but TransactionByContentHash is still empty")?
        .decode()
        .context("decoding stored location")?;
    Ok(StoreOutcome::Stored { block, index })
}

/// Standard Substrate dev phrase. Its bare-master account (empty derivation) is
/// the dev-mode DotNS owner used by `bulletin-deploy` / `playground-cli`; its
/// `//deploy/N` derivations are the authorized Bulletin pool.
pub const DEV_PHRASE: &str = "bottom drive obey lake curtain smoke basket hold race lonely fit walk";

/// Build an sr25519 signer from a mnemonic (+ optional derivation path). Defaults
/// to the bare-master dev account so `trikit` owns the same dev-mode names
/// `bulletin-deploy` / `playground-cli` register. Never logs the mnemonic.
pub fn build_signer(mnemonic: Option<&str>, derivation_path: Option<&str>) -> Result<Keypair> {
    let phrase = mnemonic.unwrap_or(DEV_PHRASE);
    let suffix = derivation_path.unwrap_or("");
    let uri = SecretUri::from_str(&format!("{phrase}{suffix}"))
        .context("failed to parse mnemonic + derivation path")?;
    Keypair::from_uri(&uri).context("failed to derive sr25519 keypair")
}

pub fn account_id(signer: &Keypair) -> AccountId32 {
    AccountId32(signer.public_key().0)
}

/// Resolve the H160 (EVM) address for an account via the `ReviveApi.address`
/// runtime API on the given Asset Hub client.
pub async fn revive_address(
    client: &OnlineClient<AssetHubConfig>,
    account: AccountId32,
) -> Result<H160> {
    let call = asset_hub::runtime_apis().revive_api().address(account);
    let h160 = client
        .at_current_block()
        .await?
        .runtime_apis()
        .call(call)
        .await
        .context("ReviveApi.address runtime call failed")?;
    Ok(h160)
}

fn parse_h160(addr: &str) -> Result<H160> {
    let raw = addr.strip_prefix("0x").unwrap_or(addr);
    let bytes = hex::decode(raw).with_context(|| format!("invalid H160 hex '{addr}'"))?;
    let arr: [u8; 20] = bytes
        .as_slice()
        .try_into()
        .with_context(|| format!("expected 20-byte H160, got {} bytes", bytes.len()))?;
    Ok(H160(arr))
}

/// Read a `.dot` name's raw DotNS contenthash bytes (EIP-1577, e.g. `0xe301…`)
/// by dry-running the resolver's `contenthash(bytes32)` view via `ReviveApi.call`
/// on the given Asset Hub client. Returns empty when no contenthash is set.
/// `name` must be normalized already.
pub async fn resolve_contenthash(
    client: &OnlineClient<AssetHubConfig>,
    env: &Env,
    name: &str,
) -> Result<Vec<u8>> {
    let node = dotns::namehash(name);
    let input_data = dotns::encode_contenthash_call(node);
    let dest = parse_h160(env.dotns_content_resolver)?;
    let origin = account_id(&build_signer(None, None)?);

    let call = asset_hub::runtime_apis()
        .revive_api()
        .call(origin, dest, 0, None, None, input_data);
    let result = client
        .at_current_block()
        .await?
        .runtime_apis()
        .call(call)
        .await
        .context("ReviveApi.call runtime call failed")?;

    let exec = match result.result {
        Ok(exec) => exec,
        Err(err) => bail!("resolver call failed on chain: {err:?}"),
    };
    if exec.flags.bits & 1 != 0 {
        bail!("resolver call reverted");
    }
    dotns::decode_contenthash_return(&exec.data)
}

/// Open an Asset Hub client using the bespoke [`AssetHubConfig`] so signed
/// extrinsics carry the full 17-extension payload the runtime expects. The
/// client's metadata cache is pre-seeded from the persistent on-disk cache.
pub async fn asset_hub_client(env: &Env) -> Result<OnlineClient<AssetHubConfig>> {
    connect_with_cache(env.asset_hub_rpc, |metadata_cache| AssetHubConfig {
        metadata_cache,
    })
    .await
}

/// Ensure the signer's account has an H160 mapping in `Revive.OriginalAccount`.
/// A signed `Revive.call` requires the caller to be mapped; if it isn't, submit
/// a `Revive.map_account()` first and wait for it to finalize.
pub async fn ensure_mapped(client: &OnlineClient<AssetHubConfig>, signer: &Keypair) -> Result<()> {
    let account = account_id(signer);
    let at = client.at_current_block().await?;
    let h160 = at
        .runtime_apis()
        .call(asset_hub::runtime_apis().revive_api().address(account))
        .await
        .context("ReviveApi.address runtime call failed")?;

    let existing = at
        .storage()
        .try_fetch(asset_hub::storage().revive().original_account(), (h160,))
        .await
        .context("reading Revive.OriginalAccount")?;
    if existing.is_some() {
        return Ok(());
    }

    println!("account not mapped on Asset Hub; submitting Revive.map_account()...");
    let call = asset_hub::tx().revive().map_account();
    let events = client
        .tx()
        .await?
        .sign_and_submit_then_watch_default(&call, signer)
        .await
        .context("submitting Revive.map_account")?
        .wait_for_finalized_success()
        .await
        .context("Revive.map_account did not finalize successfully")?;
    println!("mapped (tx 0x{})", hex::encode(events.extrinsic_hash().0));
    Ok(())
}

/// Read-only `ReviveApi.call` dry-run against `dest` with `calldata`, returning
/// the raw ABI-encoded return data. Rejects on-chain errors and reverts so
/// callers can treat a successful result as authoritative. Nothing is submitted.
pub async fn revive_view(
    client: &OnlineClient<AssetHubConfig>,
    origin: AccountId32,
    dest: H160,
    value: u128,
    calldata: Vec<u8>,
) -> Result<Vec<u8>> {
    let call = asset_hub::runtime_apis()
        .revive_api()
        .call(origin, dest, value, None, None, calldata);
    let outcome = client
        .at_current_block()
        .await?
        .runtime_apis()
        .call(call)
        .await
        .context("ReviveApi.call dry-run failed")?;

    let exec = match outcome.result {
        Ok(exec) => exec,
        Err(err) => bail!("view call failed on chain: {err:?}"),
    };
    if exec.flags.bits & 1 != 0 {
        bail!("view call reverted");
    }
    Ok(exec.data)
}

/// Submit a signed `Revive.call` to `dest` with `calldata`, transferring `value`
/// native tokens (0 for non-payable calls). Ensures the signer is mapped,
/// dry-runs via `ReviveApi.call` to derive gas + storage-deposit limits (and to
/// reject reverts before spending fees), then submits with a ~20% margin and
/// waits for finalization. Returns the finalized extrinsic hash.
pub async fn revive_call(
    client: &OnlineClient<AssetHubConfig>,
    signer: &Keypair,
    dest: H160,
    value: u128,
    calldata: Vec<u8>,
) -> Result<[u8; 32]> {
    ensure_mapped(client, signer).await?;

    let origin = account_id(signer);
    let dry = asset_hub::runtime_apis().revive_api().call(
        origin,
        dest,
        value,
        None,
        None,
        calldata.clone(),
    );
    let outcome = client
        .at_current_block()
        .await?
        .runtime_apis()
        .call(dry)
        .await
        .context("ReviveApi.call dry-run failed")?;

    let exec = match outcome.result {
        Ok(exec) => exec,
        Err(err) => bail!(
            "dry-run reverted, refusing to submit: {err:?} \
             (are you the owner of this name and is its resolver set?)"
        ),
    };
    if exec.flags.bits & 1 != 0 {
        bail!(
            "dry-run reverted (revert flag set), refusing to submit \
             (likely not the domain owner or the resolver is not configured)"
        );
    }

    let required = outcome.weight_required;
    let weight_limit = asset_hub::runtime_types::sp_weights::weight_v2::Weight {
        ref_time: required.ref_time + required.ref_time / 5,
        proof_size: required.proof_size + required.proof_size / 5,
    };
    let storage_deposit_limit = match outcome.storage_deposit {
        asset_hub::runtime_types::pallet_revive::primitives::StorageDeposit::Charge(v) => {
            v + v / 5 + 1
        }
        asset_hub::runtime_types::pallet_revive::primitives::StorageDeposit::Refund(_) => 0,
    };

    let call =
        asset_hub::tx()
            .revive()
            .call(dest, value, weight_limit, storage_deposit_limit, calldata);
    let events = client
        .tx()
        .await?
        .sign_and_submit_then_watch_default(&call, signer)
        .await
        .context("submitting Revive.call")?
        .wait_for_finalized_success()
        .await
        .context("Revive.call did not finalize successfully")?;
    Ok(events.extrinsic_hash().0)
}

/// Submit a signed `Balances.transfer_keep_alive` of `value` plancks to `dest`,
/// signed via [`AssetHubConfig`]. Returns the finalized extrinsic hash.
pub async fn transfer_keep_alive(
    env: &Env,
    signer: &Keypair,
    dest: AccountId32,
    value: u128,
) -> Result<[u8; 32]> {
    let client = asset_hub_client(env).await?;
    let call = asset_hub::tx()
        .balances()
        .transfer_keep_alive(subxt::utils::MultiAddress::Id(dest), value);
    let events = client
        .tx()
        .await?
        .sign_and_submit_then_watch_default(&call, signer)
        .await
        .context("submitting Balances.transfer_keep_alive")?
        .wait_for_finalized_success()
        .await
        .context("Balances.transfer_keep_alive did not finalize successfully")?;
    Ok(events.extrinsic_hash().0)
}

/// Bind a normalized `.dot` `name` to `cid` by submitting a signed
/// `setContenthash(node, 0xe301 ++ cid)` to the env's DotNS content resolver on
/// the given Asset Hub client. Returns the raw contenthash bytes that were set
/// (for read-back verification).
pub async fn set_contenthash(
    client: &OnlineClient<AssetHubConfig>,
    env: &Env,
    signer: &Keypair,
    name: &str,
    cid: &Cid,
) -> Result<Vec<u8>> {
    let node = dotns::namehash(name);
    let contenthash = dotns::cid_to_contenthash(cid);
    let calldata = dotns::encode_set_contenthash_call(node, &contenthash);
    let dest = parse_h160(env.dotns_content_resolver)?;

    let block = revive_call(client, signer, dest, 0, calldata).await?;
    println!("setContenthash finalized (tx 0x{})", hex::encode(block));
    Ok(contenthash)
}

/// Register an open-tier `.dot` `name` for `signer` via the commit/reveal flow on
/// the DotNS RegistrarController. Signs and submits `commit` then, after the
/// commitment matures, the payable `register`, and verifies ownership in the
/// Registry. Returns the owner H160 and the native value paid. `name` must be
/// normalized already.
pub async fn register_name(env: &Env, signer: &Keypair, name: &str) -> Result<(H160, u128)> {
    let label = name.strip_suffix(".dot").unwrap_or(name).to_string();

    let registrar = parse_h160(env.registrar_controller)?;
    let pop_rules = parse_h160(env.pop_rules)?;
    let registry = parse_h160(env.registry)?;

    let client = asset_hub_client(env).await?;
    ensure_mapped(&client, signer).await?;

    let origin = account_id(signer);
    let owner = client
        .at_current_block()
        .await?
        .runtime_apis()
        .call(
            asset_hub::runtime_apis()
                .revive_api()
                .address(origin.clone()),
        )
        .await
        .context("ReviveApi.address runtime call failed")?;

    let status_data = revive_view(
        &client,
        origin.clone(),
        pop_rules,
        0,
        registrar::encode_classify_name(&label),
    )
    .await?;
    let status = registrar::decode_classify_status(&status_data)?;
    if status != 0 {
        bail!(
            "{name} requires PoP tier {status} (not open); \
             trikit only supports open-tier registration"
        );
    }

    let price_data = revive_view(
        &client,
        origin.clone(),
        pop_rules,
        0,
        registrar::encode_price(&label, owner),
    )
    .await?;
    let price_wei = registrar::decode_price(&price_data)?;
    let value_native = registrar::register_value_native(price_wei)?;

    let mut secret = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut secret);

    let commitment_data = revive_view(
        &client,
        origin.clone(),
        registrar,
        0,
        registrar::encode_make_commitment(registrar::registration(&label, owner, secret)),
    )
    .await?;
    let commitment = registrar::decode_commitment(&commitment_data)?;

    println!(
        "committing {name} (commitment 0x{})",
        hex::encode(commitment)
    );
    let commit_tx = revive_call(
        &client,
        signer,
        registrar,
        0,
        registrar::encode_commit(commitment),
    )
    .await?;
    println!("commit   finalized (tx 0x{})", hex::encode(commit_tx));

    let age_data = revive_view(
        &client,
        origin.clone(),
        registrar,
        0,
        registrar::encode_min_commitment_age(),
    )
    .await?;
    let min_age = registrar::decode_min_commitment_age(&age_data)?;
    let wait = min_age + 6;
    println!("waiting {wait}s for commitment to mature");
    tokio::time::sleep(Duration::from_secs(wait)).await;

    println!("registering {name} (value {value_native} plancks)");
    let register_tx = revive_call(
        &client,
        signer,
        registrar,
        value_native,
        registrar::encode_register(registrar::registration(&label, owner, secret)),
    )
    .await?;
    println!("register finalized (tx 0x{})", hex::encode(register_tx));

    let node = dotns::namehash(name);
    let owner_data =
        revive_view(&client, origin, registry, 0, registrar::encode_owner(node)).await?;
    let onchain_owner = registrar::decode_owner(&owner_data)?;
    if onchain_owner != owner {
        bail!(
            "ownership verification failed: Registry owner is 0x{} but expected 0x{}",
            hex::encode(onchain_owner.0),
            hex::encode(owner.0)
        );
    }

    Ok((owner, value_native))
}
