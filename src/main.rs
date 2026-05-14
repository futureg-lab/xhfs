use crate::interface::cli::MainCommand;
use clap::Parser;
use tracing_subscriber::EnvFilter;

pub mod addr;
pub mod bfs;
pub mod crypto;
pub mod device;
pub mod interface;
pub mod utils;

#[cfg(test)]
mod tests;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("brutefs=ERROR"))
        .unwrap();

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .without_time()
        .init();

    MainCommand::parse().run().await
}
