use crate::chain::{self, bulletin, BulletinConfig, DEV_PHRASE};
use crate::env::Env;
use crate::ui;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use rand::Rng;
use serde_json::json;
use std::str::FromStr;
use std::time::Duration;
use subxt::utils::AccountId32;
use subxt::OnlineClient;
use subxt_signer::sr25519::Keypair;

/// Chain-enforced `MaxTransactionSize` (2 MiB).
const MAX_TRANSACTION_SIZE: usize = 2 * 1024 * 1024;

/// How long to wait for the IPFS gateway to serve a CID during `verify`.
const GATEWAY_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// Check whether a CID resolves on the env's IPFS gateway.
    Verify {
        /// The CID to check (e.g. bafy...).
        cid: String,
    },
    /// Authorize an account for Bulletin storage (signer needs Authorizer rights).
    Authorize {
        /// SS58 address to authorize (defaults to the signer's account).
        #[arg(long)]
        address: Option<String>,
        /// Transaction count to authorize.
        #[arg(long, default_value_t = 1_000_000)]
        transactions: u32,
        /// Byte allowance to authorize (default: 1 GiB).
        #[arg(long, default_value_t = 1_073_741_824)]
        bytes: u64,
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
        Cmd::Verify { cid } => verify(env, cid).await,
        Cmd::Authorize {
            address,
            transactions,
            bytes,
        } => authorize(env, address, transactions, bytes, mnemonic, derivation_path).await,
    }
}

/// Resolve the write signer: the caller's mnemonic when supplied, otherwise a
/// random authorized account from the shared Bulletin storage pool (the default
/// owner signer has no Bulletin quota — only the pool accounts do).
fn resolve_signer(mnemonic: Option<String>, derivation_path: Option<String>) -> Result<Keypair> {
    match mnemonic {
        Some(phrase) => chain::build_signer(Some(&phrase), derivation_path.as_deref()),
        None => pool_signer(),
    }
}

/// A random authorized account from the shared Bulletin storage pool, spreading
/// load and cutting nonce contention across concurrent deploys.
pub fn pool_signer() -> Result<Keypair> {
    let n = rand::thread_rng().gen_range(0u32..=9);
    chain::build_signer(Some(DEV_PHRASE), Some(&format!("//deploy/{n}")))
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

    let client = chain::bulletin_client(env).await?;

    let scope = bulletin::runtime_types::pallet_bulletin_transaction_storage::types::AuthorizationScope::Account(account);
    let address = bulletin::storage().transaction_storage().authorizations();
    let at = client.at_current_block().await?;
    let authorization = at
        .storage()
        .try_fetch(address, (scope,))
        .await
        .context("reading TransactionStorage.Authorizations")?;

    match authorization {
        Some(value) => {
            let auth = value.decode().context("decoding Authorization")?;
            let e = auth.extent;
            if ui::json() {
                ui::emit(&json!({
                    "address": account.to_string(),
                    "authorized": true,
                    "transactions": e.transactions,
                    "transactions_allowance": e.transactions_allowance,
                    "bytes": e.bytes,
                    "bytes_allowance": e.bytes_allowance,
                    "expires_block": auth.expiration,
                }));
            } else {
                ui::kv("address", account);
                ui::kv("authorized", "yes");
                ui::kv(
                    "txs",
                    format!("{} / {}", e.transactions, e.transactions_allowance),
                );
                ui::kv(
                    "bytes",
                    format!("{} / {} allowance", e.bytes, e.bytes_allowance),
                );
                ui::kv("expires", format!("block #{}", auth.expiration));
            }
        }
        None => {
            if ui::json() {
                ui::emit(&json!({ "address": account.to_string(), "authorized": false }));
            } else {
                ui::kv("address", account);
                ui::kv("authorized", "no (not authorized)");
            }
        }
    }
    Ok(())
}

/// Check whether `cid` resolves on the env's IPFS gateway with a real HTTP GET —
/// a live retrievability probe (does it actually load in a browser?) beyond the
/// on-chain read-back that `deploy` already does. Reports resolvability and the
/// HTTP status; exits 0 either way (read the `resolvable` field in `--json`).
async fn verify(env: &Env, cid_str: String) -> Result<()> {
    let cid =
        cid::Cid::try_from(cid_str.as_str()).with_context(|| format!("invalid CID '{cid_str}'"))?;
    let url = format!("{}/ipfs/{cid}", env.ipfs_gateway);

    ui::step(format!("verify {}", ui::ellipsize(&cid.to_string())));
    let http = reqwest::Client::builder()
        .timeout(GATEWAY_TIMEOUT)
        .build()
        .context("building HTTP client")?;
    let outcome = http.get(&url).send().await;

    let (resolvable, status) = match &outcome {
        Ok(resp) => (resp.status().is_success(), Some(resp.status().as_u16())),
        Err(_) => (false, None),
    };

    if ui::json() {
        ui::emit(&json!({
            "cid": cid.to_string(),
            "gateway": url,
            "resolvable": resolvable,
            "status": status,
        }));
    } else {
        ui::kv("cid", cid);
        ui::kv("gateway", &url);
        match &outcome {
            Ok(resp) if resp.status().is_success() => {
                ui::success(format!("resolvable (HTTP {})", resp.status().as_u16()));
            }
            Ok(resp) => ui::note(format!(
                "not resolvable yet (HTTP {}) — may still be propagating",
                resp.status().as_u16()
            )),
            Err(err) => ui::note(format!("not resolvable ({err})")),
        }
    }
    Ok(())
}

