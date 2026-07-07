use crate::bulletin;
use crate::chain;
use crate::chain::config::bulletin as bulletin_rt;
use crate::env::Env;
use crate::pool;
use crate::ui;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use serde_json::json;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;
use subxt::utils::AccountId32;
use subxt_signer::sr25519::Keypair;

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
    /// Manage the private per-machine upload pool (`~/.dotkit/pool.toml`). Testnet-only.
    #[command(subcommand)]
    Pool(PoolCmd),
}

#[derive(Subcommand)]
pub enum PoolCmd {
    /// Generate + persist a private pool keystore and print its `//deploy/N` accounts.
    Init {
        /// Number of //deploy/N accounts to derive.
        #[arg(long, default_value_t = pool::DEFAULT_ACCOUNTS)]
        accounts: u32,
        /// Overwrite an existing keystore (generates a NEW mnemonic).
        #[arg(long)]
        force: bool,
    },
    /// Show the private pool accounts (offline; no chain access).
    Status,
    /// Authorize every pool account for Bulletin storage in one `utility.batch_all`.
    /// Signer defaults to `//Alice` (the testnet Authorizer); override with global
    /// `--mnemonic`/`--derivation-path`. Idempotent: already-authorized accounts are skipped.
    Authorize {
        /// Transaction count to authorize per account.
        #[arg(long, default_value_t = 1_000_000)]
        transactions: u32,
        /// Byte allowance per account (default: 100 MiB).
        #[arg(long, default_value_t = 104_857_600)]
        bytes: u64,
    },
}

pub async fn run(
    env: &Env,
    cmd: Cmd,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
    pool_source: pool::PoolSource,
) -> Result<()> {
    match cmd {
        Cmd::Status { address } => {
            status(env, address, mnemonic, derivation_path, pool_source).await
        }
        Cmd::Store { path } => store(env, path, mnemonic, derivation_path, pool_source).await,
        Cmd::StoreCar { path } => {
            store_car(env, path, mnemonic, derivation_path, pool_source).await
        }
        Cmd::Verify { cid } => verify(env, cid).await,
        Cmd::Authorize {
            address,
            transactions,
            bytes,
        } => authorize(env, address, transactions, bytes, mnemonic, derivation_path).await,
        Cmd::Pool(PoolCmd::Init { accounts, force }) => pool_init(accounts, force),
        Cmd::Pool(PoolCmd::Status) => pool_status(env, pool_source).await,
        Cmd::Pool(PoolCmd::Authorize {
            transactions,
            bytes,
        }) => pool_authorize(env, mnemonic, derivation_path, transactions, bytes).await,
    }
}

/// Resolve the write signer: the caller's mnemonic when supplied, otherwise a
/// random authorized account from the selected Bulletin pool (the default owner
/// signer has no Bulletin quota — only pool accounts do).
fn resolve_signer(
    mnemonic: Option<String>,
    derivation_path: Option<String>,
    pool_source: pool::PoolSource,
) -> Result<Keypair> {
    match mnemonic {
        Some(phrase) => chain::build_signer(Some(&phrase), derivation_path.as_deref()),
        None => pool::pool_signer(pool_source),
    }
}

