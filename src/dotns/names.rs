//! DotNS naming operations on Asset Hub: resolver reads/writes (contenthash and
//! text records), registry ownership, PoP classification and pricing, name-NFT
//! transfers, and the commit/reveal registration flow. Built on the
//! [`crate::chain::revive`] contract-call primitives and the resolver /
//! registrar ABI helpers in this module's siblings.

use super::registrar_abi as registrar;
use super::resolver as dotns;
use crate::chain::asset_hub::asset_hub_client;
use crate::chain::config::{asset_hub, AssetHubConfig};
use crate::chain::revive::{
    ensure_mapped, parse_h160, revert_reason, revive_address, revive_call, revive_view,
};
use crate::chain::signer::{account_id, build_signer};
use crate::env::Env;
use crate::ui;
use anyhow::{bail, Context, Result};
use cid::Cid;
use rand::RngCore;
use std::str::FromStr;
use std::time::Duration;
use subxt::utils::{AccountId32, H160};
use subxt::OnlineClient;
use subxt_signer::sr25519::Keypair;

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
        bail!(
            "resolver contenthash call reverted: {}",
            revert_reason(&exec.data)
        );
    }
    dotns::decode_contenthash_return(&exec.data)
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
    ui::kv("tx", format!("0x{}", hex::encode(block)));
    Ok(contenthash)
}

/// Read a `.dot` name's `key` text record via the resolver's `text(bytes32,string)`
/// dry-run. Empty string when unset. `name` must be normalized.
pub async fn resolve_text(
    client: &OnlineClient<AssetHubConfig>,
    env: &Env,
    name: &str,
    key: &str,
) -> Result<String> {
    let node = dotns::namehash(name);
    let calldata = dotns::encode_text_call(node, key);
    let dest = parse_h160(env.dotns_content_resolver)?;
    let origin = account_id(&build_signer(None, None)?);

    let data = revive_view(client, origin, dest, 0, calldata).await?;
    dotns::decode_text_return(&data)
}

/// Set a `.dot` name's `key` text record via a signed `setText`. Returns the
/// finalized extrinsic hash. `name` must be normalized.
pub async fn set_text(
    client: &OnlineClient<AssetHubConfig>,
    env: &Env,
    signer: &Keypair,
    name: &str,
    key: &str,
    value: &str,
) -> Result<[u8; 32]> {
    let node = dotns::namehash(name);
    let calldata = dotns::encode_set_text_call(node, key, value);
    let dest = parse_h160(env.dotns_content_resolver)?;

    let block = revive_call(client, signer, dest, 0, calldata).await?;
    ui::kv("tx", format!("0x{}", hex::encode(block)));
    Ok(block)
}

/// DotNS Registry owner of a normalized `.dot` `name`, or `None` if unregistered
/// (ENS `owner(bytes32)` maps unknown nodes to the zero address). Needs `env.registry`.
pub async fn name_owner(
    client: &OnlineClient<AssetHubConfig>,
    env: &Env,
    name: &str,
) -> Result<Option<H160>> {
    let node = dotns::namehash(name);
    let registry = parse_h160(env.registry)?;
    let origin = account_id(&build_signer(None, None)?);
    let data = revive_view(client, origin, registry, 0, registrar::encode_owner(node)).await?;
    let owner = registrar::decode_owner(&data)?;
    Ok((owner.0 != [0u8; 20]).then_some(owner))
}

/// Classify a `.dot` `name` via the PoP rules' `classifyName`, returning the
/// required personhood `(tier, status)` where `status` is a human availability
/// string. Reverts (surfaced to the caller) for labels that break the digit-suffix
/// rule. `name` may be with or without the `.dot` suffix.
pub async fn classify_name(
    client: &OnlineClient<AssetHubConfig>,
    env: &Env,
    name: &str,
) -> Result<(u8, String)> {
    let label = name.strip_suffix(".dot").unwrap_or(name);
    let pop_rules = parse_h160(env.pop_rules)?;
    let origin = account_id(&build_signer(None, None)?);
    let data = revive_view(
        client,
        origin,
        pop_rules,
        0,
        registrar::encode_classify_name(label),
    )
    .await?;
    registrar::decode_classify(&data)
}