/// Authorize `address` (or the signer's own account) for Bulletin storage. Unlike
/// store/status this does not use the storage pool: the signer must hold Bulletin
/// Authorizer privileges (pass one via `--mnemonic`), else the chain returns
/// `BadOrigin`.
async fn authorize(
    env: &Env,
    address: Option<String>,
    transactions: u32,
    bytes: u64,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;
    let who = match address {
        Some(addr) => AccountId32::from_str(&addr)
            .map_err(|e| anyhow::anyhow!("invalid SS58 address: {e}"))?,
        None => chain::account_id(&signer),
    };

    let client = chain::bulletin_client(env).await?;
    ui::step(format!("authorize {who}"));
    let tx = chain::authorize_bulletin_account(&client, &signer, who, transactions, bytes).await?;

    if ui::json() {
        ui::emit(&json!({
            "address": who.to_string(),
            "transactions": transactions,
            "bytes": bytes,
            "tx": format!("0x{}", hex::encode(tx)),
        }));
    } else {
        ui::success(format!("authorized {who}"));
        ui::kv("txs", transactions);
        ui::kv("bytes", bytes);
        ui::kv("tx", format!("0x{}", hex::encode(tx)));
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

    let client = chain::bulletin_client(env).await?;
    let signer = resolve_signer(mnemonic, derivation_path)?;

    let (stored, block, index) = match chain::store_block(&client, &signer, 0x55, &data).await? {
        chain::StoreOutcome::AlreadyPresent { block, index } => (false, block, index),
        chain::StoreOutcome::Stored { block, index } => (true, block, index),
    };

    if ui::json() {
        ui::emit(&json!({
            "cid": cid.to_string(),
            "gateway": gateway_url,
            "stored": stored,
            "block": block,
            "index": index,
        }));
    } else {
        if stored {
            ui::success(format!("stored (block #{block} index {index})"));
        } else {
            ui::success(format!("already stored (block #{block} index {index})"));
        }
        ui::kv("cid", cid);
        ui::kv("gateway", gateway_url);
    }
    Ok(())
}

/// Summary of storing a CAR's blocks on the Bulletin chain.
pub struct CarStored {
    pub root: cid::Cid,
    pub stored: usize,
    pub skipped: usize,
}

/// Read a CARv1 file into its root CID + validated, upload-ready blocks. Verifies
/// each block is sha2-256, hashes to its CID, and fits one ≤2 MiB extrinsic.
pub async fn read_car_prepared(path: &str) -> Result<(cid::Cid, Vec<chain::PreparedBlock>)> {
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
        let content_hash = chain::content_hash(&data);
        if hash.digest() != content_hash {
            bail!("block {cid}: CAR data does not hash to the CID digest (corrupt CAR?)");
        }
        if data.len() > MAX_TRANSACTION_SIZE {
            bail!(
                "block {cid} is {} bytes, exceeding the chain's MaxTransactionSize of {MAX_TRANSACTION_SIZE} bytes (2 MiB)",
                data.len()
            );
        }
        prepared.push(chain::PreparedBlock {
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
    prepared: Vec<chain::PreparedBlock>,
    signer: &Keypair,
) -> Result<CarStored> {
    let total = prepared.len();
    let (stored, skipped) = chain::store_car_blocks(
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

async fn store_car(
    env: &Env,
    path: String,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let signer = resolve_signer(mnemonic, derivation_path)?;
    let client = chain::bulletin_client(env).await?;
    ui::step(format!("upload {path} to Bulletin"));
    let summary = store_car_file(env, &client, &path, &signer).await?;

    let total = summary.stored + summary.skipped;
    let gateway = format!("{}/ipfs/{}/", env.ipfs_gateway, summary.root);
    if ui::json() {
        ui::emit(&json!({
            "root": summary.root.to_string(),
            "stored": summary.stored,
            "skipped": summary.skipped,
            "total": total,
            "gateway": gateway,
        }));
    } else {
        ui::kv("root", summary.root);
        ui::kv(
            "blocks",
            format!(
                "{} stored · {} skipped · {total} total",
                summary.stored, summary.skipped
            ),
        );
        ui::kv("gateway", gateway);
    }
    Ok(())
}
