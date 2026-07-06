use crate::chain;
use crate::dotns;
use crate::env::Env;
use crate::ui;
use anyhow::{bail, Context, Result};
use cid::Cid;
use clap::Subcommand;
use serde_json::json;
use subxt::utils::H160;

#[derive(Subcommand)]
pub enum Cmd {
    /// Resolve a .dot name to its contenthash CID.
    Resolve {
        /// The .dot name (e.g. myapp00.dot).
        name: String,
    },
    /// Show whether a .dot name is registered and who owns it.
    #[command(name = "owner-of", alias = "oo")]
    OwnerOf {
        /// The .dot name (e.g. myapp00.dot).
        name: String,
    },
    /// Read-only overview of a .dot name (owner, tier, price, contenthash).
    Lookup {
        /// The .dot name (e.g. myapp00.dot).
        name: String,
    },
    /// Register an open-tier .dot name (commit/reveal) to the signer.
    Register {
        /// The .dot name to register (e.g. myapp00.dot).
        name: String,
    },
    /// Transfer a .dot name you own to another account (0x H160 or SS58).
    Transfer {
        /// The .dot name to transfer (must be owned by the signer).
        name: String,
        /// Recipient address: a 0x-prefixed H160 or an SS58 address.
        to: String,
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
            let contenthash = dotns::resolve_contenthash(&client, env, &name).await?;
            let cid = if contenthash.is_empty() {
                None
            } else {
                Some(dotns::contenthash_to_cid(&contenthash)?)
            };
            if ui::json() {
                ui::emit(&json!({ "name": name, "cid": cid }));
            } else {
                match cid {
                    Some(cid) => println!("{cid}"),
                    None => println!("no contenthash set for {name}"),
                }
            }
        }
        Cmd::OwnerOf { name } => {
            owner_of(env, &name).await?;
        }
        Cmd::Lookup { name } => {
            lookup(env, &name).await?;
        }
        Cmd::Register { name } => {
            register(env, &name, mnemonic, derivation_path).await?;
        }
        Cmd::Transfer { name, to } => {
            transfer(env, &name, &to, mnemonic, derivation_path).await?;
        }
        Cmd::Content(ContentCmd::Read(args)) => {
            let raw = args.first().context("usage: name content <name>")?;
            let name = dotns::normalize_name(raw);
            let client = chain::asset_hub_client(env).await?;
            let contenthash = dotns::resolve_contenthash(&client, env, &name).await?;
            let hex = (!contenthash.is_empty()).then(|| format!("0x{}", hex::encode(&contenthash)));
            if ui::json() {
                ui::emit(&json!({ "name": name, "contenthash": hex }));
            } else {
                match hex {
                    Some(hex) => println!("{hex}"),
                    None => println!("no contenthash set for {name}"),
                }
            }
        }
        Cmd::Content(ContentCmd::Set { name, cid }) => {
            set(env, &name, &cid, mnemonic, derivation_path).await?;
        }
        Cmd::Text(TextCmd::Get { name, key }) => {
            let name = dotns::normalize_name(&name);
            let client = chain::asset_hub_client(env).await?;
            let value = dotns::resolve_text(&client, env, &name, &key).await?;
            if ui::json() {
                ui::emit(&json!({ "name": name, "key": key, "value": value }));
            } else if value.is_empty() {
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

async fn owner_of(env: &Env, name: &str) -> Result<()> {
    let name = dotns::normalize_name(name);
    let client = chain::asset_hub_client(env).await?;
    let owner = dotns::name_owner(&client, env, &name).await?;
    let owner_hex = owner.map(|o| format!("0x{}", hex::encode(o.0)));

    if ui::json() {
        ui::emit(&json!({
            "name": name,
            "registered": owner.is_some(),
            "owner": owner_hex,
        }));
    } else {
        ui::kv("name", &name);
        match owner_hex {
            Some(owner) => {
                ui::kv("registered", "yes");
                ui::kv("owner", owner);
            }
            None => ui::kv("registered", "no (unregistered)"),
        }
    }
    Ok(())
}

async fn lookup(env: &Env, name: &str) -> Result<()> {
    let name = dotns::normalize_name(name);
    let client = chain::asset_hub_client(env).await?;

    let owner = dotns::name_owner(&client, env, &name).await?;
    let owner_hex = owner.map(|o| format!("0x{}", hex::encode(o.0)));

    let contenthash = dotns::resolve_contenthash(&client, env, &name).await?;
    let cid = if contenthash.is_empty() {
        None
    } else {
        Some(dotns::contenthash_to_cid(&contenthash)?)
    };

    // Classification (required PoP tier + human status) reverts for labels that
    // break the digit-suffix rule; treat that as "unavailable" rather than fatal.
    let classify = dotns::classify_name(&client, env, &name).await.ok();
    let (tier, status) = match &classify {
        Some((tier, status)) => (Some(*tier), Some(status.clone())),
        None => (None, None),
    };

    // Base list price for a fresh registrant (zero owner ⇒ no discount); best-effort.
    let price = dotns::name_price_native(&client, env, &name, H160([0u8; 20]))
        .await
        .ok();

    if ui::json() {
        ui::emit(&json!({
            "name": name,
            "registered": owner.is_some(),
            "owner": owner_hex,
            "required_tier": tier,
            "tier_name": tier.map(dotns::tier_name),
            "status": status,
            "price_pas": price.map(|p| p as f64 / 1e10),
            "cid": cid,
        }));
    } else {
        ui::kv("name", &name);
        match owner_hex {
            Some(owner) => {
                ui::kv("registered", "yes");
                ui::kv("owner", owner);
            }
            None => ui::kv("registered", "no (available)"),
        }
        if let Some(tier) = tier {
            ui::kv("tier", format!("{} ({tier})", dotns::tier_name(tier)));
        }
        if let Some(status) = &status {
            ui::kv("status", status);
        }
        if let Some(price) = price {
            ui::kv("price", format!("~{} PAS", price as f64 / 1e10));
        }
        ui::kv("content", cid.as_deref().unwrap_or("(none)"));
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

    let (owner, value_native) = dotns::register_name(env, &signer, &name).await?;
    let cost_pas = value_native as f64 / 1e10;
    if ui::json() {
        ui::emit(&json!({
            "name": name,
            "owner": format!("0x{}", hex::encode(owner.0)),
            "cost_pas": cost_pas,
        }));
    } else {
        println!();
        ui::success(format!("registered {name}"));
        ui::kv("owner", format!("0x{}", hex::encode(owner.0)));
        ui::kv("cost", format!("~{cost_pas} PAS"));
    }
    Ok(())
}

async fn transfer(
    env: &Env,
    name: &str,
    to: &str,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    let name = dotns::normalize_name(name);
    let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;

    let outcome = dotns::transfer_name(env, &signer, &name, to).await?;
    let fee_pas = outcome.fee_native as f64 / 1e10;
    if ui::json() {
        ui::emit(&json!({
            "name": name,
            "from": format!("0x{}", hex::encode(outcome.from.0)),
            "to": format!("0x{}", hex::encode(outcome.to.0)),
            "fee_pas": fee_pas,
            "tx": format!("0x{}", hex::encode(outcome.tx)),
        }));
    } else {
        ui::success(format!("transferred {name}"));
        ui::kv("from", format!("0x{}", hex::encode(outcome.from.0)));
        ui::kv("to", format!("0x{}", hex::encode(outcome.to.0)));
        ui::kv("fee", format!("~{fee_pas} PAS"));
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

    ui::step(format!("bind {name} → {}", ui::ellipsize(&cid.to_string())));
    let client = chain::asset_hub_client(env).await?;
    let expected = dotns::set_contenthash(&client, env, &signer, &name, &cid).await?;

    let onchain = dotns::resolve_contenthash(&client, env, &name).await?;
    if onchain != expected {
        bail!(
            "read-back mismatch: set 0x{} but chain has 0x{}",
            hex::encode(&expected),
            hex::encode(&onchain)
        );
    }
    if ui::json() {
        ui::emit(&json!({ "name": name, "cid": cid.to_string() }));
    } else {
        ui::success(format!("bound {name}"));
        ui::kv("cid", cid);
    }
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
    dotns::set_text(&client, env, &signer, &name, key, value).await?;

    let onchain = dotns::resolve_text(&client, env, &name, key).await?;
    if onchain != value {
        bail!("read-back mismatch: set '{value}' but chain has '{onchain}'");
    }
    if ui::json() {
        ui::emit(&json!({ "name": name, "key": key, "value": value }));
    } else {
        ui::success(format!("set '{key}' on {name}"));
        ui::kv(key, value);
    }
    Ok(())
}
