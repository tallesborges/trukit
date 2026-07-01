use crate::chain;
use crate::env::Env;
use anyhow::{Context, Result};
use clap::Subcommand;
use std::str::FromStr;
use subxt::utils::AccountId32;

#[derive(Subcommand)]
pub enum Cmd {
    /// Print the resolved environment configuration.
    Env,
    /// Derive the signer and prove connectivity to Asset Hub + Bulletin.
    Whoami,
    /// Ensure the signer has an H160 mapping on Asset Hub (Revive.map_account).
    Map,
    /// Send native PAS to an account on Asset Hub (Balances.transfer_keep_alive).
    Transfer {
        /// Destination SS58 address.
        dest: String,
        /// Amount in plancks (native PAS smallest unit).
        plancks: u128,
    },
}

pub async fn run(
    env: &Env,
    cmd: Cmd,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    match cmd {
        Cmd::Env => {
            println!("env                    {}", env.id);
            println!("bulletin_rpc           {}", env.bulletin_rpc);
            println!("asset_hub_rpc          {}", env.asset_hub_rpc);
            println!("ipfs_gateway           {}", env.ipfs_gateway);
            println!("dotns_content_resolver {}", env.dotns_content_resolver);
        }
        Cmd::Whoami => {
            let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;
            let account = chain::account_id(&signer);
            let (asset_hub, bulletin) =
                tokio::try_join!(chain::asset_hub_client(env), chain::bulletin_client(env))?;
            let h160 = chain::revive_address(&asset_hub, account.clone()).await?;
            let asset_hub_block = asset_hub.at_current_block().await?.block_number();
            let bulletin_block = bulletin.at_current_block().await?.block_number();

            println!("env         {}", env.id);
            println!("ss58        {account}");
            println!("h160        0x{}", hex::encode(h160.0));
            println!("asset_hub   {}  #{asset_hub_block}", env.asset_hub_rpc);
            println!("bulletin    {}  #{bulletin_block}", env.bulletin_rpc);
        }
        Cmd::Map => {
            let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;
            let client = chain::asset_hub_client(env).await?;
            chain::ensure_mapped(&client, &signer).await?;
            println!(
                "account {} is mapped on Asset Hub",
                chain::account_id(&signer)
            );
        }
        Cmd::Transfer { dest, plancks } => {
            let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;
            let dest = AccountId32::from_str(&dest)
                .map_err(|e| anyhow::anyhow!("invalid SS58 dest address: {e}"))?;
            let tx = chain::transfer_keep_alive(env, &signer, dest, plancks)
                .await
                .context("transfer failed")?;
            println!(
                "transferred {plancks} plancks {} -> {dest} (tx 0x{})",
                chain::account_id(&signer),
                hex::encode(tx)
            );
        }
    }
    Ok(())
}
