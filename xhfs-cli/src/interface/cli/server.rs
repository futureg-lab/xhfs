use crate::{interface::cli::GlobalOptions, servers::webdav::webdav_main};
use clap::{Args, Subcommand};
use std::sync::Arc;

#[derive(Args, Debug)]
pub struct ServerCommands {
    #[command(subcommand)]
    pub command: ServerSubcommands,
}

#[derive(Subcommand, Debug)]
pub enum ServerSubcommands {
    /// Spawn a WebDAV server
    Webdav(Webdav),
}

#[derive(Args, Debug)]
pub struct Webdav {
    /// Server address
    #[arg(short, long, default_value = "127.0.0.1")]
    pub address: String,
    /// Server port
    #[arg(short, long, default_value = "1122")]
    pub port: u16,
    #[arg(short, long, default_value = "false")]
    pub read_only: bool,
    #[command(flatten)]
    pub global: GlobalOptions,
}

impl ServerCommands {
    pub async fn run(&self) -> eyre::Result<()> {
        match &self.command {
            ServerSubcommands::Webdav(w) => {
                let xhfs = Arc::new(w.global.get_xhfs().await?);
                webdav_main(w.address.clone(), w.port, xhfs, w.read_only).await?;
            }
        }
        Ok(())
    }
}
