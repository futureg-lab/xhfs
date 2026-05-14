use crate::{
    bfs::{BruteFS, INodeKind, WriteOption},
    interface::config::Config,
    utils::{normalize_path, u64_to_utc_datetime},
};
use async_recursion::async_recursion;
use clap::{Args, Parser, Subcommand};
use eyre::OptionExt;
use std::{
    io::Write,
    path::{Path, PathBuf},
};
use tokio::{
    fs,
    io::{AsyncWriteExt, stdout},
};

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
    /// Write local file into brutefs
    Write(WriteCommand),
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
}

#[derive(Subcommand, Debug)]
pub enum FsSubcommands {
    /// List directory entries
    Ls(LsCommand),
    /// Copy file
    Cp(CopyCommand),
    /// Move file
    Mv(MoveCommand),
    /// Show entry statistics
    Stats(PathCommand),
    /// Remove file or folder
    Rm(PathCommand),
    /// Create a new directory
    Mkdir(PathCommand),
    /// Create a link from a target
    Ln(LinkCommand),
}

#[derive(Args, Debug)]
pub struct FsCommands {
    #[command(subcommand)]
    pub command: FsSubcommands,
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
pub struct WriteCommand {
    /// Local source file
    pub src_path: PathBuf,
    /// Destination path inside brutefs
    pub dest_path: PathBuf,
    #[arg(long, default_value = "false")]
    pub overwrite: bool,
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

#[derive(Args, Debug)]
pub struct LinkCommand {
    /// Source path inside brutefs
    pub src_path: PathBuf,
    /// Link destination path inside brutefs
    pub dest_path: PathBuf,
    #[arg(short, long, default_value = "false")]
    pub overwrite: bool,
    #[command(flatten)]
    pub global: GlobalOptions,
}

#[derive(Args, Debug)]
pub struct CatCommand {
    pub src_path: PathBuf,
    #[command(flatten)]
    pub global: GlobalOptions,
}

#[derive(Args, Debug)]
pub struct CopyCommand {
    /// Source path
    pub src: PathBuf,
    /// Destination path
    pub dest: PathBuf,
    #[arg(long, default_value = "false")]
    pub overwrite: bool,
    #[command(flatten)]
    pub global: GlobalOptions,
}

#[derive(Args, Debug)]
pub struct MoveCommand {
    /// Source path
    pub src: PathBuf,
    /// Destination path
    pub dest: PathBuf,
    #[command(flatten)]
    pub global: GlobalOptions,
}

#[derive(Args, Debug)]
pub struct LsCommand {
    /// Directory path
    #[arg(default_value = "/", value_parser = clap::value_parser!(PathBuf))]
    pub path: PathBuf,
    #[arg(long, default_value = "false")]
    pub tree: bool,
    #[command(flatten)]
    pub global: GlobalOptions,
}

impl MainCommand {
    pub async fn run(&self) -> eyre::Result<()> {
        match &self.command {
            Commands::Format(global_options) => {
                if global_options.force || confirm_destructive_action() {
                    let bfs = global_options.get_bfs(true).await?;
                    println!("Formatted {} Bytes", bfs.total_capacity()?);
                } else {
                    println!("abort");
                }
            }
            Commands::Write(w) => {
                let bfs = w.global.get_bfs(false).await?;
                let data = fs::read(&w.src_path).await?;
                bfs.fwrite(
                    &w.dest_path,
                    data,
                    WriteOption {
                        overwrite: w.overwrite,
                    },
                )
                .await?;
            }
            Commands::Download(d) => {
                let bfs = d.global.get_bfs(false).await?;
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
                let bfs = r.global.get_bfs(false).await?;
                let data = bfs.fread(&r.path).await?;
                let mut out = stdout();
                out.write_all(&data).await?;
                out.flush().await?;
            }
            Commands::X(x) => x.run().await?,
            Commands::Infos(global_options) => {
                let bfs = global_options.get_bfs(false).await?;
                println!("{}", bfs.format_headers_report().await?);
            }
        }

        Ok(())
    }
}

impl FsCommands {
    pub async fn run(&self) -> eyre::Result<()> {
        match &self.command {
            FsSubcommands::Ls(ls) => {
                if ls.tree {
                    ls.tree().await?
                } else {
                    ls.ls().await?
                }
            }
            FsSubcommands::Cp(cp) => cp.run().await?,
            FsSubcommands::Mv(mv) => mv.run().await?,
            FsSubcommands::Stats(pc) => pc.stats().await?,
            FsSubcommands::Rm(pc) => pc.rm().await?,
            FsSubcommands::Mkdir(pc) => pc.mkdir().await?,
            FsSubcommands::Ln(ln) => ln.run().await?,
        }

        Ok(())
    }
}

impl GlobalOptions {
    pub async fn get_bfs(&self, format_new: bool) -> eyre::Result<BruteFS> {
        let config = Config::load(self.config.clone())?;
        config.materialize(format_new, self.password.clone()).await
    }
}

impl LsCommand {
    pub async fn ls(&self) -> eyre::Result<()> {
        let ls = self;
        let bfs = ls.global.get_bfs(false).await?;
        let entries = bfs.ls(&ls.path).await?;
        for entry in entries {
            let full_path = ls.path.join(&entry);
            if ls.global.verbose {
                let stat = bfs
                    .stats(&full_path)
                    .await?
                    .ok_or_eyre(format!("Missing stat for {entry}"))?;

                let ctime = u64_to_utc_datetime(stat.ctime);

                let (kind, size_str) = match stat.kind {
                    INodeKind::File => {
                        let size = stat.size.unwrap_or(0);
                        ("FILE", bytesize::ByteSize(size as u64).to_string())
                    }
                    INodeKind::Directory => ("DIR", "-".to_string()),
                    INodeKind::Symlink => {
                        let size = stat.size.unwrap_or(0);
                        ("LINK", bytesize::ByteSize(size as u64).to_string())
                    }
                };

                println!(
                    "{:<4} {:<20} {:>10} {}",
                    kind,
                    ctime.format("%Y-%m-%d %H:%M:%S"),
                    size_str,
                    entry
                );
            } else {
                println!("{entry}");
            }
        }

        Ok(())
    }

