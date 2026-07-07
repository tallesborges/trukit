mod bulletin;
mod chain;
mod commands;
mod config;
mod dotns;
mod env;
mod merkle;
mod pool;
mod ui;

use clap::{Parser, Subcommand};

/// Which Bulletin upload pool to sign with (maps to [`pool::PoolSource`]).
#[derive(Clone, Copy, clap::ValueEnum)]
enum PoolArg {
    Local,
    Shared,
}

/// A fast CLI for the Triangle/Trinity ecosystem: Bulletin storage and DotNS
/// naming (Asset Hub / pallet_revive).
#[derive(Parser)]
#[command(name = "dotkit", version, about)]
struct Cli {
    /// Target environment (drives Bulletin RPC + Asset Hub contract addresses together).
    #[arg(long, global = true, default_value = "paseo-next-v2")]
    env: String,

    /// Signer mnemonic (falls back to $MNEMONIC then $DOTNS_MNEMONIC; defaults to a shared dev account on testnets).
    #[arg(long, global = true)]
    mnemonic: Option<String>,

    /// Substrate derivation path appended to the mnemonic (e.g. //Alice).
    #[arg(long, global = true)]
    derivation_path: Option<String>,

    /// Bulletin upload pool: `local` (private ~/.dotkit pool) or `shared` (shared dev-phrase pool).
    /// Default: local if a keystore exists, else shared.
    #[arg(long, global = true, value_enum)]
    pool: Option<PoolArg>,

    /// Suppress step/detail output; only errors are printed (useful in CI/scripts).
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Emit a single machine-readable JSON object per command instead of human output.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Deploy a static app: merkleize -> Bulletin -> bind .dot contenthash (MVP).
    Deploy(commands::deploy::Args),
    /// Bulletin chain storage ops.
    #[command(subcommand)]
    Bulletin(commands::bulletin::Cmd),
    /// Asset Hub ops: transfers, H160 mapping, and DotNS naming.
    #[command(name = "asset-hub", subcommand)]
    AssetHub(commands::asset_hub::Cmd),
    /// Signer / environment utilities (multichain).
    #[command(subcommand)]
    Account(commands::account::Cmd),
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        ui::error(&err);
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    ui::set_quiet(cli.quiet);
    ui::set_json(cli.json);
    let env = env::Env::resolve(&cli.env)?;
    let pool_source = match cli.pool {
        None => pool::PoolSource::Auto,
        Some(PoolArg::Local) => pool::PoolSource::Local,
        Some(PoolArg::Shared) => pool::PoolSource::Shared,
    };
    match cli.command {
        Command::Deploy(args) => {
            let mnemonic = cli
                .mnemonic
                .or_else(|| std::env::var("MNEMONIC").ok())
                .or_else(|| std::env::var("DOTNS_MNEMONIC").ok());
            commands::deploy::run(&env, args, mnemonic, cli.derivation_path, pool_source).await
        }
        Command::Bulletin(cmd) => {
            commands::bulletin::run(&env, cmd, cli.mnemonic, cli.derivation_path, pool_source).await
        }
        Command::AssetHub(cmd) => {
            let mnemonic = cli
                .mnemonic
                .or_else(|| std::env::var("MNEMONIC").ok())
                .or_else(|| std::env::var("DOTNS_MNEMONIC").ok());
            commands::asset_hub::run(&env, cmd, mnemonic, cli.derivation_path).await
        }
        Command::Account(cmd) => {
            let mnemonic = cli
                .mnemonic
                .or_else(|| std::env::var("MNEMONIC").ok())
                .or_else(|| std::env::var("DOTNS_MNEMONIC").ok());
            commands::account::run(&env, cmd, mnemonic, cli.derivation_path).await
        }
    }
}
