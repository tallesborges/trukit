//! Chain access layer: subxt config, metadata caching, signer construction, and
//! the `pallet_revive` / Asset Hub primitives. Higher-level Bulletin storage and
//! DotNS naming operations live in the sibling [`crate::bulletin`] and
//! [`crate::dotns`] modules, built on top of these primitives.

pub mod asset_hub;
pub mod config;
pub mod metadata;
pub mod revive;
pub mod signer;

pub use asset_hub::{account_balance, asset_hub_client, transfer_keep_alive};
pub use revive::{ensure_mapped, revive_address};
pub use signer::{account_id, build_signer, shared_pool_signer};
