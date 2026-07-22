use clap::Parser;

use prist::cli::Cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    prist::commands::run(cli).await
}
