//! DotNS RegistrarController + Registry ABI — commit/reveal registration on
//! Asset Hub via `pallet_revive`.
//!
//! Registration gotchas, all learned by decoding on-chain reverts (surfaced by
//! [`crate::chain::revive::revert_reason`]):
//!
//! - **Label digit rule**: a label must end in *no digits* or *exactly 2 digits*.
//!   Anything else (e.g. `myapp1` or `myapp1234`) makes `classifyName` revert with
//!   custom error `0x2dfc7d98` ("Name must have no digit suffix or exactly 2 digit
//!   suffix"). `register_name` hits this on its first read, before any commit.
//! - **Commit/reveal timing**: `register` reverts with `CommitmentTooNew`
//!   (`0x74480cc9`) until the commitment matures. The dry-run evaluates against the
//!   lagging *finalized* block, so a fixed wall-clock sleep races the chain clock —
//!   poll the dry-run until it clears instead (see [`super::names::await_commitment_mature`]).
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

    function ownerOf(uint256 tokenId) external view returns (address);
    function quoteTransferFee(uint256 tokenId, address to) external view returns (uint256);
    function transferFrom(address from, address to, uint256 tokenId) external payable;

    struct PersonhoodInfo {
        uint8 status;
        bytes32 contextAlias;
    }

    function personhoodStatus(address account, bytes32 context) external view returns (PersonhoodInfo);
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
    Ok(decode_classify(data)?.0)
}

