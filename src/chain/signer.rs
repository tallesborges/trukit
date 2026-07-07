//! sr25519 signer construction: the shared dev phrase, mnemonic-derived signers,
//! and the Bulletin storage pool accounts.

use anyhow::{Context, Result};
use rand::Rng;
use std::str::FromStr;
use subxt::utils::AccountId32;
use subxt_signer::{sr25519::Keypair, SecretUri};

/// Standard Substrate dev phrase. Its base account (empty derivation) is the
/// shared dev-mode DotNS owner on testnets; derived sub-accounts form the
/// authorized Bulletin storage pool.
pub const DEV_PHRASE: &str =
    "bottom drive obey lake curtain smoke basket hold race lonely fit walk";

/// Build an sr25519 signer from a mnemonic (+ optional derivation path). Defaults
/// to the shared dev account so `dotkit` owns the same dev-mode names on testnets.
/// Never logs the mnemonic.
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

/// A random authorized account from the **shared** Bulletin storage pool
/// (`//deploy/{0..9}` off the dev phrase), spreading load and cutting nonce
/// contention across concurrent uploads. Only these derivations hold Bulletin
/// quota on testnets — the base owner signer does not. For the per-machine
/// private pool, see [`crate::pool::pool_signer`].
pub fn shared_pool_signer() -> Result<Keypair> {
    let n = rand::thread_rng().gen_range(0u32..=9);
    build_signer(Some(DEV_PHRASE), Some(&format!("//deploy/{n}")))
}
