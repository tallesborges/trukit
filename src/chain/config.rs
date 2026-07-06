//! subxt [`Config`] surface for the two chains dotkit talks to: the generated
//! runtime interfaces (`asset_hub`, `bulletin`) plus the bespoke transaction
//! extension sets and [`AssetHubConfig`] / [`BulletinConfig`] that make signing
//! against those runtimes produce the exact wire payload each expects.

use super::metadata::MetadataCache;
use scale_info::PortableRegistry;
use subxt::metadata::ArcMetadata;
use subxt::utils::AccountId32;
use subxt::PolkadotConfig;

#[subxt::subxt(runtime_metadata_path = "artifacts/paseo_next_v2_asset_hub.scale")]
pub mod asset_hub {}

#[subxt::subxt(runtime_metadata_path = "artifacts/paseo_next_v2_bulletin.scale")]
pub mod bulletin {}

/// The Bulletin chain declares three custom, empty transaction extensions on top
/// of the usual Substrate ones — `AuthorizeCall`, `ValidateStorageCalls` and
/// `AllowanceBasedPriority` — plus `CheckNonZeroSender`, `CheckWeight` and
/// `StorageWeightReclaim`, none of which subxt's `PolkadotConfig` provides.
/// Signing therefore needs a bespoke [`Config`] whose `TransactionExtensions`
/// tuple covers every extension the runtime lists, in declared order. Each of the
/// extensions below encodes nothing for both the value and the implicit payload.
macro_rules! empty_extension {
    ($ext:ident, $name:literal) => {
        pub struct $ext;

        impl<T: subxt::Config> subxt::config::TransactionExtension<T> for $ext {
            type Decoded = ();
            type Params = ();

            fn new(
                _client: &subxt::config::ClientState<T>,
                _params: Self::Params,
            ) -> core::result::Result<Self, subxt::error::TransactionExtensionError> {
                Ok($ext)
            }
        }

        impl subxt::ext::frame_decode::extrinsics::TransactionExtension<PortableRegistry> for $ext {
            const NAME: &str = $name;

            fn encode_value_to(
                &self,
                _type_id: u32,
                _type_resolver: &PortableRegistry,
                _out: &mut Vec<u8>,
            ) -> core::result::Result<
                (),
                subxt::ext::frame_decode::extrinsics::TransactionExtensionError,
            > {
                Ok(())
            }

            fn encode_implicit_to(
                &self,
                _type_id: u32,
                _type_resolver: &PortableRegistry,
                _out: &mut Vec<u8>,
            ) -> core::result::Result<
                (),
                subxt::ext::frame_decode::extrinsics::TransactionExtensionError,
            > {
                Ok(())
            }
        }
    };
}

empty_extension!(AuthorizeCall, "AuthorizeCall");
empty_extension!(CheckNonZeroSender, "CheckNonZeroSender");
empty_extension!(CheckWeight, "CheckWeight");
empty_extension!(ValidateStorageCalls, "ValidateStorageCalls");
empty_extension!(AllowanceBasedPriority, "AllowanceBasedPriority");
empty_extension!(StorageWeightReclaim, "StorageWeightReclaim");
empty_extension!(EthSetOrigin, "EthSetOrigin");

/// Asset Hub declares several custom transaction extensions that carry a real,
/// non-empty value (unlike the empty ones above). For a plain signed call none
/// of the optional behaviours apply, so each encodes its inert default —
/// `Option::None` (one `0x00` byte) or `false` — and nothing for the implicit.
macro_rules! default_value_extension {
    ($ext:ident, $name:literal, $value:expr) => {
        pub struct $ext;

        impl<T: subxt::Config> subxt::config::TransactionExtension<T> for $ext {
            type Decoded = ();
            type Params = ();

            fn new(
                _client: &subxt::config::ClientState<T>,
                _params: Self::Params,
            ) -> core::result::Result<Self, subxt::error::TransactionExtensionError> {
                Ok($ext)
            }
        }

        impl subxt::ext::frame_decode::extrinsics::TransactionExtension<PortableRegistry> for $ext {
            const NAME: &str = $name;

            fn encode_value_to(
                &self,
                _type_id: u32,
                _type_resolver: &PortableRegistry,
                out: &mut Vec<u8>,
            ) -> core::result::Result<
                (),
                subxt::ext::frame_decode::extrinsics::TransactionExtensionError,
            > {
                subxt::ext::codec::Encode::encode_to(&$value, out);
                Ok(())
            }

            fn encode_implicit_to(
                &self,
                _type_id: u32,
                _type_resolver: &PortableRegistry,
                _out: &mut Vec<u8>,
            ) -> core::result::Result<
                (),
                subxt::ext::frame_decode::extrinsics::TransactionExtensionError,
            > {
                Ok(())
            }
        }
    };
}

