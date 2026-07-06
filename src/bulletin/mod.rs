//! Bulletin chain storage: content-addressed block storage primitives
//! ([`storage`]) and the CAR read/upload layer ([`upload`]) built on them.

pub mod storage;
pub mod upload;

pub use storage::{
    authorize_bulletin_account, bulletin_client, content_hash, raw_cid, store_block, PreparedBlock,
    StoreOutcome, MAX_TRANSACTION_SIZE,
};
pub use upload::{read_car_prepared, store_car_file, store_prepared_blocks};
