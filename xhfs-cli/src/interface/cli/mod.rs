use crate::interface::{
    cli::{inspect::InspectCommands, server::ServerCommands, x::*},
    config::Config,
};
use clap::{Args, Parser, Subcommand};
use eyre::Context;
use futures::StreamExt;
use std::{
    collections::HashMap,
    io::Write,
    path::PathBuf,
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncWriteExt, stdout},
    sync::RwLock,
};
use xhfs_core::{
    device::{ConcreteDevice, disk::Controller, kv_device::*, logical::LogicalDevice},
    utils::systime_to_u64,
    xhfs::{crypto::KeyDerivation, ds::PrettySize, *},
};

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
    pub password: bool,
    #[arg(short, long)]
    pub verbose: bool,
    #[arg(short, long)]
    pub force: bool,
    /// Activate dev mode flags (only relevant for server related features)
    #[arg(long, default_value = "false")]
    pub dev: bool,
    /// Physical unit capacity (must be divisible by 1024)
    #[arg(long, default_value = "134217728")]
    pub dev_unit_capacity: usize,
    /// Physical unit replication count
    #[arg(long, default_value = "1")]
    pub dev_replica_count: u8,
    /// How many logical device rassembling each dev_replica_count
    #[arg(long, default_value = "1")]
    pub dev_logical_count: usize,
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
                    println!("Formatted {}", PrettySize(xhfs.total_capacity()? as u64));
                } else {
                    println!("abort");
                }
            }
            Commands::Upload(u) => {
                let xhfs = u.global.get_xhfs().await?;
                let data = fs::read(&u.src_path).await?;
                let modified = fs::metadata(&u.src_path)
                    .await
                    .map(|m| m.modified().ok())
                    .ok()
                    .flatten()
                    .map(systime_to_u64);

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
                            modified,
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
                            modified,
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
                if let Some(stats) = xhfs.stats(&d.src_path, false).await? {
                    let file = std::fs::OpenOptions::new().write(true).open(&dest_path)?;
                    let time = UNIX_EPOCH + Duration::from_secs(stats.mtime);
                    file.set_modified(time)?;
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
                    "Config loaded: {}",
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

    fn resolve_password(&self) -> Option<String> {
        if self.password {
            let password = rpassword::prompt_password("Password: ").unwrap();
            if password.is_empty() {
                return None;
            }
            Some(password)
        } else {
            std::env::var("XHFS_PASSWORD").ok()
        }
    }

    async fn load_xhfs(&self, format: bool) -> eyre::Result<XHFS> {
        if self.dev {
            println!("In-memory mode enabled, the configs will be ignored.");
            let xhfs = create_simple_memory_xhfs(
                self.dev_unit_capacity,
                self.dev_replica_count,
                self.dev_logical_count,
            )
            .await?;
            return Ok(xhfs);
        }

        let config = Config::load(self.resolve_config_path())?;
        let password = self.resolve_password();
        let xhfs = config.materialize(format, password).await?;
        xhfs.get_root_inode().await.with_context(|| {
            "Failed to decrypt data. The password may be incorrect or the data may be corrupted.".to_string()
        })?;
        Ok(xhfs)
    }

    pub async fn format_and_get_xhfs(&self) -> eyre::Result<XHFS> {
        self.load_xhfs(true).await
    }

    pub async fn get_xhfs(&self) -> eyre::Result<XHFS> {
        self.load_xhfs(false).await
    }
}

fn confirm_destructive_action() -> bool {
    print!("This action is destructive. Proceed? [y/N]: ");
    std::io::stdout().flush().unwrap();
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .expect("Failed to read line");

    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

async fn create_simple_memory_xhfs(
    unit_capacity: usize,
    replica_count: u8,
    logical_count: usize,
) -> eyre::Result<XHFS> {
    let slot_capacity = 1024;
    if !unit_capacity.is_multiple_of(slot_capacity) {
        eyre::bail!("In-memory capacity must be divisible by {slot_capacity}");
    }

    let mut logical_devices = vec![];
    for _ in 0..logical_count {
        let devices = (0..replica_count)
            .map(|_| {
                ConcreteDevice::KVDevice(KVDevice {
                    store: Arc::new(MemoryKV(RwLock::new(HashMap::new()))),
                    total_slots: unit_capacity / slot_capacity,
                    slot_capacity,
                })
            })
            .collect::<Vec<_>>();

        logical_devices.push(LogicalDevice::new(2, devices)?);
    }

    let ctrl = Controller::from(logical_devices).await?;
    XHFS::format_new(ctrl, None, KeyDerivation::default()).await
}
