//! Browse Publisher registry on Asset Hub (`pallet_revive`): list or retract a
//! `.dot` label so it shows up in Browse without users having to search for its
//! name.
//!
//! `publish(string label)` / `unpublish(string label)` are non-payable writes to
//! the env's Publisher contract (`paritytech/browse` `evm/src/Publisher.sol`).
//! The contract keys by the **bare label** (no `.dot`, no subdomains) and checks
//! the caller owns the name NFT (`registrar.ownerOf`). Callers other than the
//! contract owner are additionally personhood-gated (Lite tier ≥ 1) and
//! rate-limited per day; [`explain_publisher_revert`] turns those two
//! owner-hittable reverts into actionable messages. Only paseo-next-v2 has a
//! deployed Publisher today (`env.publisher`).

use crate::chain::config::asset_hub;
use crate::chain::revive::{parse_h160, revive_call};
use crate::chain::{account_id, asset_hub_client, revive_address};
use crate::dotns::{name_owner, normalize_name};
use crate::env::Env;
use alloy_sol_types::{sol, SolCall, SolError};
use anyhow::{bail, Context, Result};
use subxt::utils::H160;
use subxt_signer::sr25519::Keypair;

sol! {
    function publish(string label) external;
    function unpublish(string label) external;

    error NoPersonhood();
    error RateLimitExceeded(uint64 nextAvailableAt);
}

/// Outcome of a Publisher `publish`/`unpublish`.
pub struct PublishOutcome {
    pub label: String,
    pub publisher: H160,
    pub tx: [u8; 32],
}

/// List a `.dot` `name` in the Browse Publisher registry.
pub async fn publish(env: &Env, signer: &Keypair, name: &str) -> Result<PublishOutcome> {
    submit(env, signer, name, true).await
}

/// Retract a `.dot` `name` from the Browse Publisher registry (no rebuild needed).
pub async fn unpublish(env: &Env, signer: &Keypair, name: &str) -> Result<PublishOutcome> {
    submit(env, signer, name, false).await
}

async fn submit(env: &Env, signer: &Keypair, name: &str, publish: bool) -> Result<PublishOutcome> {
    let verb = if publish { "publish" } else { "unpublish" };
    if env.publisher.is_empty() {
        bail!(
            "--{verb} is not supported on env '{}' (no Publisher contract deployed; paseo-next-v2 only)",
            env.id
        );
    }
    let full = normalize_name(name);
    let label = publisher_label(&full)?;
    let publisher = parse_h160(env.publisher)?;
    let client = asset_hub_client(env).await?;

    // Pre-check ownership for a clear message instead of the contract's bare
    // NotOwner revert (the Registry owner matches the NFT for a normal name).
    let ours = revive_address(&client, account_id(signer)).await?;
    match name_owner(&client, env, &full).await? {
        Some(owner) if owner.0 == ours.0 => {}
        Some(owner) => bail!(
            "{full} is owned by 0x{} (not you); only the name owner can {verb} it in Browse",
            hex::encode(owner.0)
        ),
        None => bail!("{full} is not registered; register/own it before you can {verb} it"),
    }

    let calldata = if publish {
        publishCall {
            label: label.clone(),
        }
        .abi_encode()
    } else {
        unpublishCall {
            label: label.clone(),
        }
        .abi_encode()
    };

    // Dry-run first so known Publisher reverts become actionable messages;
    // unknown reverts fall through to revive_call's revert_reason.
    let dry = asset_hub::runtime_apis().revive_api().call(
        account_id(signer),
        publisher,
        0,
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
        .context("publish dry-run (ReviveApi.call) failed")?;
    if let Ok(exec) = &outcome.result {
        if exec.flags.bits & 1 != 0 {
            if let Some(hint) = explain_publisher_revert(&exec.data) {
                bail!("cannot {verb} {full}: {hint}");
            }
        }
    }

    let tx = revive_call(&client, signer, publisher, 0, calldata).await?;

    Ok(PublishOutcome {
        label,
        publisher,
        tx,
    })
}

/// Translate a Publisher custom-error revert into an actionable message. Decodes
/// the two selectors a legitimate owner can still hit (`NoPersonhood`,
/// `RateLimitExceeded`); returns `None` for anything else so the generic
/// `revert_reason` path can surface it verbatim.
fn explain_publisher_revert(data: &[u8]) -> Option<String> {
    let selector: [u8; 4] = data.get(..4)?.try_into().ok()?;
    if selector == NoPersonhood::SELECTOR {
        return Some(
            "publishing to Browse needs Lite or Full personhood. Get verified at \
             https://sudo.personhood.dev/personhood-faucet (env Next V2), bind it to the dotns \
             context via sudo.personhood.dev/dotns-bootstrap, or publish from a verified signer."
                .to_string(),
        );
    }
    if selector == RateLimitExceeded::SELECTOR {
        let decoded = RateLimitExceeded::abi_decode_raw(&data[4..]).ok()?;
        return Some(rate_limit_message(decoded.nextAvailableAt));
    }
    None
}

/// Human message for the Publisher's per-day publish cap (Lite 1/day, Full
/// 5/day), reporting when the next slot opens relative to the local clock.
fn rate_limit_message(next_available_at: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let base = "daily publish cap reached (Lite 1/day, Full 5/day)";
    if next_available_at > now {
        let mins = (next_available_at - now).div_ceil(60);
        format!("{base}; next publish allowed in ~{mins} min (unix {next_available_at}).")
    } else {
        format!("{base}; the window should be open now (unix {next_available_at}).")
    }
}

/// The bare, publishable label for a normalized `.dot` `name`: `.dot`-stripped,
/// with empty labels and subdomains rejected (the Publisher only keys base
/// `<label>.dot` nodes).
fn publisher_label(normalized: &str) -> Result<String> {
    let label = normalized.strip_suffix(".dot").unwrap_or(normalized);
    if label.is_empty() {
        bail!("empty label: nothing to publish");
    }
    if label.contains('.') {
        bail!("subdomains are not supported by the Publisher registry (publish the base <label>.dot only)");
    }
    Ok(label.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publisher_label_strips_and_validates() {
        assert_eq!(publisher_label("dotshare.dot").unwrap(), "dotshare");
        assert_eq!(publisher_label("dotshare").unwrap(), "dotshare");
        assert!(publisher_label("app.dotshare.dot").is_err());
        assert!(publisher_label(".dot").is_err());
    }

    #[test]
    fn selectors_match_wire_spec() {
        assert_eq!(hex::encode(publishCall::SELECTOR), "243e280b");
        assert_eq!(hex::encode(unpublishCall::SELECTOR), "2768c83a");
        assert_eq!(hex::encode(NoPersonhood::SELECTOR), "ceef23ac");
        assert_eq!(hex::encode(RateLimitExceeded::SELECTOR), "c366d2e5");
    }

    #[test]
    fn explains_known_publisher_reverts() {
        let no_pop = NoPersonhood {}.abi_encode();
        assert!(explain_publisher_revert(&no_pop)
            .unwrap()
            .contains("personhood"));

        let rate = RateLimitExceeded { nextAvailableAt: 0 }.abi_encode();
        assert!(explain_publisher_revert(&rate)
            .unwrap()
            .contains("daily publish cap"));

        // Unknown selector / short data fall through to the generic path.
        assert!(explain_publisher_revert(&[0xde, 0xad, 0xbe, 0xef]).is_none());
        assert!(explain_publisher_revert(&[0x00]).is_none());
    }
}
