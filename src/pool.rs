//! Per-machine private Bulletin upload pool keystore (`~/.dotkit/pool.toml`).
//!
//! A locally-generated BIP39 mnemonic whose `//deploy/{0..N}` sub-accounts form
//! a private Bulletin upload pool — isolated from the shared `DEV_PHRASE` pool so
//! uploads don't contend on nonces/quota with everyone else. **Testnet only**:
//! the mnemonic is stored in plaintext and holds no mainnet value.

use anyhow::{bail, Context, Result};
use rand::{Rng, RngCore};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use subxt::utils::AccountId32;
use subxt_signer::sr25519::Keypair;

use crate::chain;
use crate::ui;

/// Default number of derived pool accounts, matching the shared pool's `0..=9`.
pub const DEFAULT_ACCOUNTS: u32 = 10;

const DIR_NAME: &str = ".dotkit";
const FILE_NAME: &str = "pool.toml";

const HEADER: &str = "\
# dotkit private Bulletin upload pool — TESTNET ONLY.
# Plaintext BIP39 mnemonic for a per-machine pool (no mainnet value).
# Regenerate with `dotkit bulletin pool init --force`.
";

/// Persisted keystore contents.
#[derive(Debug, Serialize, Deserialize)]
pub struct Pool {
    /// BIP39 mnemonic for the private pool root. Testnet-only, low value.
    pub mnemonic: String,
    /// Number of `//deploy/{0..accounts-1}` sub-accounts.
    pub accounts: u32,
    /// Unix creation timestamp (informational).
    pub created_unix: u64,
}

/// `~/.dotkit/pool.toml`.
pub fn keystore_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME env var not set")?;
    Ok(PathBuf::from(home).join(DIR_NAME).join(FILE_NAME))
}

/// Load the keystore, or `None` when it doesn't exist yet.
pub fn load() -> Result<Option<Pool>> {
    let path = keystore_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading pool keystore {}", path.display()))?;
    let pool = toml::from_str(&raw)
        .with_context(|| format!("parsing pool keystore {}", path.display()))?;
    Ok(Some(pool))
}

/// Generate a fresh pool with a new random 12-word mnemonic.
pub fn generate(accounts: u32) -> Result<Pool> {
    let mut entropy = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut entropy);
    let mnemonic = subxt_signer::bip39::Mnemonic::from_entropy(&entropy)
        .context("generating BIP39 mnemonic")?
        .to_string();
    Ok(Pool {
        mnemonic,
        accounts,
        created_unix: now_unix(),
    })
}

/// Persist the keystore to `~/.dotkit/pool.toml`, creating the dir if needed and
/// locking file perms to `0600` (dir `0700`) so the mnemonic isn't world-readable.
pub fn save(pool: &Pool) -> Result<PathBuf> {
    let path = keystore_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        set_mode(dir, 0o700)?;
    }
    let body = format!(
        "{HEADER}{}",
        toml::to_string_pretty(pool).context("serializing keystore")?
    );
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    set_mode(&path, 0o600)?;
    Ok(path)
}

/// Derive the `//deploy/{index}` signer for a stored pool mnemonic.
pub fn pool_keypair(mnemonic: &str, index: u32) -> Result<Keypair> {
    chain::build_signer(Some(mnemonic), Some(&format!("//deploy/{index}")))
}

/// Every derived `(index, account)` pair for a pool.
pub fn accounts(pool: &Pool) -> Result<Vec<(u32, AccountId32)>> {
    (0..pool.accounts)
        .map(|i| Ok((i, chain::account_id(&pool_keypair(&pool.mnemonic, i)?))))
        .collect()
}

/// The `(label, accounts)` a `--pool` selection resolves to, for inspection:
/// - `Local` / `Auto`-with-keystore → the private keystore's `//deploy/N`.
/// - `Shared` / `Auto`-without-keystore → the shared `DEV_PHRASE//deploy/{0..9}`.
pub fn accounts_for(source: PoolSource) -> Result<(&'static str, Vec<(u32, AccountId32)>)> {
    let use_local = match source {
        PoolSource::Shared => false,
        PoolSource::Local => true,
        PoolSource::Auto => keystore_path().map(|p| p.exists()).unwrap_or(false),
    };
    if use_local {
        let Some(pool) = load()? else {
            bail!(
                "no pool keystore at {} — run `dotkit bulletin pool init` first",
                keystore_path()?.display()
            );
        };
        Ok(("private", accounts(&pool)?))
    } else {
        let accts = (0u32..=9)
            .map(|n| {
                let kp = chain::build_signer(None, Some(&format!("//deploy/{n}")))?;
                Ok((n, chain::account_id(&kp)))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(("shared", accts))
    }
}

/// Which Bulletin upload pool a command should sign with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolSource {
    /// Local private pool if a keystore exists, else the shared pool.
    Auto,
    /// Force the local private pool (error if no keystore).
    Local,
    /// Force the shared `DEV_PHRASE//deploy/N` pool.
    Shared,
}

/// Resolve a random Bulletin upload signer for `source`. Emits a one-line note
/// (suppressed under `--quiet`/`--json`) naming which pool + account was picked.
pub fn pool_signer(source: PoolSource) -> Result<Keypair> {
    let use_local = match source {
        PoolSource::Shared => false,
        PoolSource::Local => true,
        PoolSource::Auto => keystore_path().map(|p| p.exists()).unwrap_or(false),
    };

    if use_local {
        let Some(pool) = load()? else {
            bail!(
                "--pool local requested but no keystore at {} — run `dotkit bulletin pool init` first",
                keystore_path()?.display()
            );
        };
        let n = rand::thread_rng().gen_range(0..pool.accounts);
        let kp = pool_keypair(&pool.mnemonic, n)?;
        ui::note(format!("pool: private //deploy/{n} ({})", chain::account_id(&kp)));
        Ok(kp)
    } else {
        let kp = chain::shared_pool_signer()?;
        ui::note(format!("pool: shared ({})", chain::account_id(&kp)));
        Ok(kp)
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(unix)]
fn set_mode(path: &std::path::Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("setting permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_path: &std::path::Path, _mode: u32) -> Result<()> {
    Ok(())
}
