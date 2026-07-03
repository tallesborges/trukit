use crate::chain;
use crate::dotns;
use crate::env::Env;
use crate::ui;
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
    /// Register an open-tier .dot name (commit/reveal) to the signer.
    Register {
        /// The .dot name to register (e.g. myapp00.dot).
        name: String,
    },
    /// Read or set a .dot name's raw contenthash record.
    #[command(subcommand)]
    Content(ContentCmd),
    /// Read or set a .dot name's text records (e.g. manifest, executable).
    #[command(subcommand)]
    Text(TextCmd),
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
    /// Read the raw contenthash record of a .dot name (`asset-hub name content <name>`).
    #[command(external_subcommand)]
    Read(Vec<String>),
}

#[derive(Subcommand)]
pub enum TextCmd {
    /// Read a text record (e.g. `asset-hub name text get myapp00 manifest`).
    Get {
        /// The .dot name.
        name: String,
        /// Record key (e.g. manifest, executable, url).
        key: String,
    },
    /// Set a text record on a .dot name (signed Revive.call).
    Set {
        /// The .dot name (must be owned by the signer).
        name: String,
        /// Record key (e.g. manifest, executable).
        key: String,
        /// Record value.
        value: String,
    },
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
            let client = chain::asset_hub_client(env).await?;
            let contenthash = chain::resolve_contenthash(&client, env, &name).await?;
            if contenthash.is_empty() {
                println!("no contenthash set for {name}");
            } else {
                println!("{}", dotns::contenthash_to_cid(&contenthash)?);
            }
        }
        Cmd::Register { name } => {
            register(env, &name, mnemonic, derivation_path).await?;
        }
        Cmd::Content(ContentCmd::Read(args)) => {
            let raw = args.first().context("usage: name content <name>")?;
            let name = dotns::normalize_name(raw);
            let client = chain::asset_hub_client(env).await?;
            let contenthash = chain::resolve_contenthash(&client, env, &name).await?;
            if contenthash.is_empty() {
                println!("no contenthash set for {name}");
            } else {
                println!("0x{}", hex::encode(&contenthash));
            }
        }
        Cmd::Content(ContentCmd::Set { name, cid }) => {
            set(env, &name, &cid, mnemonic, derivation_path).await?;
        }
        Cmd::Text(TextCmd::Get { name, key }) => {
            let name = dotns::normalize_name(&name);
            let client = chain::asset_hub_client(env).await?;
            let value = chain::resolve_text(&client, env, &name, &key).await?;
            if value.is_empty() {
                println!("no '{key}' text record set for {name}");
            } else {
                println!("{value}");
            }
        }
        Cmd::Text(TextCmd::Set { name, key, value }) => {
            text_set(env, &name, &key, &value, mnemonic, derivation_path).await?;
        }
    }
    Ok(())
}

async fn register(
    env: &Env,
    name: &str,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let name = dotns::normalize_name(name);
    let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;

    let (owner, value_native) = chain::register_name(env, &signer, &name).await?;
    let cost_pas = value_native as f64 / 1e10;
    println!();
    ui::success(format!("registered {name}"));
    ui::kv("owner", format!("0x{}", hex::encode(owner.0)));
    ui::kv("cost", format!("~{cost_pas} PAS"));
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

    ui::step(format!("bind {name} → {}", ui::ellipsize(&cid.to_string())));
    let client = chain::asset_hub_client(env).await?;
    let expected = chain::set_contenthash(&client, env, &signer, &name, &cid).await?;

    let onchain = chain::resolve_contenthash(&client, env, &name).await?;
    if onchain != expected {
        bail!(
            "read-back mismatch: set 0x{} but chain has 0x{}",
            hex::encode(&expected),
            hex::encode(&onchain)
        );
    }
    ui::success(format!("bound {name}"));
    ui::kv("cid", cid);
    Ok(())
}

async fn text_set(
    env: &Env,
    name: &str,
    key: &str,
    value: &str,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let name = dotns::normalize_name(name);
    let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;

    ui::step(format!("set '{key}' on {name}"));
    let client = chain::asset_hub_client(env).await?;
    chain::set_text(&client, env, &signer, &name, key, value).await?;

    let onchain = chain::resolve_text(&client, env, &name, key).await?;
    if onchain != value {
        bail!("read-back mismatch: set '{value}' but chain has '{onchain}'");
    }
    ui::success(format!("set '{key}' on {name}"));
    ui::kv(key, value);
    Ok(())
}
