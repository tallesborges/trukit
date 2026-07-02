use crate::chain;
use crate::env::Env;
use crate::ui;
use anyhow::{Context, Result};
use clap::Subcommand;
use std::str::FromStr;
use subxt::utils::AccountId32;

#[derive(Subcommand)]
pub enum Cmd {
    /// Send native PAS to an account (Balances.transfer_keep_alive).
    Transfer {
        /// Destination SS58 address.
        dest: String,
        /// Amount in plancks (native PAS smallest unit).
        plancks: u128,
    },
    /// Ensure the signer has an H160 mapping (Revive.map_account).
    Map,
    /// DotNS naming ops (resolve, register, content records).
    #[command(subcommand)]
    Name(super::name::Cmd),
}

pub async fn run(
    env: &Env,
    cmd: Cmd,
    mnemonic: Option<String>,
    derivation_path: Option<String>,
) -> Result<()> {
    match cmd {
        Cmd::Transfer { dest, plancks } => {
            let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;
            let dest = AccountId32::from_str(&dest)
                .map_err(|e| anyhow::anyhow!("invalid SS58 dest address: {e}"))?;
            let tx = chain::transfer_keep_alive(env, &signer, dest, plancks)
                .await
                .context("transfer failed")?;
            ui::success(format!("transferred {plancks} plancks"));
            ui::kv("from", chain::account_id(&signer));
            ui::kv("to", dest);
            ui::kv("tx", format!("0x{}", hex::encode(tx)));
        }
        Cmd::Map => {
            let signer = chain::build_signer(mnemonic.as_deref(), derivation_path.as_deref())?;
            let client = chain::asset_hub_client(env).await?;
            chain::ensure_mapped(&client, &signer).await?;
            ui::success(format!(
                "account {} is mapped on Asset Hub",
                chain::account_id(&signer)
            ));
        }
        Cmd::Name(cmd) => super::name::run(env, cmd, mnemonic, derivation_path).await?,
    }
    Ok(())
}
