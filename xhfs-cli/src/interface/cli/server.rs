use std::sync::Arc;

use clap::{Args, Subcommand};

use crate::{interface::cli::GlobalOptions, servers::webdav::webdav_main};

#[derive(Args, Debug)]
pub struct ServerCommands {
    #[command(subcommand)]
    pub command: ServerSubcommands,
}

#[derive(Subcommand, Debug)]
pub enum ServerSubcommands {
    /// Spawn a Webdav server
    Webdav(Webdav),
}

#[derive(Args, Debug)]
pub struct Webdav {
    #[arg(short, long, default_value = "127.0.0.1")]
    pub address: String,
    /// Server port
    #[arg(short, long, default_value = "1122")]
    pub port: u16,
    #[command(flatten)]
    pub global: GlobalOptions,
}

impl ServerCommands {
    pub async fn run(&self) -> eyre::Result<()> {
        match &self.command {
            ServerSubcommands::Webdav(w) => {
                let xhfs = w.global.get_xhfs().await?;
                webdav_main(w.address.clone(), w.port, Arc::new(xhfs)).await?;
            }
        }
        Ok(())
    }
}
