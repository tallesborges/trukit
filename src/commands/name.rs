use crate::chain;
use crate::dotns;
use crate::env::Env;
use anyhow::{bail, Context, Result};
use cid::Cid;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum Cmd {
    /// Resolve a .dot name to its contenthash CID.
    Resolve {
        /// The .dot name (e.g. myapp00.dot).
        name: String,
    },
    /// Read or set a .dot name's raw contenthash record.
    #[command(subcommand)]
    Content(ContentCmd),
}

#[derive(Subcommand)]
pub enum ContentCmd {
    /// Bind a CID to a .dot name's contenthash record (signed Revive.call).
    Set {
        /// The .dot name (must be owned by the signer).
        name: String,
        /// The CIDv1 to bind (e.g. bafy...).
        cid: String,
    },
    /// Read the raw contenthash record of a .dot name (`name content <name>`).
    #[command(external_subcommand)]
    Read(Vec<String>),
}

pub async fn run(
    env: &Env,
    cmd: Cmd,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    match cmd {
        Cmd::Resolve { name } => {
            let name = dotns::normalize_name(&name);
            let contenthash = chain::resolve_contenthash(env, &name).await?;
            if contenthash.is_empty() {
                println!("no contenthash set for {name}");
            } else {
                println!("{}", dotns::contenthash_to_cid(&contenthash)?);
            }
        }
        Cmd::Content(ContentCmd::Read(args)) => {
            let raw = args.first().context("usage: name content <name>")?;
            let name = dotns::normalize_name(raw);
            let contenthash = chain::resolve_contenthash(env, &name).await?;
            if contenthash.is_empty() {
                println!("no contenthash set for {name}");
            } else {
                println!("0x{}", hex::encode(&contenthash));
            }
        }
        Cmd::Content(ContentCmd::Set { name, cid }) => {
            set(env, &name, &cid, mnemonic, derivation_path).await?;
        }
    }
    Ok(())
}

async fn set(
    env: &Env,
    name: &str,
    cid: &str,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let name = dotns::normalize_name(name);
    let cid = Cid::try_from(cid).with_context(|| format!("invalid CID '{cid}'"))?;
    let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;

    println!("binding {name} -> {cid}");
    let expected = chain::set_contenthash(env, &signer, &name, &cid).await?;

    let onchain = chain::resolve_contenthash(env, &name).await?;
    if onchain != expected {
        bail!(
            "read-back mismatch: set 0x{} but chain has 0x{}",
            hex::encode(&expected),
            hex::encode(&onchain)
        );
    }
    println!("bound    {cid}");
    Ok(())
}