default_value_extension!(
    AuthorizeValueTransfer,
    "AuthorizeValueTransfer",
    Option::<()>::None
);
default_value_extension!(AsPgas, "AsPgas", Option::<()>::None);
default_value_extension!(AsRingAlias, "AsRingAlias", Option::<()>::None);
default_value_extension!(AsDotnsGateway, "AsDotnsGateway", Option::<()>::None);
default_value_extension!(RestrictOrigins, "RestrictOrigins", false);

use subxt::config::transaction_extensions as tx_ext;

type BulletinTxExtensions = (
    AuthorizeCall,
    CheckNonZeroSender,
    tx_ext::CheckSpecVersion,
    tx_ext::CheckTxVersion,
    tx_ext::CheckGenesis<BulletinConfig>,
    tx_ext::CheckMortality<BulletinConfig>,
    tx_ext::CheckNonce,
    CheckWeight,
    tx_ext::ChargeTransactionPayment,
    ValidateStorageCalls,
    AllowanceBasedPriority,
    tx_ext::CheckMetadataHash,
    StorageWeightReclaim,
);

/// subxt [`Config`] for the Bulletin chain. Account/address/signature/hashing all
/// match a standard Substrate chain; only the transaction-extension set differs.
/// Genesis hash and runtime version are still fetched from the node, but the
/// [`MetadataCache`] keeps each spec version's metadata so a reused client
/// downloads it once instead of on every block access.
#[derive(Debug, Clone, Default)]
pub struct BulletinConfig {
    pub(crate) metadata_cache: MetadataCache,
}

impl subxt::Config for BulletinConfig {
    type AccountId = AccountId32;
    type Address = subxt::utils::MultiAddress<AccountId32, ()>;
    type Signature = subxt::utils::MultiSignature;
    type Hasher = <PolkadotConfig as subxt::Config>::Hasher;
    type Header = <PolkadotConfig as subxt::Config>::Header;
    type AssetId = u32;
    type TransactionExtensions = BulletinTxExtensions;

    fn metadata_for_spec_version(&self, spec_version: u32) -> Option<ArcMetadata> {
        self.metadata_cache.get(spec_version)
    }

    fn set_metadata_for_spec_version(&self, spec_version: u32, metadata: ArcMetadata) {
        self.metadata_cache.set(spec_version, metadata);
    }
}

/// Asset Hub (paseo-next-v2) lists 17 transaction extensions in this exact
/// declared order. subxt matches each by name, so the tuple must name all of
/// them. Six are custom to the individuality/revive runtime — five carry a
/// value (`AuthorizeValueTransfer`, `AsPgas`, `AsRingAlias`, `AsDotnsGateway`,
/// `RestrictOrigins`) and encode their inert default, `AuthorizeCall`/
/// `EthSetOrigin` are empty. `ChargeAssetTxPayment` pays fees in the native
/// token (tip 0, `asset_id: None`).
type AssetHubTxExtensions = (
    AuthorizeValueTransfer,
    AuthorizeCall,
    AsPgas,
    AsRingAlias,
    AsDotnsGateway,
    RestrictOrigins,
    CheckNonZeroSender,
    tx_ext::CheckSpecVersion,
    tx_ext::CheckTxVersion,
    tx_ext::CheckGenesis<AssetHubConfig>,
    tx_ext::CheckMortality<AssetHubConfig>,
    tx_ext::CheckNonce,
    CheckWeight,
    tx_ext::ChargeAssetTxPayment<AssetHubConfig>,
    tx_ext::CheckMetadataHash,
    EthSetOrigin,
    StorageWeightReclaim,
);

/// subxt [`Config`] for Asset Hub. Same account/address/signature/hashing as a
/// standard Substrate chain; only the extension set differs. `AssetId = u32` is
/// only used by `ChargeAssetTxPayment`, which we always call with `None`, so the
/// concrete type never affects the encoded bytes.
#[derive(Debug, Clone, Default)]
pub struct AssetHubConfig {
    pub(crate) metadata_cache: MetadataCache,
}

impl subxt::Config for AssetHubConfig {
    type AccountId = AccountId32;
    type Address = subxt::utils::MultiAddress<AccountId32, ()>;
    type Signature = subxt::utils::MultiSignature;
    type Hasher = <PolkadotConfig as subxt::Config>::Hasher;
    type Header = <PolkadotConfig as subxt::Config>::Header;
    type AssetId = u32;
    type TransactionExtensions = AssetHubTxExtensions;

    fn metadata_for_spec_version(&self, spec_version: u32) -> Option<ArcMetadata> {
        self.metadata_cache.get(spec_version)
    }

    fn set_metadata_for_spec_version(&self, spec_version: u32, metadata: ArcMetadata) {
        self.metadata_cache.set(spec_version, metadata);
    }
}
