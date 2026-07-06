//! Preparing and uploading content to the Bulletin chain: reading a CARv1 into
//! validated blocks and storing a block set so its root DAG resolves on the IPFS
//! gateway. Shared by `deploy` and `bulletin store-car` so neither command
//! depends on the other.

use crate::bulletin::storage::{self, PreparedBlock, MAX_TRANSACTION_SIZE};
use crate::chain::config::BulletinConfig;
use crate::env::Env;
use crate::ui;
use anyhow::{bail, Context, Result};
use subxt::OnlineClient;
use subxt_signer::sr25519::Keypair;

/// Summary of storing a CAR's blocks on the Bulletin chain.
pub struct CarStored {
    pub root: cid::Cid,
    pub stored: usize,
    pub skipped: usize,
}

/// Read a CARv1 file into its root CID + validated, upload-ready blocks. Verifies
/// each block is sha2-256, hashes to its CID, and fits one ≤2 MiB extrinsic.
pub async fn read_car_prepared(path: &str) -> Result<(cid::Cid, Vec<PreparedBlock>)> {
    let file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening CAR file {path}"))?;
    let mut car = iroh_car::CarReader::new(tokio::io::BufReader::new(file))
        .await
        .with_context(|| format!("parsing CARv1 header from {path}"))?;

    let root = *car
        .header()
        .roots()
        .first()
        .context("CAR header has no roots")?;

    let mut prepared = Vec::new();
    while let Some((cid, data)) = car.next_block().await.context("reading next CAR block")? {
        let hash = cid.hash();
        if hash.code() != 0x12 {
            bail!(
                "block {cid} uses multihash code 0x{:x}; only sha2-256 (0x12) CARs are supported",
                hash.code()
            );
        }
        let content_hash = storage::content_hash(&data);
        if hash.digest() != content_hash {
            bail!("block {cid}: CAR data does not hash to the CID digest (corrupt CAR?)");
        }
        if data.len() > MAX_TRANSACTION_SIZE {
            bail!(
                "block {cid} is {} bytes, exceeding the chain's MaxTransactionSize of {MAX_TRANSACTION_SIZE} bytes (2 MiB)",
                data.len()
            );
        }
        prepared.push(PreparedBlock {
            codec: cid.codec(),
            data,
            content_hash,
        });
    }
    Ok((root, prepared))
}

/// Store a prepared block set on the Bulletin chain (each block keyed by its own
/// content hash) so `root`'s DAG resolves on the IPFS gateway. Reuses the single
/// `client` for the whole upload so metadata is downloaded once.
pub async fn store_prepared_blocks(
    env: &Env,
    client: &OnlineClient<BulletinConfig>,
    root: cid::Cid,
    prepared: Vec<PreparedBlock>,
    signer: &Keypair,
) -> Result<CarStored> {
    let total = prepared.len();
    let (stored, skipped) = storage::store_car_blocks(
        client,
        env.bulletin_rpc,
        signer,
        &prepared,
        |done, stored, skipped| {
            ui::progress(format!(
                "blocks     {done}/{total} · stored {stored} · skipped {skipped}"
            ));
        },
    )
    .await?;
    ui::progress_clear();

    Ok(CarStored {
        root,
        stored,
        skipped,
    })
}

/// Store every IPLD block of a CARv1 individually (each keyed by its own content
/// hash) so the CAR's root DAG resolves on the IPFS gateway. Kubo chunks files
/// into ≤256 KiB blocks, so every block fits a single ≤2 MiB extrinsic.
pub async fn store_car_file(
    env: &Env,
    client: &OnlineClient<BulletinConfig>,
    path: &str,
    signer: &Keypair,
) -> Result<CarStored> {
    let (root, prepared) = read_car_prepared(path).await?;
    store_prepared_blocks(env, client, root, prepared, signer).await
}
