use crate::chain;
use crate::env::Env;
use crate::ui;
use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum Cmd {
    /// Print the resolved environment configuration.
    Env,
    /// Derive the signer and prove connectivity to Asset Hub + Bulletin.
    Whoami,
}

pub async fn run(
    env: &Env,
    cmd: Cmd,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    match cmd {
        Cmd::Env => {
            ui::kv("env", env.id);
            ui::kv("bulletin", env.bulletin_rpc);
            ui::kv("asset_hub", env.asset_hub_rpc);
            ui::kv("gateway", env.ipfs_gateway);
            ui::kv("resolver", env.dotns_content_resolver);
        }
        Cmd::Whoami => {
            let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;
            let account = chain::account_id(&signer);
            let (asset_hub, bulletin) =
                tokio::try_join!(chain::asset_hub_client(env), chain::bulletin_client(env))?;
            let h160 = chain::revive_address(&asset_hub, account).await?;
            let asset_hub_block = asset_hub.at_current_block().await?.block_number();
            let bulletin_block = bulletin.at_current_block().await?.block_number();

            ui::kv("env", env.id);
            ui::kv("ss58", account);
            ui::kv("h160", format!("0x{}", hex::encode(h160.0)));
            ui::kv(
                "asset_hub",
                format!("{}  #{asset_hub_block}", env.asset_hub_rpc),
            );
            ui::kv(
                "bulletin",
                format!("{}  #{bulletin_block}", env.bulletin_rpc),
            );
        }
    }
    Ok(())
}
