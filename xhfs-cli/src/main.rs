use crate::interface::cli::MainCommand;
use clap::Parser;
use tracing_subscriber::EnvFilter;

mod interface;
mod servers;

#[cfg(test)]
mod tests;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    dotenv::dotenv().ok();

    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("xhfs=ERROR"))
        .unwrap();

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .without_time()
        .init();

    MainCommand::parse().run().await
}
