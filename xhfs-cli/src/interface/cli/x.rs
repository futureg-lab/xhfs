use crate::interface::cli::GlobalOptions;
use async_recursion::async_recursion;
use clap::{Args, Subcommand};
use eyre::OptionExt;
use std::path::{Path, PathBuf};
use xhfs_core::{
    utils::{normalize_path, u64_to_utc_datetime},
    xhfs::{WriteOption, XHFS, ds::INodeKind},
};

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

#[derive(Args, Debug)]
pub struct PathCommand {
    /// Target path inside XHFS
    pub path: PathBuf,
    #[arg(long, short)]
    pub recursive: bool,
    #[command(flatten)]
    pub global: GlobalOptions,
}

#[derive(Args, Debug)]
pub struct LinkCommand {
    /// Source path inside XHFS
    pub src_path: PathBuf,
    /// Link destination path inside XHFS
    pub dest_path: PathBuf,
    /// As Symlink
    #[arg(short, long, default_value = "false")]
    pub symlink: bool,
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
    #[arg(long, default_value = "false")]
    pub recursive: bool,
    #[command(flatten)]
    pub global: GlobalOptions,
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

impl LsCommand {
    pub async fn ls(&self) -> eyre::Result<()> {
        let ls = self;
        let xhfs = ls.global.get_xhfs().await?;
        let entries = xhfs.ls(&ls.path).await?;
        for entry in entries {
            let full_path = ls.path.join(&entry);
            if ls.global.verbose {
                let stat = xhfs
                    .stats(&full_path, self.recursive)
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
                    INodeKind::Hardlink => {
                        let size = stat.size.unwrap_or(0);
                        ("HARDLINK", bytesize::ByteSize(size as u64).to_string())
                    }
                };

                println!(
                    "{:<8} {:<20} {:>10} {}",
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
        let xhfs = ls.global.get_xhfs().await?;
        self.print_tree(&xhfs, &self.path, "").await?;
        Ok(())
    }

    #[async_recursion(?Send)]
    async fn print_tree(&self, xhfs: &XHFS, path: &Path, prefix: &str) -> eyre::Result<()> {
        let entries = xhfs.ls(path).await?;
        let count = entries.len();
        for (i, entry) in entries.into_iter().enumerate() {
            let is_last = i == count - 1;
            let connector = if is_last { "└── " } else { "├── " };
            let full_path = path.join(&entry);
            let stat = xhfs.stats(&full_path, self.recursive).await?;

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
                    self.print_tree(xhfs, &full_path, &new_prefix).await?;
                }
            }
        }
        Ok(())
    }
}

impl CopyCommand {
    pub async fn run(&self) -> eyre::Result<()> {
        let xhfs = self.global.get_xhfs().await?;
        xhfs.fcopy(
            &self.src,
            &self.dest,
            WriteOption {
                overwrite: self.overwrite,
            },
        )
        .await?;
        Ok(())
    }
}

impl MoveCommand {
    pub async fn run(&self) -> eyre::Result<()> {
        let xhfs = self.global.get_xhfs().await?;
        xhfs.fmove(&self.src, &self.dest).await?;
        Ok(())
    }
}

impl PathCommand {
    pub async fn mkdir(&self) -> eyre::Result<()> {
        let xhfs = self.global.get_xhfs().await?;
        xhfs.mkdir(&self.path, self.recursive).await?;
        Ok(())
    }

    pub async fn rm(&self) -> eyre::Result<()> {
        let xhfs = self.global.get_xhfs().await?;
        if self.recursive {
            eyre::bail!("Recursive option not supported yet for unlink");
        }
        xhfs.unlink(&self.path).await?;
        Ok(())
    }

    pub async fn stats(&self) -> eyre::Result<()> {
        let xhfs = self.global.get_xhfs().await?;
        let stat = xhfs.stats(&self.path, self.recursive).await?;
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
                INodeKind::Hardlink => {
                    let size = stat.size.unwrap_or(0);
                    ("HARDLINK", bytesize::ByteSize(size as u64).to_string())
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
        } else {
            println!("Path {} does not exist", normalize_path(&self.path));
        }
        Ok(())
    }
}

impl LinkCommand {
    pub async fn run(&self) -> eyre::Result<()> {
        let xhfs = self.global.get_xhfs().await?;
        if self.symlink {
            xhfs.create_symlink(
                &self.dest_path,
                &self.src_path,
                WriteOption {
                    overwrite: self.overwrite,
                },
            )
            .await?;
        } else {
            xhfs.create_hardlink(
                &self.dest_path,
                &self.src_path,
                WriteOption {
                    overwrite: self.overwrite,
                },
            )
            .await?;
        }
        Ok(())
    }
}
