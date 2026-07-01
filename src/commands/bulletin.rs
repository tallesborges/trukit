use crate::chain::{self, bulletin, BulletinConfig};
use crate::env::Env;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use std::str::FromStr;
use subxt::utils::AccountId32;
use subxt::OnlineClient;
use subxt_signer::sr25519::Keypair;

/// Standard Substrate dev phrase. Its `//deploy/0` derivation is authorized to
/// write to the paseo-next-v2 Bulletin chain, so it's the default signer here.
const DEV_PHRASE: &str = "bottom drive obey lake curtain smoke basket hold race lonely fit walk";

/// Chain-enforced `MaxTransactionSize` (2 MiB).
const MAX_TRANSACTION_SIZE: usize = 2 * 1024 * 1024;

#[derive(Subcommand)]
pub enum Cmd {
    /// Store a single blob/file on the Bulletin chain.
    Store {
        /// Path to the file to store.
        path: String,
    },
    /// Store every IPLD block of a CARv1 so its root content CID resolves.
    StoreCar {
        /// Path to the `.car` file (CARv1, sha2-256 blocks; e.g. `ipfs dag export`).
        path: String,
    },
    /// Show authorization / quota for an account.
    Status {
        /// SS58 address to inspect (defaults to the signer's account).
        #[arg(long)]
        address: Option<String>,
    },
}

pub async fn run(
    env: &Env,
    cmd: Cmd,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    match cmd {
        Cmd::Status { address } => status(env, address, mnemonic, derivation_path).await,
        Cmd::Store { path } => store(env, path, mnemonic, derivation_path).await,
        Cmd::StoreCar { path } => store_car(env, path, mnemonic, derivation_path).await,
    }
}

/// Resolve the write signer: the caller's mnemonic when supplied, otherwise the
/// authorized dev-phrase `//deploy/0` account (dev Alice has no Bulletin quota).
fn resolve_signer(mnemonic: Option<String>, derivation_path: Option<String>) -> Result<Keypair> {
    match mnemonic {
        Some(phrase) => chain::build_signer(Some(&phrase), derivation_path.as_deref()),
        None => pool_signer(),
    }
}

/// The Bulletin-authorized pool signer (dev-phrase `//deploy/0`). `deploy` uses
/// this to store blocks regardless of the domain-owner mnemonic.
pub fn pool_signer() -> Result<Keypair> {
    chain::build_signer(Some(DEV_PHRASE), Some("//deploy/0"))
}

async fn status(
    env: &Env,
    address: Option<String>,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let account = match address {
        Some(addr) => AccountId32::from_str(&addr)
            .map_err(|e| anyhow::anyhow!("invalid SS58 address: {e}"))?,
        None => chain::account_id(&resolve_signer(mnemonic, derivation_path)?),
    };

    let client = OnlineClient::<BulletinConfig>::from_url(env.bulletin_rpc)
        .await
        .with_context(|| format!("connecting to Bulletin RPC {}", env.bulletin_rpc))?;

    let scope = bulletin::runtime_types::pallet_bulletin_transaction_storage::types::AuthorizationScope::Account(account.clone());
    let address = bulletin::storage().transaction_storage().authorizations();
    let at = client.at_current_block().await?;
    let authorization = at
        .storage()
        .try_fetch(address, (scope,))
        .await
        .context("reading TransactionStorage.Authorizations")?;

    println!("address                {account}");
    match authorization {
        Some(value) => {
            let auth = value.decode().context("decoding Authorization")?;
            let e = auth.extent;
            println!("authorized             yes");
            println!(
                "transactions           {} / {}",
                e.transactions, e.transactions_allowance
            );
            println!("bytes_stored           {}", e.bytes);
            println!("bytes_allowance        {}", e.bytes_allowance);
            println!("expiration_block       {}", auth.expiration);
        }
        None => println!("authorized             no (not authorized)"),
    }
    Ok(())
}

async fn store(
    env: &Env,
    path: String,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let data = std::fs::read(&path).with_context(|| format!("reading file {path}"))?;
    if data.len() > MAX_TRANSACTION_SIZE {
        bail!(
            "file {path} is {} bytes, exceeding the chain's MaxTransactionSize of {MAX_TRANSACTION_SIZE} bytes (2 MiB)",
            data.len()
        );
    }

    let cid = chain::raw_cid(&data);
    let gateway_url = format!("{}/ipfs/{cid}", env.ipfs_gateway);

    let client = OnlineClient::<BulletinConfig>::from_url(env.bulletin_rpc)
        .await
        .with_context(|| format!("connecting to Bulletin RPC {}", env.bulletin_rpc))?;
    let signer = resolve_signer(mnemonic, derivation_path)?;

    match chain::store_block(&client, &signer, 0x55, &data).await? {
        chain::StoreOutcome::AlreadyPresent { block, index } => {
            println!("already stored at block #{block} index {index}");
        }
        chain::StoreOutcome::Stored { block, index } => {
            println!("stored at block #{block} index {index}");
        }
    }
    println!("cid      {cid}");
    println!("gateway  {gateway_url}");
    Ok(())
}

/// Summary of storing a CAR's blocks on the Bulletin chain.
pub struct CarStored {
    pub root: cid::Cid,
    pub stored: usize,
    pub skipped: usize,
}

/// Store every IPLD block of a CARv1 individually (each keyed by its own content
/// hash) so the CAR's root DAG resolves on the IPFS gateway. Kubo chunks files
/// into ≤256 KiB blocks, so every block fits a single ≤2 MiB extrinsic.
pub async fn store_car_file(env: &Env, path: &str, signer: &Keypair) -> Result<CarStored> {
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

    let client = OnlineClient::<BulletinConfig>::from_url(env.bulletin_rpc)
        .await
        .with_context(|| format!("connecting to Bulletin RPC {}", env.bulletin_rpc))?;

    let mut stored = 0usize;
    let mut skipped = 0usize;
    while let Some((cid, data)) = car.next_block().await.context("reading next CAR block")? {
        let hash = cid.hash();
        if hash.code() != 0x12 {
            bail!(
                "block {cid} uses multihash code 0x{:x}; only sha2-256 (0x12) CARs are supported",
                hash.code()
            );
        }
        if hash.digest() != chain::content_hash(&data) {
            bail!("block {cid}: CAR data does not hash to the CID digest (corrupt CAR?)");
        }
        if data.len() > MAX_TRANSACTION_SIZE {
            bail!(
                "block {cid} is {} bytes, exceeding the chain's MaxTransactionSize of {MAX_TRANSACTION_SIZE} bytes (2 MiB)",
                data.len()
            );
        }

        match chain::store_block(&client, signer, cid.codec(), &data).await? {
            chain::StoreOutcome::Stored { .. } => stored += 1,
            chain::StoreOutcome::AlreadyPresent { .. } => skipped += 1,
        }
    }

    Ok(CarStored {
        root,
        stored,
        skipped,
    })
}

async fn store_car(
    env: &Env,
    path: String,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let signer = resolve_signer(mnemonic, derivation_path)?;
    let summary = store_car_file(env, &path, &signer).await?;

    let total = summary.stored + summary.skipped;
    let gateway_url = format!("{}/ipfs/{}/", env.ipfs_gateway, summary.root);
    println!("root     {}", summary.root);
    println!(
        "blocks   stored={} skipped={} total={total}",
        summary.stored, summary.skipped
    );
    println!("gateway  {gateway_url}");
    Ok(())
}
