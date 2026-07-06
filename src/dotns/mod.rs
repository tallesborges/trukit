//! DotNS naming on Asset Hub: the resolver/registrar ABI encoders
//! ([`resolver`], [`registrar_abi`]) and the high-level naming operations
//! ([`names`]) that drive registration, records, ownership, and transfers.

pub mod names;
pub mod registrar_abi;
pub mod resolver;

pub use names::{
    classify_name, ensure_domain, name_owner, name_price_native, register_name,
    resolve_contenthash, resolve_text, set_contenthash, set_text, tier_name, transfer_name,
};
pub use resolver::{contenthash_to_cid, normalize_name};
