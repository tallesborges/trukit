mod chain;
mod commands;
mod dotns;
mod env;
mod merkle;
mod registrar;
mod ui;

use clap::{Parser, Subcommand};

/// A fast CLI for the Triangle/Trinity ecosystem: Bulletin storage, DotNS naming
/// (Asset Hub / pallet_revive), and People / Statement Store.
#[derive(Parser)]
#[command(name = "trukit", version, about)]
struct Cli {
    /// Target environment (drives Bulletin RPC + Asset Hub contract addresses together).
    #[arg(long, global = true, default_value = "paseo-next-v2")]
    env: String,

    /// Signer mnemonic (falls back to $MNEMONIC then $DOTNS_MNEMONIC; defaults to the shared bare-master dev account used on testnets).
    #[arg(long, global = true)]
    mnemonic: Option<String>,

    /// Substrate derivation path appended to the mnemonic (e.g. //Alice or //deploy/0).
    #[arg(long, global = true)]
    derivation_path: Option<String>,

    /// Suppress step/detail output; only errors are printed (useful in CI/scripts).
    #[arg(short, long, global = true)]
    quiet: bool,

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
    /// DotNS naming ops on Asset Hub.
    #[command(subcommand)]
    Name(commands::name::Cmd),
    /// People chain / Statement Store ops (research-first; wire format unverified).
    #[command(subcommand)]
    Statement(commands::statement::Cmd),
    /// Signer / account / environment utilities.
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
    let env = env::Env::resolve(&cli.env)?;
    match cli.command {
        Command::Deploy(args) => {
            let mnemonic = cli
                .mnemonic
                .or_else(|| std::env::var("MNEMONIC").ok())
                .or_else(|| std::env::var("DOTNS_MNEMONIC").ok());
            commands::deploy::run(&env, args, mnemonic, cli.derivation_path).await
        }
        Command::Bulletin(cmd) => {
            commands::bulletin::run(&env, cmd, cli.mnemonic, cli.derivation_path).await
        }
        Command::Name(cmd) => {
            let mnemonic = cli
                .mnemonic
                .or_else(|| std::env::var("MNEMONIC").ok())
                .or_else(|| std::env::var("DOTNS_MNEMONIC").ok());
            commands::name::run(&env, cmd, mnemonic, cli.derivation_path).await
        }
        Command::Statement(cmd) => commands::statement::run(&env, cmd),
        Command::Account(cmd) => {
            let mnemonic = cli
                .mnemonic
                .or_else(|| std::env::var("MNEMONIC").ok())
                .or_else(|| std::env::var("DOTNS_MNEMONIC").ok());
            commands::account::run(&env, cmd, mnemonic, cli.derivation_path).await
        }
    }
}
