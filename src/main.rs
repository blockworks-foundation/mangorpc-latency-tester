pub mod config;
pub mod discord;
pub mod measure_txs;
pub mod rpcnode_check_alive;
pub mod rpcnode_define_checks;
pub mod slot_latency_tester;

use anyhow::bail;
use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use config::MeasureTxsConfig;
use config::ParsedConfig;
use measure_txs::watch_measure_txs;
use rpcnode_check_alive::check;
use slot_latency_tester::measure_slot_latency;

#[derive(Parser)]
#[command(name = "node-checker")]
#[command(author, version, about, long_about=None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Args)]
struct CheckAlive {
    #[arg(long, help = "discord webhook URL to send notifications to")]
    discord_webhook: Option<String>,
    #[arg(long, help = "label for identifying the RPC node")]
    rpcnode_label: Option<String>,
    #[arg(long, help = "comma-separated list of checks to enable")]
    checks_enabled: Option<String>,
}

#[derive(Args)]
struct WatchMeasureSendTransaction {
    #[arg(
        long,
        help = "how often to check nodes by sending transactions to them (in seconds)"
    )]
    watch_interval_seconds: u64,
}

#[derive(Subcommand)]
enum Commands {
    #[clap(aliases = &["a"], about = "check if a single node is alive")]
    CheckAlive(CheckAlive),
    #[clap(aliases = &["l"], about = "measure slot latency between different nodes")]
    MeasureSlotLatency,
    #[clap(aliases = &["wt"], about = "measure tx submission times to different nodes every n seconds")]
    WatchMeasureSendTransaction(WatchMeasureSendTransaction),
}

#[tokio::main]
async fn main() -> Result<()> {
    let ParsedConfig {
        measure_txs:
            MeasureTxsConfig {
                pubsub_url,
                rpc_url,
                helius_url,
                urls_by_label,
                ..
            },
        user,
        .. // general is set globally using OnceCell
    } = config::setup()?;
    let cli = Cli::parse();
    match cli.command {
        Some(command) => match command {
            Commands::CheckAlive(CheckAlive {
                discord_webhook,
                rpcnode_label,
                checks_enabled,
            }) => {
                check(discord_webhook, rpcnode_label, checks_enabled).await?;
                Ok(())
            }
            Commands::MeasureSlotLatency => measure_slot_latency().await,
            Commands::WatchMeasureSendTransaction(WatchMeasureSendTransaction {
                watch_interval_seconds,
            }) => {
                watch_measure_txs(
                    user,
                    pubsub_url,
                    rpc_url,
                    helius_url,
                    urls_by_label,
                    watch_interval_seconds,
                )
                .await
            }
        },
        None => {
            bail!("no command given");
        }
    }
}