/// The base list price (native plancks, no registration margin) of a `.dot`
/// `name` for `owner`, via the PoP rules' `priceWithoutCheck`. `name` may carry
/// the `.dot` suffix or not.
pub async fn name_price_native(
    client: &OnlineClient<AssetHubConfig>,
    env: &Env,
    name: &str,
    owner: H160,
) -> Result<u128> {
    let label = name.strip_suffix(".dot").unwrap_or(name);
    let pop_rules = parse_h160(env.pop_rules)?;
    let origin = account_id(&build_signer(None, None)?);
    let data = revive_view(
        client,
        origin,
        pop_rules,
        0,
        registrar::encode_price(label, owner),
    )
    .await?;
    registrar::base_price_native(registrar::decode_price(&data)?)
}

/// Resolve a transfer recipient given as either a `0x`-prefixed H160 or an SS58
/// address (mapped to its H160 via `ReviveApi.address`).
async fn resolve_recipient(client: &OnlineClient<AssetHubConfig>, to: &str) -> Result<H160> {
    let to = to.trim();
    if to.starts_with("0x") && to.len() == 42 {
        return parse_h160(to);
    }
    let account = AccountId32::from_str(to).map_err(|e| {
        anyhow::anyhow!("recipient '{to}' is neither a 0x H160 nor a valid SS58 address: {e}")
    })?;
    revive_address(client, account).await
}

/// Outcome of a name-NFT transfer.
pub struct TransferOutcome {
    pub from: H160,
    pub to: H160,
    pub fee_native: u128,
    pub tx: [u8; 32],
}

/// Transfer the `.dot` `name` (an ERC721 on the DotNS Registrar) from the signer
/// to `to_raw` (a `0x` H160 or an SS58 address). Prechecks NFT ownership so we
/// fail before spending fees, quotes the friction fee (0 for same-tier/upward
/// moves) and pays it as the payable `transferFrom` call value, then verifies
/// `ownerOf` flipped to the recipient. `name` must be normalized.
pub async fn transfer_name(
    env: &Env,
    signer: &Keypair,
    name: &str,
    to_raw: &str,
) -> Result<TransferOutcome> {
    if env.registrar.is_empty() {
        bail!(
            "name transfer is not supported on env '{}' (no registrar NFT address configured)",
            env.id
        );
    }
    let registrar_addr = parse_h160(env.registrar)?;
    let client = asset_hub_client(env).await?;
    ensure_mapped(&client, signer).await?;

    let origin = account_id(signer);
    let from = revive_address(&client, origin).await?;
    let to = resolve_recipient(&client, to_raw).await?;
    let token_id = registrar::token_id(dotns::namehash(name));

    let owner_data = revive_view(
        &client,
        origin,
        registrar_addr,
        0,
        registrar::encode_owner_of(token_id),
    )
    .await?;
    let current = registrar::decode_owner_of(&owner_data)?;
    if current.0 == [0u8; 20] {
        bail!("{name} is not registered (no name NFT minted); nothing to transfer");
    }
    if current.0 != from.0 {
        bail!(
            "{name} is owned by 0x{} (not you); only the owner can transfer it",
            hex::encode(current.0)
        );
    }
    if to.0 == from.0 {
        bail!(
            "refusing to transfer {name} to its current owner (0x{})",
            hex::encode(to.0)
        );
    }

    let fee_data = revive_view(
        &client,
        origin,
        registrar_addr,
        0,
        registrar::encode_quote_transfer_fee(token_id, to),
    )
    .await?;
    let fee_native = registrar::fee_value_native(registrar::decode_quote_transfer_fee(&fee_data)?)?;

    ui::step(format!("transfer {name} → 0x{}", hex::encode(to.0)));
    if fee_native > 0 {
        ui::kv("fee", format!("{fee_native} plancks"));
    }
    let calldata = registrar::encode_transfer_from(from, to, token_id);
    let tx = revive_call(&client, signer, registrar_addr, fee_native, calldata).await?;
    ui::kv("tx", format!("0x{}", hex::encode(tx)));

    let after_data = revive_view(
        &client,
        origin,
        registrar_addr,
        0,
        registrar::encode_owner_of(token_id),
    )
    .await?;
    let after = registrar::decode_owner_of(&after_data)?;
    if after.0 != to.0 {
        bail!(
            "transfer submitted but ownerOf is still 0x{} (expected 0x{})",
            hex::encode(after.0),
            hex::encode(to.0)
        );
    }

    Ok(TransferOutcome {
        from,
        to,
        fee_native,
        tx,
    })
}

