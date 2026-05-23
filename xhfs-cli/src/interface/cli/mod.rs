use crate::interface::{
    cli::{inspect::InspectCommands, server::ServerCommands, x::*},
    config::Config,
};
use clap::{Args, Parser, Subcommand};
use futures::StreamExt;
use std::{io::Write, path::PathBuf};
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncWriteExt, stdout},
};
use xhfs_core::xhfs::*;

mod inspect;
mod server;
mod x;

#[derive(Parser, Debug)]
#[command(
    author = "michael-0acf4",
    version,
    about = "XHFS distributed File System"
)]
pub struct MainCommand {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Upload local file into XHFS
    Upload(UploadCommand),
    /// Download file from XHFS into local filesystem
    Download(DownloadCommand),
    /// Read the file immediately and print to stdout
    Read(ReadCommand),
    /// Format a new XHFS
    Format(GlobalOptions),
    /// Show XHFS/device information
    Info(GlobalOptions),
    /// XHFS operations
    X(FsCommands),
    /// Inspect current filesystem
    Inspect(InspectCommands),
    /// Server commands
    Server(ServerCommands),
}

#[derive(Args, Debug, Clone)]
pub struct GlobalOptions {
    /// Path to config yaml (default xhfs.yaml, or env XHFS_CONFIG)
    #[arg(long)]
    pub config: Option<PathBuf>,
    #[arg(long)]
    pub password: Option<String>,
    #[arg(short, long)]
    pub verbose: bool,
    #[arg(short, long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct UploadCommand {
    /// Local source file
    pub src_path: PathBuf,
    /// Destination path inside XHFS
    pub dest_path: Option<PathBuf>,
    #[arg(short, long, default_value = "false")]
    pub overwrite: bool,
    /// Maximum chunk of data to write at a time
    #[arg(short, long, default_value = "1048576")]
    pub block_size: usize,
    /// If set, will try to oneshot data write as a single block
    #[arg(long, default_value = "false")]
    pub single_block: bool,
    #[command(flatten)]
    pub global: GlobalOptions,
}

#[derive(Args, Debug)]
pub struct DownloadCommand {
    /// Source path inside XHFS
    pub src_path: PathBuf,
    /// Local destination path
    pub dest_path: Option<PathBuf>,
    #[arg(short, long, default_value = "false")]
    pub overwrite: bool,
    /// Maximum chunk of data to read at a time
    #[arg(short, long, default_value = "1048576")]
    pub block_size: usize,
    #[arg(short, long, default_value = "false")]
    pub single_block: bool,
    #[command(flatten)]
    pub global: GlobalOptions,
}

#[derive(Args, Debug)]
pub struct ReadCommand {
    /// Source path inside XHFS
    pub src_path: PathBuf,
    #[arg(short, long, default_value = "false")]
    pub overwrite: bool,
    /// Maximum chunk of data to read at a time
    #[arg(short, long, default_value = "1048576")]
    pub block_size: usize,
    #[arg(short, long, default_value = "false")]
    pub single_block: bool,
    #[command(flatten)]
    pub global: GlobalOptions,
}

impl MainCommand {
    pub async fn run(&self) -> eyre::Result<()> {
        match &self.command {
            Commands::Format(global_options) => {
                if global_options.force || confirm_destructive_action() {
                    let xhfs = global_options.format_and_get_xhfs().await?;
                    println!("Formatted {} Bytes", xhfs.total_capacity()?);
                } else {
                    println!("abort");
                }
            }
            Commands::Upload(u) => {
                let xhfs = u.global.get_xhfs().await?;
                let data = fs::read(&u.src_path).await?;
                let dest_path = match &u.dest_path {
                    Some(path) => path.to_owned(),
                    None => PathBuf::from(u.src_path.file_name().ok_or_else(|| {
                        eyre::eyre!(
                            "Could not derive destination path from source {}",
                            u.src_path.display()
                        )
                    })?),
                };

                if u.single_block {
                    xhfs.fwrite(
                        &dest_path,
                        data,
                        WriteOption {
                            overwrite: u.overwrite,
                        },
                    )
                    .await?;
                } else {
                    let file = File::open(&u.src_path).await?;
                    xhfs.fwrite_stream(
                        &dest_path,
                        file,
                        u.block_size,
                        WriteOption {
                            overwrite: u.overwrite,
                        },
                    )
                    .await?;
                }
            }
            Commands::Download(d) => {
                let xhfs = d.global.get_xhfs().await?;
                let dest_path = match &d.dest_path {
                    Some(path) => path.clone(),
                    None => d.src_path.file_name().expect("Valid filename").into(),
                };
                if dest_path.exists() && !d.overwrite {
                    eyre::bail!("File {} already exists", dest_path.display());
                }
                if d.single_block {
                    let data = xhfs.fread(&d.src_path).await?;
                    tokio::fs::write(&dest_path, data).await?;
                } else {
                    let mut data = xhfs.fread_stream(&d.src_path, d.block_size).await?;
                    let mut file = OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(d.overwrite)
                        .create_new(!d.overwrite)
                        .open(&dest_path)
                        .await?;
                    while let Some(chunk) = data.next().await {
                        let chunk = chunk?;
                        file.write_all(&chunk).await?;
                    }
                    file.flush().await?;
                }
            }
            Commands::Read(r) => {
                let xhfs = r.global.get_xhfs().await?;
                let mut out = stdout();
                if r.single_block {
                    let data = xhfs.fread(&r.src_path).await?;
                    out.write_all(&data).await?;
                    out.flush().await?;
                } else {
                    let mut data = xhfs.fread_stream(&r.src_path, r.block_size).await?;
                    let mut out = stdout();
                    while let Some(chunk) = data.next().await {
                        let chunk = chunk?;
                        out.write_all(&chunk).await?;
                    }
                }
                out.flush().await?;
            }
            Commands::X(x) => x.run().await?,
            Commands::Info(global_options) => {
                println!(
                    "Config used: {}",
                    global_options.resolve_config_path().display()
                );
                let xhfs = global_options.get_xhfs().await?;
                println!("{}", xhfs.format_headers_report().await?);
            }
            Commands::Inspect(i) => i.command.run().await?,
            Commands::Server(s) => s.run().await?,
        }

        Ok(())
    }
}

impl GlobalOptions {
    fn resolve_config_path(&self) -> PathBuf {
        match &self.config {
            Some(config) => config.clone(),
            None => match std::env::var("XHFS_CONFIG") {
                Ok(val) => PathBuf::from(val),
                Err(_) => PathBuf::from("xhfs.yaml"),
            },
        }
    }

    pub async fn format_and_get_xhfs(&self) -> eyre::Result<XHFS> {
        let config = Config::load(self.resolve_config_path())?;
        config.materialize(true, self.password.clone()).await
    }

    pub async fn get_xhfs(&self) -> eyre::Result<XHFS> {
        let config = Config::load(self.resolve_config_path())?;
        config.materialize(false, self.password.clone()).await
    }
}

fn confirm_destructive_action() -> bool {
    print!("This action is destructive. Proceed? [y/N]: ");
    std::io::stdout().flush().unwrap();
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .expect("Failed to read line");

    match input.trim().to_lowercase().as_str() {
        "y" | "yes" => true,
        _ => false,
    }
}