/// Decode `classifyName` -> `(tier, status)` where `status` is the human-readable
/// availability string (e.g. "Available to all"). Used by the `name lookup` view.
pub fn decode_classify(data: &[u8]) -> Result<(u8, String)> {
    let ret = classifyNameCall::abi_decode_returns(data).context("decoding classifyName return")?;
    Ok((ret._0, ret._1))
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

/// The ERC721 tokenId of a name is `uint256(namehash(name))` — the same node
/// hash the Registry keys by, reinterpreted big-endian as a 256-bit integer.
pub fn token_id(node: [u8; 32]) -> U256 {
    U256::from_be_bytes(node)
}

/// ABI-encode `ownerOf(uint256 tokenId)` on the DotNS Registrar (name NFT).
pub fn encode_owner_of(token_id: U256) -> Vec<u8> {
    ownerOfCall { tokenId: token_id }.abi_encode()
}

/// Decode `ownerOf` -> the current name-NFT holder (zero address if unminted).
pub fn decode_owner_of(data: &[u8]) -> Result<H160> {
    let ret = ownerOfCall::abi_decode_returns(data).context("decoding Registrar.ownerOf")?;
    Ok(to_h160(ret))
}

/// ABI-encode `quoteTransferFee(uint256 tokenId, address to)` — the friction fee
/// (in 18-decimal wei) the Registrar charges to move `tokenId` to `to`.
pub fn encode_quote_transfer_fee(token_id: U256, to: H160) -> Vec<u8> {
    quoteTransferFeeCall {
        tokenId: token_id,
        to: to_address(to),
    }
    .abi_encode()
}

/// Decode `quoteTransferFee` -> the required fee in 18-decimal EVM wei.
pub fn decode_quote_transfer_fee(data: &[u8]) -> Result<U256> {
    quoteTransferFeeCall::abi_decode_returns(data).context("decoding quoteTransferFee")
}

/// ABI-encode `transferFrom(address from, address to, uint256 tokenId)` — the
/// payable name-NFT transfer; the friction fee is sent as the call value.
pub fn encode_transfer_from(from: H160, to: H160, token_id: U256) -> Vec<u8> {
    transferFromCall {
        from: to_address(from),
        to: to_address(to),
        tokenId: token_id,
    }
    .abi_encode()
}

/// ABI-encode `personhoodStatus(account, context)` on the personhood precompile.
pub fn encode_personhood_status(account: H160, context: [u8; 32]) -> Vec<u8> {
    personhoodStatusCall {
        account: to_address(account),
        context: FixedBytes::from(context),
    }
    .abi_encode()
}

/// Decode `personhoodStatus` → the account's personhood tier (0 NoStatus /
/// 1 Lite / 2 Full / 3 Reserved) in the queried context.
pub fn decode_personhood_status(data: &[u8]) -> Result<u8> {
    let ret =
        personhoodStatusCall::abi_decode_returns(data).context("decoding personhoodStatus")?;
    Ok(ret.status)
}

/// Wei per native planck: 18-decimal EVM wei scaled to the 10-decimal native
/// token is a ratio of 1e8. Shared by every wei→native conversion below.
const WEI_PER_NATIVE_PLANCK: u64 = 100_000_000;

/// Convert an 18-decimal EVM wei price into the native `Revive.call` value.
/// Applies the +10% margin the contract charges, ceiling the wei division,
/// then scales from 18-decimal wei to the 10-decimal native token (ratio 1e8).
pub fn register_value_native(price_wei: U256) -> Result<u128> {
    let scaled = price_wei * U256::from(11u64);
    let with_margin = (scaled + U256::from(9u64)) / U256::from(10u64);
    let native = with_margin / U256::from(WEI_PER_NATIVE_PLANCK);
    u128::try_from(native).context("register value overflows u128")
}

/// Convert an 18-decimal EVM wei fee (e.g. `quoteTransferFee`) into native
/// plancks, ceiling the wei→native division so we never underpay and trip
/// `TransferFeeRequired`. No margin — the quote is already the exact fee.
pub fn fee_value_native(fee_wei: U256) -> Result<u128> {
    let ratio = U256::from(WEI_PER_NATIVE_PLANCK);
    let native = (fee_wei + ratio - U256::from(1u64)) / ratio;
    u128::try_from(native).context("transfer fee overflows u128")
}

/// Convert an 18-decimal EVM wei price into native plancks (floored, no margin) —
/// the base "list price" shown by `name lookup`, distinct from the payable
/// [`register_value_native`] which adds the contract's +10% registration margin.
pub fn base_price_native(price_wei: U256) -> Result<u128> {
    let native = price_wei / U256::from(WEI_PER_NATIVE_PLANCK);
    u128::try_from(native).context("price overflows u128")
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
    fn fee_conversion_ceils() {
        // 1 PAS fee = 1e8 wei exactly -> 1 planck.
        assert_eq!(fee_value_native(U256::from(100_000_000u64)).unwrap(), 1);
        // Sub-planck dust ceils up so we never underpay TransferFeeRequired.
        assert_eq!(fee_value_native(U256::from(1u64)).unwrap(), 1);
        assert_eq!(fee_value_native(U256::from(0u64)).unwrap(), 0);
    }

    #[test]
    fn token_id_is_namehash_be() {
        let node = [
            0x99, 0xbc, 0x92, 0xdb, 0x90, 0x0d, 0xea, 0xaf, 0xbb, 0xe9, 0xbc, 0xc9, 0x8e, 0x6f,
            0xca, 0x31, 0x63, 0x02, 0x51, 0x52, 0x20, 0xc2, 0x72, 0x9a, 0x86, 0x1d, 0x59, 0x0c,
            0xc6, 0xb1, 0x92, 0x6a,
        ];
        assert_eq!(token_id(node), U256::from_be_bytes(node));
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
        assert_eq!(hex::encode(personhoodStatusCall::SELECTOR), "886af133");
        // Standard ERC721 selectors on the name-NFT Registrar.
        assert_eq!(hex::encode(ownerOfCall::SELECTOR), "6352211e");
        assert_eq!(hex::encode(transferFromCall::SELECTOR), "23b872dd");
    }

    #[test]
    fn personhood_status_decodes() {
        // Real returndata from the personhood precompile on paseo-next-v2 for a
        // Full account: status word (=2) ++ 32-byte context alias.
        let data = hex::decode(
            "0000000000000000000000000000000000000000000000000000000000000002\
             abff28c3a6547093e759274350d8640312a2073bfc0584896af86be939496e25",
        )
        .unwrap();
        assert_eq!(decode_personhood_status(&data).unwrap(), 2);
    }
}