    pub async fn tree(&self) -> eyre::Result<()> {
        let ls = self;
        let bfs = ls.global.get_bfs(false).await?;
        self.print_tree(&bfs, &self.path, "").await?;
        Ok(())
    }

    #[async_recursion(?Send)]
    async fn print_tree(&self, bfs: &BruteFS, path: &Path, prefix: &str) -> eyre::Result<()> {
        let entries = bfs.ls(path).await?;
        let count = entries.len();
        for (i, entry) in entries.into_iter().enumerate() {
            let is_last = i == count - 1;
            let connector = if is_last { "└── " } else { "├── " };
            let full_path = path.join(&entry);
            let stat = bfs.stats(&full_path).await?;

            let mut info = String::new();
            if self.global.verbose {
                if let Some(ref s) = stat {
                    let size = if matches!(s.kind, INodeKind::Directory) {
                        "-".to_string()
                    } else {
                        bytesize::ByteSize(s.size.unwrap_or(0) as u64).to_string()
                    };
                    info = format!(
                        " [{:<4} {:>8}]",
                        format!("{:?}", s.kind).to_uppercase(),
                        size
                    );
                }
            }

            println!("{}{}{}{}", prefix, connector, entry, info);

            if let Some(s) = stat {
                if matches!(s.kind, INodeKind::Directory) {
                    let new_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });
                    self.print_tree(bfs, &full_path, &new_prefix).await?;
                }
            }
        }
        Ok(())
    }
}

impl CopyCommand {
    pub async fn run(&self) -> eyre::Result<()> {
        let cp = self;
        let bfs = cp.global.get_bfs(false).await?;
        bfs.fcopy(
            &cp.src,
            &cp.dest,
            WriteOption {
                overwrite: cp.overwrite,
            },
        )
        .await?;
        Ok(())
    }
}

impl MoveCommand {
    pub async fn run(&self) -> eyre::Result<()> {
        let cp = self;
        let bfs = cp.global.get_bfs(false).await?;
        bfs.fmove(&cp.src, &cp.dest).await?;
        Ok(())
    }
}

impl PathCommand {
    pub async fn mkdir(&self) -> eyre::Result<()> {
        let bfs = self.global.get_bfs(false).await?;
        bfs.mkdir(&self.path, self.recursive).await?;
        Ok(())
    }

    pub async fn rm(&self) -> eyre::Result<()> {
        let bfs = self.global.get_bfs(false).await?;
        if self.recursive {
            eyre::bail!("Recursive option not supported yet for unlink");
        }
        bfs.unlink(&self.path).await?;
        Ok(())
    }

    pub async fn stats(&self) -> eyre::Result<()> {
        let bfs = self.global.get_bfs(false).await?;
        let stat = bfs.stats(&self.path).await?;
        if let Some(stat) = stat {
            let (kind, size_str) = match stat.kind {
                INodeKind::File => {
                    let size = stat.size.unwrap_or(0);
                    ("FILE", bytesize::ByteSize(size as u64).to_string())
                }
                INodeKind::Directory => ("DIR", "-".to_string()),
                INodeKind::Symlink => {
                    let size = stat.size.unwrap_or(0);
                    ("LINK", bytesize::ByteSize(size as u64).to_string())
                }
            };
            println!("Path: {}", normalize_path(&self.path));
            println!("Type: {kind}");
            println!("Size: {size_str}");
            println!(
                " Created Time: {}",
                u64_to_utc_datetime(stat.ctime).format("%Y-%m-%d %H:%M:%S")
            );
            println!(
                " Modified Time: {}",
                u64_to_utc_datetime(stat.mtime).format("%Y-%m-%d %H:%M:%S")
            );
            println!(
                " Updated Time: {}",
                u64_to_utc_datetime(stat.utime).format("%Y-%m-%d %H:%M:%S")
            );
        } else {
            println!("Path {} does not exist", normalize_path(&self.path));
        }
        Ok(())
    }
}

impl LinkCommand {
    pub async fn run(&self) -> eyre::Result<()> {
        let bfs = self.global.get_bfs(false).await?;
        bfs.create_link(
            &self.dest_path,
            &self.src_path,
            WriteOption {
                overwrite: self.overwrite,
            },
        )
        .await?;
        Ok(())
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
