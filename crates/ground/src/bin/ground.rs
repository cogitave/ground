//! The `ground` command.

use clap::Parser;

#[tokio::main]
async fn main() {
    let cli = ground::cli::Cli::parse();
    std::process::exit(ground::cli::run_cli(cli).await);
}