async fn status(
    env: &Env,
    address: Option<String>,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
    pool_source: pool::PoolSource,
) -> Result<()> {
    let account = match address {
        Some(addr) => AccountId32::from_str(&addr)
            .map_err(|e| anyhow::anyhow!("invalid SS58 address: {e}"))?,
        None => chain::account_id(&resolve_signer(mnemonic, derivation_path, pool_source)?),
    };

    let client = bulletin::bulletin_client(env).await?;

    let scope = bulletin_rt::runtime_types::pallet_bulletin_transaction_storage::types::AuthorizationScope::Account(account);
    let address = bulletin_rt::storage()
        .transaction_storage()
        .authorizations();
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

    let client = bulletin::bulletin_client(env).await?;
    ui::step(format!("authorize {who}"));
    let tx =
        bulletin::authorize_bulletin_account(&client, &signer, who, transactions, bytes).await?;

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
    pool_source: pool::PoolSource,
) -> Result<()> {
    let data = std::fs::read(&path).with_context(|| format!("reading file {path}"))?;
    if data.len() > bulletin::MAX_TRANSACTION_SIZE {
        bail!(
            "file {path} is {} bytes, exceeding the chain's MaxTransactionSize of {} bytes (2 MiB)",
            data.len(),
            bulletin::MAX_TRANSACTION_SIZE
        );
    }

    let cid = bulletin::raw_cid(&data);
    let gateway_url = format!("{}/ipfs/{cid}", env.ipfs_gateway);

    let client = bulletin::bulletin_client(env).await?;
    let signer = resolve_signer(mnemonic, derivation_path, pool_source)?;

    let (stored, block, index) = match bulletin::store_block(&client, &signer, 0x55, &data).await? {
        bulletin::StoreOutcome::AlreadyPresent { block, index } => (false, block, index),
        bulletin::StoreOutcome::Stored { block, index } => (true, block, index),
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

async fn store_car(
    env: &Env,
    path: String,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
    pool_source: pool::PoolSource,
) -> Result<()> {
    let signer = resolve_signer(mnemonic, derivation_path, pool_source)?;
    let client = bulletin::bulletin_client(env).await?;
    ui::step(format!("upload {path} to Bulletin"));
    let summary = bulletin::store_car_file(env, &client, &path, &signer).await?;

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

// ---- private upload pool (`bulletin pool …`) ----

fn pool_init(accounts: u32, force: bool) -> Result<()> {
    if accounts == 0 {
        bail!("--accounts must be >= 1");
    }
    let path = pool::keystore_path()?;
    if let Some(existing) = pool::load()? {
        if !force {
            bail!(
                "pool keystore already exists at {} ({} accounts); \
                 run `dotkit bulletin pool status` to view, or pass --force to regenerate",
                path.display(),
                existing.accounts
            );
        }
    }
    let p = pool::generate(accounts)?;
    let path = pool::save(&p)?;
    print_pool(&p, &path, true)
}

async fn pool_status(env: &Env, pool_source: pool::PoolSource) -> Result<()> {
    let (label, accts) = pool::accounts_for(pool_source)?;
    let client = bulletin::bulletin_client(env).await?;

    ui::step(format!("pool status ({label} · {} accounts)", accts.len()));
    let mut authorized = 0usize;
    let mut rows = Vec::new();
    for (i, a) in &accts {
        let info = bulletin::authorization(&client, a).await?;
        if info.is_some() {
            authorized += 1;
        }
        rows.push((*i, a.clone(), info));
    }

    if ui::json() {
        let accounts: Vec<_> = rows
            .iter()
            .map(|(i, a, info)| match info {
                Some(e) => json!({
                    "index": i,
                    "ss58": a.to_string(),
                    "authorized": true,
                    "transactions": e.transactions,
                    "transactions_allowance": e.transactions_allowance,
                    "bytes": e.bytes,
                    "bytes_allowance": e.bytes_allowance,
                    "expires_block": e.expiration,
                }),
                None => json!({ "index": i, "ss58": a.to_string(), "authorized": false }),
            })
            .collect();
        ui::emit(&json!({
            "pool": label,
            "count": accts.len(),
            "authorized": authorized,
            "accounts": accounts,
        }));
    } else {
        for (i, a, info) in &rows {
            let addr = ui::ellipsize(&a.to_string());
            match info {
                Some(e) => ui::kv(
                    &format!("//deploy/{i}"),
                    format!(
                        "{addr}  txs {}/{} · bytes {}/{} · exp #{}",
                        e.transactions, e.transactions_allowance, e.bytes, e.bytes_allowance,
                        e.expiration
                    ),
                ),
                None => ui::kv(&format!("//deploy/{i}"), format!("{addr}  ✗ not authorized")),
            }
        }
        ui::success(format!("{authorized}/{} authorized ({label} pool)", accts.len()));
    }
    Ok(())
}

async fn pool_authorize(
    env: &Env,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
    transactions: u32,
    bytes: u64,
) -> Result<()> {
    let path = pool::keystore_path()?;
    let Some(p) = pool::load()? else {
        bail!(
            "no pool keystore at {} — run `dotkit bulletin pool init` first",
            path.display()
        );
    };
    let accts = pool::accounts(&p)?;

    // Authorizer signer: default to the testnet Authorizer `//Alice`; otherwise
    // honour an explicit --mnemonic/--derivation-path.
    let signer = match (mnemonic.as_deref(), derivation_path.as_deref()) {
        (None, None) => chain::build_signer(None, Some("//Alice"))?,
        (m, d) => chain::build_signer(m, d)?,
    };

    let client = bulletin::bulletin_client(env).await?;

    ui::step("check existing authorizations");
    let mut pending = Vec::new();
    for (i, a) in &accts {
        if bulletin::is_authorized(&client, a).await? {
            ui::kv(&format!("//deploy/{i}"), "already authorized");
        } else {
            pending.push(a.clone());
        }
    }

    if pending.is_empty() {
        if ui::json() {
            ui::emit(&json!({
                "authorized": 0,
                "skipped": accts.len(),
                "status": "all-authorized",
            }));
        } else {
            ui::success(format!(
                "all {} pool accounts already authorized",
                accts.len()
            ));
        }
        return Ok(());
    }

    ui::step(format!(
        "authorize {} account(s) via utility.batch_all",
        pending.len()
    ));
    let tx =
        bulletin::batch_authorize_accounts(&client, &signer, &pending, transactions, bytes).await?;

    if ui::json() {
        ui::emit(&json!({
            "authorized": pending.len(),
            "skipped": accts.len() - pending.len(),
            "transactions": transactions,
            "bytes": bytes,
            "tx": format!("0x{}", hex::encode(tx)),
        }));
    } else {
        ui::success(format!("authorized {} account(s)", pending.len()));
        ui::kv("txs", transactions);
        ui::kv("bytes", bytes);
        ui::kv("tx", format!("0x{}", hex::encode(tx)));
    }
    Ok(())
}

fn print_pool(p: &pool::Pool, path: &Path, created: bool) -> Result<()> {
    let accts = pool::accounts(p)?;
    if ui::json() {
        ui::emit(&json!({
            "keystore": path.display().to_string(),
            "count": p.accounts,
            "accounts": accts
                .iter()
                .map(|(i, a)| json!({ "index": i, "ss58": a.to_string() }))
                .collect::<Vec<_>>(),
        }));
    } else {
        if created {
            ui::success("created private upload pool");
        }
        ui::kv("keystore", path.display());
        ui::kv("accounts", p.accounts);
        ui::note("testnet-only · plaintext mnemonic · no mainnet value");
        for (i, a) in &accts {
            ui::kv(&format!("//deploy/{i}"), a);
        }
    }
    Ok(())
}