/// Ensure `signer` owns `name` before a deploy binds to it: proceed if already
/// theirs; register open-tier when `allow_register` and it's unregistered; error
/// if it's taken. No-op when the env has no registry (the bind dry-run enforces
/// ownership instead).
pub async fn ensure_domain(
    client: &OnlineClient<AssetHubConfig>,
    env: &Env,
    signer: &Keypair,
    name: &str,
    allow_register: bool,
) -> Result<()> {
    if env.registry.is_empty() {
        if allow_register {
            bail!(
                "--register is not supported on env '{}' (no registrar addresses configured)",
                env.id
            );
        }
        return Ok(());
    }

    let ours = revive_address(client, account_id(signer)).await?;
    match name_owner(client, env, name).await? {
        Some(owner) if owner.0 == ours.0 => Ok(()),
        Some(owner) => bail!(
            "{name} is registered to 0x{} (not you); deploy requires a name you own",
            hex::encode(owner.0)
        ),
        None if !allow_register => {
            let label = name.strip_suffix(".dot").unwrap_or(name);
            bail!(
                "{name} is not registered — run `dotkit asset-hub name register {label}` first, \
                 or pass --register to register it now (open-tier, costs PAS)"
            )
        }
        None => {
            let (_, value) = register_name(env, signer, name).await?;
            ui::success(format!("registered {name} (~{} PAS)", value as f64 / 1e10));
            Ok(())
        }
    }
}

/// Selector of `CommitmentTooNew(bytes32,uint256,uint256)` — returned by the
/// registrar's `register` while the commitment is still maturing.
const COMMITMENT_TOO_NEW: [u8; 4] = [0x74, 0x48, 0x0c, 0xc9];
/// Give up waiting for a commitment to mature after this long.
const COMMIT_MATURITY_TIMEOUT: Duration = Duration::from_secs(120);
/// Delay between `register` dry-run probes while the commitment matures.
const COMMIT_POLL_INTERVAL: Duration = Duration::from_secs(4);

/// Poll the `register` dry-run until the commitment matures.
///
/// A commitment is only valid `minCommitmentAge` seconds after `commit`, but the
/// dry-run evaluates against the (lagging) finalized block, so a fixed wall-clock
/// sleep races the on-chain clock and reverts with `CommitmentTooNew`. Re-run the
/// dry-run until it stops returning that error; a different revert or a chain
/// error fails immediately, and the whole wait is bounded by a timeout.
pub(super) async fn await_commitment_mature(
    client: &OnlineClient<AssetHubConfig>,
    origin: AccountId32,
    registrar_addr: H160,
    value: u128,
    register_calldata: &[u8],
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + COMMIT_MATURITY_TIMEOUT;
    loop {
        let call = asset_hub::runtime_apis().revive_api().call(
            origin,
            registrar_addr,
            value,
            None,
            None,
            register_calldata.to_vec(),
        );
        let outcome = client
            .at_current_block()
            .await?
            .runtime_apis()
            .call(call)
            .await
            .context("register dry-run (ReviveApi.call) failed")?;

        match outcome.result {
            Ok(exec) if exec.flags.bits & 1 == 0 => {
                ui::progress_clear();
                return Ok(());
            }
            Ok(exec) if exec.data.starts_with(&COMMITMENT_TOO_NEW) => {
                if tokio::time::Instant::now() >= deadline {
                    ui::progress_clear();
                    bail!(
                        "commitment still maturing after {}s (chain reports CommitmentTooNew)",
                        COMMIT_MATURITY_TIMEOUT.as_secs()
                    );
                }
                ui::progress("waiting for commitment to mature…");
                tokio::time::sleep(COMMIT_POLL_INTERVAL).await;
            }
            Ok(exec) => {
                ui::progress_clear();
                bail!("register dry-run reverted: {}", revert_reason(&exec.data));
            }
            Err(err) => {
                ui::progress_clear();
                bail!("register dry-run failed on chain: {err:?}");
            }
        }
    }
}

/// pallet_revive personhood precompile — fixed runtime address, same across envs.
const PERSONHOOD_PRECOMPILE: &str = "0x000000000000000000000000000000000a010000";

/// Human name for a personhood tier byte (matches DotNS `PopStatus`).
pub fn tier_name(tier: u8) -> &'static str {
    match tier {
        0 => "NoStatus",
        1 => "Lite",
        2 => "Full",
        3 => "Reserved",
        _ => "unknown",
    }
}

