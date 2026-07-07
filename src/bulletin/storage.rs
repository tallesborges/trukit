//! Bulletin chain `TransactionStorage`: content-addressed block storage
//! (idempotent single-block and concurrent CAR-batch uploads), authorization,
//! and the CID / content-hash helpers the upload layer keys blocks by.

use crate::chain::config::{bulletin, BulletinConfig};
use crate::chain::metadata::connect_with_cache;
use crate::chain::signer::account_id;
use crate::env::Env;
use anyhow::{bail, Context, Result};
use cid::Cid;
use futures::StreamExt;
use multihash_codetable::{Code, MultihashDigest};
use std::collections::HashSet;
use std::time::Duration;
use subxt::config::transaction_extensions as tx_ext;
use subxt::utils::AccountId32;
use subxt::OnlineClient;
use subxt_signer::sr25519::Keypair;

/// Chain-enforced `MaxTransactionSize` (2 MiB) — the largest blob one store
/// extrinsic can carry.
pub const MAX_TRANSACTION_SIZE: usize = 2 * 1024 * 1024;

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

/// Read `TransactionByContentHash` at `at` and decode the stored `(block, index)`
/// location, or `None` when the content hash isn't stored yet. The single source
/// of this storage read/decode, shared by the batch probe and `store_block`.
async fn stored_location(
    at: &subxt::client::ClientAtBlock<
        BulletinConfig,
        impl subxt::client::OnlineClientAtBlockT<BulletinConfig>,
    >,
    content_hash: [u8; 32],
) -> Result<Option<(u32, u32)>> {
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
    match existing {
        Some(value) => Ok(Some(value.decode().context("decoding stored location")?)),
        None => Ok(None),
    }
}

async fn is_stored(
    at: &subxt::client::ClientAtBlock<
        BulletinConfig,
        impl subxt::client::OnlineClientAtBlockT<BulletinConfig>,
    >,
    content_hash: [u8; 32],
) -> Result<bool> {
    Ok(stored_location(at, content_hash).await?.is_some())
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
    if let Some((block, index)) = stored_location(&at, content_hash).await? {
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
    let (block, index) = stored_location(&at, content_hash)
        .await?
        .context("store finalized but TransactionByContentHash is still empty")?;
    Ok(StoreOutcome::Stored { block, index })
}

/// Authorize `who` for Bulletin `TransactionStorage` with a `transactions`/`bytes`
/// quota, submitting a signed `authorize_account` extrinsic. The `signer` must
/// hold Authorizer privileges on the chain, else the extrinsic fails with
/// `BadOrigin` (surfaced to the caller). Returns the finalized extrinsic hash.
pub async fn authorize_bulletin_account(
    client: &OnlineClient<BulletinConfig>,
    signer: &Keypair,
    who: AccountId32,
    transactions: u32,
    bytes: u64,
) -> Result<[u8; 32]> {
    let call = bulletin::tx()
        .transaction_storage()
        .authorize_account(who, transactions, bytes);
    let events = client
        .tx()
        .await?
        .sign_and_submit_then_watch_default(&call, signer)
        .await
        .context("submitting TransactionStorage.authorize_account")?
        .wait_for_finalized_success()
        .await
        .context(
            "authorize_account did not finalize successfully \
             (the signer must hold Bulletin Authorizer privileges)",
        )?;
    Ok(events.extrinsic_hash().0)
}

/// Decoded Bulletin `TransactionStorage` authorization extent for an account.
pub struct AuthInfo {
    pub transactions: u32,
    pub transactions_allowance: u32,
    pub bytes: u64,
    pub bytes_allowance: u64,
    pub expiration: u32,
}

/// Read an account's Bulletin authorization + quota, or `None` if unauthorized.
pub async fn authorization(
    client: &OnlineClient<BulletinConfig>,
    who: &AccountId32,
) -> Result<Option<AuthInfo>> {
    let scope = bulletin::runtime_types::pallet_bulletin_transaction_storage::types::AuthorizationScope::Account(who.clone());
    let address = bulletin::storage().transaction_storage().authorizations();
    let at = client.at_current_block().await?;
    let got = at
        .storage()
        .try_fetch(address, (scope,))
        .await
        .context("reading TransactionStorage.Authorizations")?;
    match got {
        Some(v) => {
            let a = v.decode().context("decoding Authorization")?;
            let e = a.extent;
            Ok(Some(AuthInfo {
                transactions: e.transactions,
                transactions_allowance: e.transactions_allowance,
                bytes: e.bytes,
                bytes_allowance: e.bytes_allowance,
                expiration: a.expiration,
            }))
        }
        None => Ok(None),
    }
}

/// Whether `who` currently holds a Bulletin `TransactionStorage` authorization.
pub async fn is_authorized(client: &OnlineClient<BulletinConfig>, who: &AccountId32) -> Result<bool> {
    Ok(authorization(client, who).await?.is_some())
}

/// Authorize many accounts in a single `utility.batch_all`, signed by an
/// Authorizer. Atomic: if any inner `authorize_account` fails the whole batch
/// rolls back. The signer must hold Bulletin Authorizer privileges (else
/// `BadOrigin`). Returns the finalized extrinsic hash.
pub async fn batch_authorize_accounts(
    client: &OnlineClient<BulletinConfig>,
    signer: &Keypair,
    accounts: &[AccountId32],
    transactions: u32,
    bytes: u64,
) -> Result<[u8; 32]> {
    let calls: Vec<bulletin::runtime_types::bulletin_paseo_runtime::RuntimeCall> = accounts
        .iter()
        .map(|who| {
            bulletin::runtime_types::bulletin_paseo_runtime::RuntimeCall::TransactionStorage(
                bulletin::runtime_types::pallet_bulletin_transaction_storage::pallet::Call::authorize_account {
                    who: who.clone(),
                    transactions,
                    bytes,
                },
            )
        })
        .collect();
    let call = bulletin::tx().utility().batch_all(calls);
    let events = client
        .tx()
        .await?
        .sign_and_submit_then_watch_default(&call, signer)
        .await
        .context("submitting utility.batch_all(authorize_account)")?
        .wait_for_finalized_success()
        .await
        .context(
            "batch_all authorize did not finalize successfully \
             (the signer must hold Bulletin Authorizer privileges)",
        )?;
    Ok(events.extrinsic_hash().0)
}
