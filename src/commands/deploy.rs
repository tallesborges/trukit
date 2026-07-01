use crate::chain;
use crate::commands::bulletin;
use crate::dotns;
use crate::env::Env;
use anyhow::{bail, Context, Result};
use cid::Cid;
use clap::Args as ClapArgs;
use std::process::Command;

#[derive(ClapArgs)]
pub struct Args {
    /// Build directory to deploy (e.g. ./dist).
    pub dir: String,
    /// Target .dot domain (e.g. myapp00.dot).
    pub domain: String,
    /// Deploy a pre-built CAR instead of merkleizing the directory.
    #[arg(long)]
    pub input_car: Option<String>,
}

pub async fn run(
    env: &Env,
    args: Args,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let domain = dotns::normalize_name(&args.domain);

    let (content_cid, car_path, _tmp) = match args.input_car {
        Some(car) => {
            let root = car_root(&car).await?;
            (root, car, None)
        }
        None => {
            require_ipfs()?;
            let cid = merkleize(&args.dir)?;
            let tmp = TempCar::for_cid(&cid);
            export_car(&cid, tmp.path())?;
            (cid, tmp.path().to_string(), Some(tmp))
        }
    };

    println!("content  {content_cid}");
    println!("uploading blocks to Bulletin (pool signer //deploy/0)...");
    let pool = bulletin::pool_signer()?;
    let stored = bulletin::store_car_file(env, &car_path, &pool).await?;
    if stored.root != content_cid {
        bail!(
            "CAR root {} does not match the merkleized content CID {content_cid}",
            stored.root
        );
    }
    println!(
        "stored   blocks stored={} skipped={} total={}",
        stored.stored,
        stored.skipped,
        stored.stored + stored.skipped
    );
    println!("gateway  {}/ipfs/{content_cid}/", env.ipfs_gateway);

    println!("binding  {domain} -> {content_cid} (domain-owner mnemonic)...");
    let owner = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;
    let expected = chain::set_contenthash(env, &owner, &domain, &content_cid).await?;
    let onchain = chain::resolve_contenthash(env, &domain).await?;
    if onchain != expected {
        bail!(
            "read-back mismatch: set 0x{} but chain has 0x{}",
            hex::encode(&expected),
            hex::encode(&onchain)
        );
    }

    let label = domain.strip_suffix(".dot").unwrap_or(&domain);
    println!();
    println!("deployed {content_cid}");
    println!("url      https://{label}.dot.li");
    Ok(())
}

fn require_ipfs() -> Result<()> {
    Command::new("ipfs")
        .arg("--version")
        .output()
        .context("`ipfs` (Kubo) not found on PATH; install it or pass --input-car")?;
    Ok(())
}

/// Merkleize a directory with Kubo into a CIDv1 (raw leaves, unpinned) without
/// adding it to the local pinset — just to compute the content DAG + root CID.
fn merkleize(dir: &str) -> Result<Cid> {
    let out = Command::new("ipfs")
        .args([
            "add",
            "-Q",
            "-r",
            "--hidden",
            "--cid-version=1",
            "--raw-leaves",
            "--pin=false",
            dir,
        ])
        .output()
        .context("running `ipfs add`")?;
    if !out.status.success() {
        bail!(
            "`ipfs add` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let cid_str = String::from_utf8(out.stdout)
        .context("`ipfs add` produced non-UTF8 output")?
        .trim()
        .to_string();
    Cid::try_from(cid_str.as_str()).with_context(|| format!("parsing content CID '{cid_str}'"))
}

/// Export a CID's full DAG to a CARv1 file via `ipfs dag export`.
fn export_car(cid: &Cid, path: &str) -> Result<()> {
    let file = std::fs::File::create(path).with_context(|| format!("creating CAR file {path}"))?;
    let status = Command::new("ipfs")
        .args(["dag", "export", &cid.to_string()])
        .stdout(file)
        .status()
        .context("running `ipfs dag export`")?;
    if !status.success() {
        bail!("`ipfs dag export {cid}` failed");
    }
    Ok(())
}

async fn car_root(path: &str) -> Result<Cid> {
    let file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening CAR file {path}"))?;
    let car = iroh_car::CarReader::new(tokio::io::BufReader::new(file))
        .await
        .with_context(|| format!("parsing CARv1 header from {path}"))?;
    car.header()
        .roots()
        .first()
        .copied()
        .context("CAR header has no roots")
}

/// A temp CAR file removed on drop.
struct TempCar(std::path::PathBuf);

impl TempCar {
    fn for_cid(cid: &Cid) -> Self {
        TempCar(std::env::temp_dir().join(format!("trikit-deploy-{cid}.car")))
    }

    fn path(&self) -> &str {
        self.0.to_str().expect("temp path is valid UTF-8")
    }
}

impl Drop for TempCar {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