/// Read `owner`'s DotNS personhood tier from the Asset Hub personhood precompile
/// (`personhoodStatus(owner, "dotns")`): 0 NoStatus / 1 Lite / 2 Full / 3 Reserved.
async fn personhood_status(client: &OnlineClient<AssetHubConfig>, owner: H160) -> Result<u8> {
    let mut context = [0u8; 32];
    context[..5].copy_from_slice(b"dotns");
    let dest = parse_h160(PERSONHOOD_PRECOMPILE)?;
    let origin = account_id(&build_signer(None, None)?);
    let calldata = registrar::encode_personhood_status(owner, context);
    let data = revive_view(client, origin, dest, 0, calldata).await?;
    registrar::decode_personhood_status(&data)
}

/// Register a `.dot` `name` for `signer` via the commit/reveal flow on the DotNS
/// RegistrarController. Handles open (tier 0) and personhood-gated Lite/Full (tier
/// 1/2) names — for the latter it pre-checks the owner's personhood so we fail
/// before committing; Reserved (tier 3) is rejected. Signs `commit`, waits for the
/// commitment to mature, submits the payable `register`, and verifies ownership in
/// the Registry. Returns the owner H160 and the native value paid. `name` must be
/// normalized already.
pub async fn register_name(env: &Env, signer: &Keypair, name: &str) -> Result<(H160, u128)> {
    let label = name.strip_suffix(".dot").unwrap_or(name).to_string();

    let registrar_addr = parse_h160(env.registrar_controller)?;
    let pop_rules = parse_h160(env.pop_rules)?;
    let registry = parse_h160(env.registry)?;

    let client = asset_hub_client(env).await?;
    ensure_mapped(&client, signer).await?;

    let origin = account_id(signer);
    let owner = client
        .at_current_block()
        .await?
        .runtime_apis()
        .call(asset_hub::runtime_apis().revive_api().address(origin))
        .await
        .context("ReviveApi.address runtime call failed")?;

    let status_data = revive_view(
        &client,
        origin,
        pop_rules,
        0,
        registrar::encode_classify_name(&label),
    )
    .await?;
    let required = registrar::decode_classify_status(&status_data)?;
    if required == 3 {
        bail!("{name} classifies as Reserved (governance tier); dotkit does not register reserved names");
    }
    if required >= 1 {
        // Personhood-gated name: the owner must already hold Lite/Full personhood
        // in the DotNS context, else `register` reverts on-chain. Check up front so
        // we fail before spending a commit.
        let have = personhood_status(&client, owner).await?;
        if have < required {
            bail!(
                "{name} requires {} personhood, but the signer (0x{}) has {}. \
                 Get verified at https://sudo.personhood.dev/personhood-faucet (env Next V2), \
                 or pass --mnemonic for a verified account.",
                tier_name(required),
                hex::encode(owner.0),
                tier_name(have),
            );
        }
        ui::note(format!(
            "personhood ok — signer has {} (name needs {})",
            tier_name(have),
            tier_name(required)
        ));
    }

    let price_data = revive_view(
        &client,
        origin,
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
        origin,
        registrar_addr,
        0,
        registrar::encode_make_commitment(registrar::registration(&label, owner, secret)),
    )
    .await?;
    let commitment = registrar::decode_commitment(&commitment_data)?;

    ui::step(format!("commit {name}"));
    ui::kv("commitment", format!("0x{}", hex::encode(commitment)));
    let commit_tx = revive_call(
        &client,
        signer,
        registrar_addr,
        0,
        registrar::encode_commit(commitment),
    )
    .await?;
    ui::kv("tx", format!("0x{}", hex::encode(commit_tx)));

    let age_data = revive_view(
        &client,
        origin,
        registrar_addr,
        0,
        registrar::encode_min_commitment_age(),
    )
    .await?;
    let min_age = registrar::decode_min_commitment_age(&age_data)?;

    ui::step(format!("register {name}"));
    ui::kv("value", format!("{value_native} plancks"));
    let register_calldata =
        registrar::encode_register(registrar::registration(&label, owner, secret));
    // A commitment can't be valid before `min_age` seconds — wait that floor,
    // then poll the dry-run until the lagging finalized clock also agrees.
    tokio::time::sleep(Duration::from_secs(min_age)).await;
    await_commitment_mature(
        &client,
        origin,
        registrar_addr,
        value_native,
        &register_calldata,
    )
    .await?;
    let register_tx = revive_call(
        &client,
        signer,
        registrar_addr,
        value_native,
        register_calldata,
    )
    .await?;
    ui::kv("tx", format!("0x{}", hex::encode(register_tx)));

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
