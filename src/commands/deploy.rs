use crate::chain;
use crate::commands::bulletin;
use crate::config::DeployConfig;
use crate::dotns;
use crate::env::Env;
use crate::merkle;
use crate::ui;
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
    /// Merkleize with the Kubo `ipfs` binary instead of the native encoder (fallback).
    #[arg(long)]
    pub kubo: bool,
    /// Deploy manifest with text records to write (defaults to ./deploy.toml if present).
    #[arg(long)]
    pub config: Option<String>,
    /// Register the domain (open-tier) if it isn't already owned by the signer.
    #[arg(long)]
    pub register: bool,
}

pub async fn run(
    env: &Env,
    args: Args,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let domain = dotns::normalize_name(&args.domain);
    let config = DeployConfig::load(args.config.as_deref())?;

    let owner = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;
    let asset_hub = chain::asset_hub_client(env).await?;
    chain::ensure_domain(&asset_hub, env, &owner, &domain, args.register).await?;

    let (content_cid, prepared) = match &args.input_car {
        Some(car) => {
            ui::step(format!("read CAR {car}"));
            bulletin::read_car_prepared(car).await?
        }
        None if args.kubo => {
            require_ipfs()?;
            ui::step(format!("merkleize {} (kubo)", args.dir));
            let cid = merkleize_kubo(&args.dir)?;
            let tmp = TempCar::for_cid(&cid);
            export_car(&cid, tmp.path())?;
            bulletin::read_car_prepared(tmp.path()).await?
        }
        None => {
            ui::step(format!("merkleize {}", args.dir));
            let m = merkle::merkleize_dir(&args.dir)?;
            (m.root, m.blocks)
        }
    };
    ui::kv("content", content_cid);

    let pool = bulletin::pool_signer()?;
    ui::step("upload to Bulletin");
    let bulletin = chain::bulletin_client(env).await?;
    let stored =
        bulletin::store_prepared_blocks(env, &bulletin, content_cid, prepared, &pool).await?;
    ui::kv(
        "blocks",
        format!(
            "{} stored · {} skipped · {} total",
            stored.stored,
            stored.skipped,
            stored.stored + stored.skipped
        ),
    );
    ui::kv(
        "gateway",
        format!("{}/ipfs/{content_cid}/", env.ipfs_gateway),
    );

    ui::step(format!(
        "bind {domain} → {}",
        ui::ellipsize(&content_cid.to_string())
    ));
    let expected = chain::set_contenthash(&asset_hub, env, &owner, &domain, &content_cid).await?;
    let onchain = chain::resolve_contenthash(&asset_hub, env, &domain).await?;
    if onchain != expected {
        bail!(
            "read-back mismatch: set 0x{} but chain has 0x{}",
            hex::encode(&expected),
            hex::encode(&onchain)
        );
    }

    for (key, value) in &config.text {
        ui::step(format!("set '{key}' on {domain}"));
        chain::set_text(&asset_hub, env, &owner, &domain, key, value).await?;
        ui::kv(key, ui::ellipsize(value));
    }

    let label = domain.strip_suffix(".dot").unwrap_or(&domain);
    let url = (!env.web_gateway.is_empty()).then(|| format!("https://{label}.{}", env.web_gateway));
    if ui::json() {
        ui::emit(&serde_json::json!({
            "domain": domain,
            "content": content_cid.to_string(),
            "url": url,
            "blocks": { "stored": stored.stored, "skipped": stored.skipped },
        }));
    } else {
        println!();
        ui::success(format!("deployed {domain}"));
        ui::kv("content", content_cid);
        if let Some(url) = url {
            ui::kv("url", url);
        }
    }
    Ok(())
}

fn require_ipfs() -> Result<()> {
    Command::new("ipfs")
        .arg("--version")
        .output()
        .context("`ipfs` (Kubo) not found on PATH; drop --kubo to use the native encoder")?;
    Ok(())
}

/// Merkleize a directory with Kubo into a CIDv1 (raw leaves, unpinned) without
/// adding it to the local pinset — just to compute the content DAG + root CID.
fn merkleize_kubo(dir: &str) -> Result<Cid> {
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

/// Export a CID's full DAG to a CARv1 file via `ipfs dag export`. Captures
/// stderr so Kubo's progress bar doesn't leak into our output.
fn export_car(cid: &Cid, path: &str) -> Result<()> {
    let file = std::fs::File::create(path).with_context(|| format!("creating CAR file {path}"))?;
    let out = Command::new("ipfs")
        .args(["dag", "export", &cid.to_string()])
        .stdout(file)
        .output()
        .context("running `ipfs dag export`")?;
    if !out.status.success() {
        bail!(
            "`ipfs dag export {cid}` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// A temp CAR file removed on drop.
struct TempCar(std::path::PathBuf);

impl TempCar {
    fn for_cid(cid: &Cid) -> Self {
        TempCar(std::env::temp_dir().join(format!("dotkit-deploy-{cid}.car")))
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
