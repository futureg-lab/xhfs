use crate::{
    bfs::{BruteFS, WriteOption},
    interface::{
        cli::{inspect::InspectCommands, x::FsCommands},
        config::Config,
    },
};
use clap::{Args, Parser, Subcommand};
use std::{io::Write, path::PathBuf};
use tokio::{
    fs::{self, File},
    io::{AsyncWriteExt, stdout},
};

mod inspect;
mod x;

#[derive(Parser, Debug)]
#[command(
    author = "michael-0acf4",
    version,
    about = "brutefs distributed File System"
)]
pub struct MainCommand {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Upload local file into brutefs
    Upload(UploadCommand),
    /// Download file from brutefs into local filesystem
    Download(DownloadCommand),
    /// Read the file immediately and print to stdout
    Read(PathCommand),
    /// Format a new brutefs
    Format(GlobalOptions),
    /// Show brutefs/device information
    Infos(GlobalOptions),
    /// brutefs operations
    X(FsCommands),
    /// Inspect current filesystem
    Inspect(InspectCommands),
}

#[derive(Args, Debug, Clone)]
pub struct GlobalOptions {
    /// Path to config yaml
    #[arg(long, default_value = "./brutefs.yaml")]
    pub config: PathBuf,
    #[arg(long)]
    pub password: Option<String>,
    /// Enable debug logs
    #[arg(long)]
    pub debug: bool,
    #[arg(short, long)]
    pub verbose: bool,
    #[arg(short, long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct PathCommand {
    /// Target path inside brutefs
    pub path: PathBuf,
    #[arg(long, short)]
    pub recursive: bool,
    #[command(flatten)]
    pub global: GlobalOptions,
}

#[derive(Args, Debug)]
pub struct UploadCommand {
    /// Local source file
    pub src_path: PathBuf,
    /// Destination path inside brutefs
    pub dest_path: PathBuf,
    #[arg(short, long, default_value = "false")]
    pub overwrite: bool,
    #[arg(short, long, default_value = "1024")]
    pub block_size: usize,
    #[arg(long, default_value = "false")]
    pub single_block: bool,
    #[command(flatten)]
    pub global: GlobalOptions,
}

#[derive(Args, Debug)]
pub struct DownloadCommand {
    /// Source path inside brutefs
    pub src_path: PathBuf,
    /// Local destination path
    pub dest_path: Option<PathBuf>,
    #[arg(short, long, default_value = "false")]
    pub overwrite: bool,
    #[command(flatten)]
    pub global: GlobalOptions,
}

impl MainCommand {
    pub async fn run(&self) -> eyre::Result<()> {
        match &self.command {
            Commands::Format(global_options) => {
                if global_options.force || confirm_destructive_action() {
                    let bfs = global_options.format_and_get_bfs().await?;
                    println!("Formatted {} Bytes", bfs.total_capacity()?);
                } else {
                    println!("abort");
                }
            }
            Commands::Upload(u) => {
                let bfs = u.global.get_bfs().await?;
                let data = fs::read(&u.src_path).await?;
                if u.single_block {
                    bfs.fwrite(
                        &u.dest_path,
                        data,
                        WriteOption {
                            overwrite: u.overwrite,
                        },
                    )
                    .await?;
                } else {
                    let file = File::open(&u.src_path).await?;
                    bfs.fwrite_stream(
                        &u.dest_path,
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
                let bfs = d.global.get_bfs().await?;
                let data = bfs.fread(&d.src_path).await?;
                let dest_path = match &d.dest_path {
                    Some(path) => path.clone(),
                    None => d.src_path.file_name().expect("Valid filename").into(),
                };
                if dest_path.exists() && !d.overwrite {
                    eyre::bail!("File {} already exists", dest_path.display());
                }
                fs::write(dest_path, data).await?;
            }
            Commands::Read(r) => {
                let bfs = r.global.get_bfs().await?;
                let data = bfs.fread(&r.path).await?;
                let mut out = stdout();
                out.write_all(&data).await?;
                out.flush().await?;
            }
            Commands::X(x) => x.run().await?,
            Commands::Infos(global_options) => {
                let bfs = global_options.get_bfs().await?;
                println!("{}", bfs.format_headers_report().await?);
            }
            Commands::Inspect(i) => i.command.run().await?,
        }

        Ok(())
    }
}

impl GlobalOptions {
    pub async fn format_and_get_bfs(&self) -> eyre::Result<BruteFS> {
        let config = Config::load(self.config.clone())?;
        config.materialize(true, self.password.clone()).await
    }

    pub async fn get_bfs(&self) -> eyre::Result<BruteFS> {
        let config = Config::load(self.config.clone())?;
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
