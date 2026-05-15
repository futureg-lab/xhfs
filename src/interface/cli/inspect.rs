use crate::{addr::MaybeU64, interface::cli::GlobalOptions};
use clap::{Args, Subcommand};
use std::path::PathBuf;
use tokio::fs;

#[derive(Args, Debug)]
pub struct InspectCommands {
    #[command(subcommand)]
    pub command: InspectSubcommands,
}

#[derive(Subcommand, Debug)]
pub enum InspectSubcommands {
    /// Inspect inode by its path
    Inode(InodeInspect),
    /// Inspect extent block by its address
    Extent(ExtentInspect),
    /// Dump block by range
    Dump(DumpBlock),
    /// Display a view of a block by range
    View(ViewBlock),
}

#[derive(Args, Debug)]
pub struct ViewBlock {
    /// Start address
    #[arg(value_parser = parse_hex_or_decimal)]
    pub start_address: u64,
    /// End address
    #[arg(value_parser = parse_hex_or_decimal)]
    pub end_address: u64,
    /// Decrypt block
    #[arg(short, long, default_value = "false")]
    pub decrypt: bool,
    #[arg(short, long, default_value = "8")]
    pub columns: usize,
    #[command(flatten)]
    pub global: GlobalOptions,
}

#[derive(Args, Debug)]
pub struct InodeInspect {
    /// Entry path
    #[arg(default_value = "/", value_parser = clap::value_parser!(PathBuf))]
    pub path: PathBuf,
    #[command(flatten)]
    pub global: GlobalOptions,
}

#[derive(Args, Debug)]
pub struct DumpBlock {
    /// Start address
    #[arg(value_parser = parse_hex_or_decimal)]
    pub start_address: u64,
    /// End address
    #[arg(value_parser = parse_hex_or_decimal)]
    pub end_address: u64,
    /// Local destination path
    pub dest_path: PathBuf,
    /// Decrypt block
    #[arg(short, long, default_value = "false")]
    pub decrypt: bool,
    #[arg(short, long, default_value = "false")]
    pub overwrite: bool,
    #[command(flatten)]
    pub global: GlobalOptions,
}

#[derive(Args, Debug)]
pub struct ExtentInspect {
    /// Extent address
    #[arg(value_parser = parse_hex_or_decimal)]
    pub address: u64,
    /// Maximum extent to resolve
    #[arg(long, short)]
    pub max_follow: u32,
    #[command(flatten)]
    pub global: GlobalOptions,
}

impl InspectSubcommands {
    pub async fn run(&self) -> eyre::Result<()> {
        match self {
            InspectSubcommands::Inode(i) => {
                let bfs = i.global.get_bfs().await?;
                let (addr, inode) = bfs.resolve_path(&i.path).await?;
                println!("Offset {addr} (0x{addr:08x})\n");
                println!("{inode}");
            }
            InspectSubcommands::Extent(e) => {
                let bfs = e.global.get_bfs().await?;
                let meta_exts = bfs
                    .find_full_extent_metadata(MaybeU64::from(e.address), Some(e.max_follow))
                    .await?;
                for (i, ext) in meta_exts.iter().enumerate() {
                    println!("#{} :: {}", i + 1, ext);
                }
            }
            InspectSubcommands::Dump(d) => {
                let bfs = d.global.get_bfs().await?;
                let size = d.end_address.saturating_sub(d.start_address);
                let blob = if d.decrypt {
                    bfs.ctrl
                        .read(d.start_address as usize, size as usize)
                        .await?
                } else {
                    bfs.ctrl
                        .raw_read(d.start_address as usize, size as usize)
                        .await?
                };
                if d.dest_path.exists() && !d.overwrite {
                    eyre::bail!("File {} already exists", d.dest_path.display());
                }
                fs::write(&d.dest_path, blob).await?;
            }
            InspectSubcommands::View(v) => {
                let bfs = v.global.get_bfs().await?;
                let size = v.end_address.saturating_sub(v.start_address);
                let blob = if v.decrypt {
                    bfs.ctrl
                        .read(v.start_address as usize, size as usize)
                        .await?
                } else {
                    bfs.ctrl
                        .raw_read(v.start_address as usize, size as usize)
                        .await?
                };
                hex_view(&blob, v.columns)?;
            }
        }
        Ok(())
    }
}

fn parse_hex_or_decimal(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if let Some(hex_str) = s.strip_prefix("0x") {
        u64::from_str_radix(hex_str, 16).map_err(|e| format!("Invalid hex: {e}"))
    } else if let Some(hex_str) = s.strip_prefix("0X") {
        u64::from_str_radix(hex_str, 16).map_err(|e| format!("Invalid hex: {e}"))
    } else {
        s.parse::<u64>().map_err(|e| format!("Invalid number: {e}"))
    }
}

pub fn hex_view(data: &[u8], cols: usize) -> eyre::Result<()> {
    if cols == 0 {
        eyre::bail!("number of columns must be > 0");
    }

    for (row_idx, chunk) in data.chunks(cols).enumerate() {
        let offset = row_idx * cols;
        print!("{:08x}: ", offset);
        let hex_part: String = chunk.iter().map(|b| format!("{:02x} ", b)).collect();
        let padding = cols.saturating_sub(chunk.len()) * 3;
        print!("{hex_part}{:width$}| ", "", width = padding);
        let ascii: String = chunk
            .iter()
            .map(|b| {
                let c = *b as char;
                if c.is_ascii_graphic() || c == ' ' {
                    c
                } else {
                    '.'
                }
            })
            .collect();
        println!("{ascii}");
    }

    Ok(())
}
