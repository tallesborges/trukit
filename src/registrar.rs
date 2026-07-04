//! DotNS RegistrarController + Registry ABI — commit/reveal registration on
//! Asset Hub via `pallet_revive`.
//!
//! Registration gotchas, all learned by decoding on-chain reverts (surfaced by
//! `chain::revert_reason`):
//!
//! - **Label digit rule**: a label must end in *no digits* or *exactly 2 digits*.
//!   Anything else (e.g. `myapp1` or `myapp1234`) makes `classifyName` revert with
//!   custom error `0x2dfc7d98` ("Name must have no digit suffix or exactly 2 digit
//!   suffix"). `register_name` hits this on its first read, before any commit.
//! - **Commit/reveal timing**: `register` reverts with `CommitmentTooNew`
//!   (`0x74480cc9`) until the commitment matures. The dry-run evaluates against the
//!   lagging *finalized* block, so a fixed wall-clock sleep races the chain clock —
//!   poll the dry-run until it clears instead (see `chain::await_commitment_mature`).
//! - **Tier vs. availability**: `classifyName` returns `(tier, status)` where
//!   `status` is a human string like "Available to all"; tier `0` is open, higher
//!   tiers are PoP-gated (see the `dotns` skill / substrate-chain-toolkit for PoP).

use alloy_primitives::{Address, FixedBytes, U256};
use alloy_sol_types::{sol, SolCall};
use anyhow::{Context, Result};
use subxt::utils::H160;

sol! {
    struct Registration {
        string label;
        address owner;
        bytes32 secret;
        bool reserved;
    }

    struct Price {
        uint256 base;
        uint8 tier;
        uint8 discountTier;
        string status;
    }

    function classifyName(string name) external view returns (uint8, string);
    function priceWithoutCheck(string name, address owner) external view returns (Price);

    function makeCommitment(Registration r) external view returns (bytes32);
    function commit(bytes32 commitment) external;
    function minCommitmentAge() external view returns (uint256);
    function register(Registration r) external payable;

    function owner(bytes32 node) external view returns (address);
}

fn to_address(h: H160) -> Address {
    Address::from(h.0)
}

fn to_h160(a: Address) -> H160 {
    H160(a.into_array())
}

/// Build the `Registration` tuple for an open-tier name (`reserved = false`).
pub fn registration(label: &str, owner: H160, secret: [u8; 32]) -> Registration {
    Registration {
        label: label.to_string(),
        owner: to_address(owner),
        secret: FixedBytes::from(secret),
        reserved: false,
    }
}

pub fn encode_classify_name(label: &str) -> Vec<u8> {
    classifyNameCall {
        name: label.to_string(),
    }
    .abi_encode()
}

/// Decode `classifyName` -> the numeric PoP status (0 == open/NoStatus).
pub fn decode_classify_status(data: &[u8]) -> Result<u8> {
    let ret = classifyNameCall::abi_decode_returns(data).context("decoding classifyName return")?;
    Ok(ret._0)
}

pub fn encode_price(label: &str, owner: H160) -> Vec<u8> {
    priceWithoutCheckCall {
        name: label.to_string(),
        owner: to_address(owner),
    }
    .abi_encode()
}

/// Decode `priceWithoutCheck` -> the price in 18-decimal EVM wei. The contract
/// returns a single dynamic struct, so the return is ABI-wrapped (leading offset)
/// and modeled here as the `Price` struct rather than flat return values.
pub fn decode_price(data: &[u8]) -> Result<U256> {
    let ret =
        priceWithoutCheckCall::abi_decode_returns(data).context("decoding priceWithoutCheck")?;
    Ok(ret.base)
}

pub fn encode_make_commitment(r: Registration) -> Vec<u8> {
    makeCommitmentCall { r }.abi_encode()
}

pub fn decode_commitment(data: &[u8]) -> Result<[u8; 32]> {
    let ret = makeCommitmentCall::abi_decode_returns(data).context("decoding makeCommitment")?;
    Ok(ret.0)
}

pub fn encode_commit(commitment: [u8; 32]) -> Vec<u8> {
    commitCall {
        commitment: FixedBytes::from(commitment),
    }
    .abi_encode()
}

pub fn encode_min_commitment_age() -> Vec<u8> {
    minCommitmentAgeCall {}.abi_encode()
}

pub fn decode_min_commitment_age(data: &[u8]) -> Result<u64> {
    let ret =
        minCommitmentAgeCall::abi_decode_returns(data).context("decoding minCommitmentAge")?;
    u64::try_from(ret).context("minCommitmentAge does not fit in u64")
}

pub fn encode_register(r: Registration) -> Vec<u8> {
    registerCall { r }.abi_encode()
}

pub fn encode_owner(node: [u8; 32]) -> Vec<u8> {
    ownerCall {
        node: FixedBytes::from(node),
    }
    .abi_encode()
}

pub fn decode_owner(data: &[u8]) -> Result<H160> {
    let ret = ownerCall::abi_decode_returns(data).context("decoding Registry.owner")?;
    Ok(to_h160(ret))
}

/// Convert an 18-decimal EVM wei price into the native `Revive.call` value.
/// Applies the +10% margin the contract charges, ceiling the wei division,
/// then scales from 18-decimal wei to the 10-decimal native token (ratio 1e8).
pub fn register_value_native(price_wei: U256) -> Result<u128> {
    let scaled = price_wei * U256::from(11u64);
    let with_margin = (scaled + U256::from(9u64)) / U256::from(10u64);
    let native = with_margin / U256::from(100_000_000u64);
    u128::try_from(native).context("register value overflows u128")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_conversion_candidate() {
        let price_wei = U256::from(10_000_000_000_000_000_000u128);
        assert_eq!(register_value_native(price_wei).unwrap(), 110_000_000_000);
    }

    #[test]
    fn selectors_match_wire_spec() {
        assert_eq!(hex::encode(classifyNameCall::SELECTOR), "3017fa33");
        assert_eq!(hex::encode(priceWithoutCheckCall::SELECTOR), "dcd62573");
        assert_eq!(hex::encode(makeCommitmentCall::SELECTOR), "7a23df1d");
        assert_eq!(hex::encode(commitCall::SELECTOR), "f14fcbc8");
        assert_eq!(hex::encode(minCommitmentAgeCall::SELECTOR), "8d839ffe");
        assert_eq!(hex::encode(registerCall::SELECTOR), "b26675d5");
        assert_eq!(hex::encode(ownerCall::SELECTOR), "02571be3");
    }
}
