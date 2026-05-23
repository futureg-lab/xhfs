use crate::interface::cli::GlobalOptions;
use clap::{Args, Subcommand};
use eyre::OptionExt;
use std::path::PathBuf;
use tokio::fs;
use xhfs_core::xhfs::{
    addr::MaybeU64,
    ds::{Bitmap, GroupLayout},
};

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
    /// Display layout and used space
    Map(ViewMap),
}

#[derive(Args, Debug)]
pub struct ViewMap {
    /// Start address
    #[arg(value_parser = parse_hex_or_decimal, default_value = "1")]
    pub start_group: u64,
    /// End address
    #[arg(value_parser = parse_hex_or_decimal, default_value = "6")]
    pub end_group: u64,
    #[arg(short, long, default_value = "64")]
    pub columns: usize,
    #[command(flatten)]
    pub global: GlobalOptions,
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
                let xhfs = i.global.get_xhfs().await?;
                let inode = xhfs.resolve_path(&i.path).await?;
                let group = GroupLayout::derive_from_inode(inode.inumber, &xhfs.geometry)
                    .ok_or_eyre("Failed calculating group layout for INode")?;
                println!(
                    "Member of group #{} (idx {})",
                    group.g_index + 1,
                    group.g_index
                );
                println!("{inode}");
            }
            InspectSubcommands::Extent(e) => {
                let xhfs = e.global.get_xhfs().await?;
                let meta_exts = xhfs
                    .find_full_extent_metadata(MaybeU64::from(e.address), Some(e.max_follow))
                    .await?;
                for (i, ext) in meta_exts.iter().enumerate() {
                    println!("#{} :: (unaligned) {} ", i + 1, ext.full_canon_region);
                }
            }
            InspectSubcommands::Dump(d) => {
                let xhfs = d.global.get_xhfs().await?;
                let size = d.end_address.saturating_sub(d.start_address);
                if d.dest_path.exists() && !d.overwrite {
                    eyre::bail!("File {} already exists", d.dest_path.display());
                }
                let blob = if d.decrypt {
                    xhfs.ctrl
                        .read(d.start_address as usize, size as usize)
                        .await?
                } else {
                    xhfs.ctrl
                        .raw_read(d.start_address as usize, size as usize)
                        .await?
                };
                fs::write(&d.dest_path, blob).await?;
            }
            InspectSubcommands::View(v) => {
                let xhfs = v.global.get_xhfs().await?;
                let size = v.end_address.saturating_sub(v.start_address);
                let blob = if v.decrypt {
                    xhfs.ctrl
                        .read(v.start_address as usize, size as usize)
                        .await?
                } else {
                    xhfs.ctrl
                        .raw_read(v.start_address as usize, size as usize)
                        .await?
                };
                hex_view(&blob, v.columns)?;
            }
            InspectSubcommands::Map(v) => {
                let xhfs = v.global.get_xhfs().await?;
                let header = xhfs.get_header().await?;
                let (g, _) = header.calculate_relative_geometry()?;
                let start_idx = v.start_group.saturating_sub(1).max(0);
                let end_idx = v
                    .end_group
                    .saturating_sub(1)
                    .min(header.format.group_count - 1);
                let mut offset = 0;
                for gc in start_idx..=end_idx {
                    println!("Group #{} (idx {}) at 0x{:08x}", gc + 1, gc, offset);

                    println!(
                        "INode Table Region:  {}",
                        g.rel_inode_table_region.add_offset(offset)
                    );
                    let inode_bitmap = {
                        let region = g.rel_inode_bitmap_region.add_offset(offset);
                        let slot = region.to_addr_slot();
                        let data = xhfs
                            .ctrl
                            .raw_read(slot.addr.into(), slot.capacity as usize)
                            .await?;
                        Bitmap::deserialize(&data)?
                    };
                    print_bitmap(&inode_bitmap, v.columns, v.global.verbose);

                    println!("Data Region:  {}", g.rel_data_region.add_offset(offset));
                    let data_bitmap = {
                        let region = g.rel_data_bitmap_region.add_offset(offset);
                        let slot = region.to_addr_slot();
                        let data = xhfs
                            .ctrl
                            .raw_read(slot.addr.into(), slot.capacity as usize)
                            .await?;
                        Bitmap::deserialize(&data)?
                    };
                    print_bitmap(&data_bitmap, v.columns, v.global.verbose);

                    offset += g.group_stride;
                    println!();
                }
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

pub fn print_bitmap(bitmap: &Bitmap, columns: usize, verbose: bool) {
    let total = bitmap.map.len() as f64;
    let used = bitmap.runs_of(true, None).iter().fold(0, |a, x| a + x.size) as f64;
    let perc = 100.0 * used / total.max(0.0001);
    println!(" Used {perc:.2} %, {used}/{total}");

    if verbose {
        let cols = columns.max(1);
        let bits_per_slot = (total / cols as f64).max(1.0);
        print!(" ");
        for slot in 0..cols {
            let start_idx = (slot as f64 * bits_per_slot).round() as usize;
            let end_idx =
                (((slot + 1) as f64 * bits_per_slot).round() as usize).min(bitmap.map.len());

            if start_idx >= end_idx {
                print!(" ");
                continue;
            }
            let slice = &bitmap.map[start_idx..end_idx];
            let active_count = slice.iter().filter(|b| **b).count();
            let density = active_count as f64 / slice.len() as f64;
            if density <= 0.1 {
                print!("░");
            } else if density <= 0.4 {
                print!("▒");
            } else if density < 7.0 {
                print!("▓");
            } else {
                print!("█");
            }
        }
        println!()
    }
}
