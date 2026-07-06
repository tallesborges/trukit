//! `pallet_revive` plumbing on Asset Hub: H160 address resolution, account
//! mapping, EVM revert decoding, and the read-only (`revive_view`) / signed
//! (`revive_call`) contract-call entry points every DotNS operation builds on.

use super::config::{asset_hub, AssetHubConfig};
use super::signer::account_id;
use crate::ui;
use anyhow::{bail, Context, Result};
use subxt::utils::{AccountId32, H160};
use subxt::OnlineClient;
use subxt_signer::sr25519::Keypair;

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

pub fn parse_h160(addr: &str) -> Result<H160> {
    let raw = addr.strip_prefix("0x").unwrap_or(addr);
    let bytes = hex::decode(raw).with_context(|| format!("invalid H160 hex '{addr}'"))?;
    let arr: [u8; 20] = bytes
        .as_slice()
        .try_into()
        .with_context(|| format!("expected 20-byte H160, got {} bytes", bytes.len()))?;
    Ok(H160(arr))
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

    ui::note("account not mapped on Asset Hub; submitting Revive.map_account()…");
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
    ui::kv(
        "mapped",
        format!("tx 0x{}", hex::encode(events.extrinsic_hash().0)),
    );
    Ok(())
}

/// Best-effort human-readable reason from EVM revert returndata. Decodes the
/// standard `Error(string)` / `Panic(uint256)` shapes (and Vyper string reverts);
/// for a custom error surfaces its 4-byte selector, and for empty returndata
/// explains the common causes.
pub fn revert_reason(data: &[u8]) -> String {
    if let Some(reason) = alloy_sol_types::decode_revert_reason(data) {
        return reason;
    }
    if data.is_empty() {
        return "no reason returned (empty revert — often an unmet require() without a message, \
                an unauthorized caller, or a call to an address with no contract code)"
            .to_string();
    }
    if data.len() >= 4 {
        let selector = hex::encode(&data[..4]);
        // Many custom errors are `SomeError(string)`: a 4-byte selector followed
        // by an ABI-encoded string. Surface that message directly when present.
        if let Ok(msg) = <String as alloy_sol_types::SolValue>::abi_decode(&data[4..]) {
            if !msg.is_empty() {
                return format!("custom error 0x{selector}: {msg:?}");
            }
        }
        return format!(
            "custom error 0x{selector} (returndata 0x{})",
            hex::encode(data)
        );
    }
    format!("unrecognized revert (returndata 0x{})", hex::encode(data))
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
        Err(err) => bail!("contract call failed on chain: {err:?}"),
    };
    if exec.flags.bits & 1 != 0 {
        bail!("contract call reverted: {}", revert_reason(&exec.data));
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
        Err(err) => bail!("dry-run failed on chain, refusing to submit: {err:?}"),
    };
    if exec.flags.bits & 1 != 0 {
        bail!(
            "dry-run reverted, refusing to submit: {}",
            revert_reason(&exec.data)
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
