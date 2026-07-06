//! Generic Asset Hub operations: opening a cache-seeded client, native balance
//! transfers, and reading an account's native (PAS) balance.

use super::config::{asset_hub, AssetHubConfig};
use super::metadata::connect_with_cache;
use crate::env::Env;
use anyhow::{Context, Result};
use subxt::utils::AccountId32;
use subxt::OnlineClient;
use subxt_signer::sr25519::Keypair;

/// Open an Asset Hub client using the bespoke [`AssetHubConfig`] so signed
/// extrinsics carry the full 17-extension payload the runtime expects. The
/// client's metadata cache is pre-seeded from the persistent on-disk cache.
pub async fn asset_hub_client(env: &Env) -> Result<OnlineClient<AssetHubConfig>> {
    connect_with_cache(env.asset_hub_rpc, |metadata_cache| AssetHubConfig {
        metadata_cache,
    })
    .await
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

/// Native (PAS) free + reserved balance of `account` on Asset Hub, read from
/// `System.Account`. Zero when the account has no on-chain record yet.
pub async fn account_balance(
    client: &OnlineClient<AssetHubConfig>,
    account: AccountId32,
) -> Result<(u128, u128)> {
    let at = client.at_current_block().await?;
    let info = at
        .storage()
        .try_fetch(asset_hub::storage().system().account(), (account,))
        .await
        .context("reading System.Account")?;
    match info {
        Some(value) => {
            let account = value.decode().context("decoding AccountInfo")?;
            Ok((account.data.free, account.data.reserved))
        }
        None => Ok((0, 0)),
    }
}
