use crate::utils::{normalize_path, u64_to_utc_datetime};
use crate::xhfs::addr::MaybeU64;
use bitvec::order::Msb0;
use bitvec::vec::BitVec;
use eyre::Context;
use std::fmt::Display;
use std::{fmt::Debug, path::PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum INodeKind {
    File,
    Directory,
    Symlink,
    Hardlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct INode {
    pub kind: INodeKind,
    pub inumber: u64,
    pub nlink: u64,
    pub total_file_size: u64,
    pub extent_addr: MaybeU64,
    pub mtime: u64,
    pub ctime: u64,
    pub extra_metadata: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    pub map: BitVec<u8, Msb0>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitRunSlot {
    pub start: usize,
    pub size: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryStat {
    pub name: String,
    pub kind: INodeKind,
    pub nlink: u64,
    pub size: Option<usize>,
    pub mtime: u64,
    pub ctime: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extent {
    /// 0 coerces to None
    pub next: MaybeU64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directory {
    pub entries: Vec<(String, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symlink {
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hardlink {
    pub inumber: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AddressSlot {
    pub addr: MaybeU64,
    pub capacity: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RegionSlot {
    pub start: MaybeU64,
    pub end: MaybeU64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XHFSError {
    Insufficient { operation: String, wanted: usize },
    Error { err: String },
}

impl Display for XHFSError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            XHFSError::Insufficient { operation, wanted } => {
                write!(f, "Insufficient space, {operation} requires {wanted} B")
            }
            XHFSError::Error { err } => write!(f, "{err}"),
        }
    }
}

impl XHFSError {
    pub fn from_error(error: impl std::error::Error) -> XHFSError {
        XHFSError::Error {
            err: error.to_string(),
        }
    }

    pub fn from_report(error: eyre::Report) -> XHFSError {
        XHFSError::Error {
            err: error.to_string(),
        }
    }
}

impl std::error::Error for XHFSError {}

impl From<eyre::Report> for XHFSError {
    fn from(value: eyre::Report) -> Self {
        XHFSError::Error {
            err: value.to_string(),
        }
    }
}

impl From<Box<dyn std::error::Error + Send + Sync>> for XHFSError {
    fn from(value: Box<dyn std::error::Error + Send + Sync>) -> Self {
        XHFSError::Error {
            err: value.to_string(),
        }
    }
}

impl AddressSlot {
    pub fn is_free(&self) -> bool {
        self.addr.is_none()
    }
}

impl Display for AddressSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            RegionSlot {
                start: self.addr,
                end: MaybeU64::from(self.addr.get() + self.capacity as u64)
            }
        )
    }
}

impl Display for RegionSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "0x{:08x} -- 0x{:08x} ({:>10} B)",
            self.start.get(),
            self.end.get(),
            self.to_addr_slot().capacity
        )
    }
}

impl RegionSlot {
    pub fn add_offset(&self, offset: u64) -> Self {
        Self {
            start: MaybeU64::from(offset + self.start.get()),
            end: MaybeU64::from(offset + self.end.get()),
        }
    }

    pub fn to_pair(&self) -> (u64, u64) {
        (self.start.get(), self.end.get())
    }

    pub fn size_span(&self) -> u64 {
        self.end.get().saturating_sub(self.start.get())
    }

    pub fn to_addr_slot(&self) -> AddressSlot {
        AddressSlot {
            addr: self.start,
            capacity: 1 + self.size_span() as usize, // like index 0 arrays
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressVector {
    pub global_offset: u64,
    pub items: Vec<AddressSlot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XHFSHeader {
    pub version: u8,
    pub format: Format,
    pub chacha20_nonce: [u8; 12],
    pub extra_metadata: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Format {
    pub param_data_block_count_per_group: u64,
    pub param_inode_count_per_group: u64,
    pub block_size_bytes: u64,
    pub group_count: u64,
}

#[derive(Debug, Clone, Default)]
pub struct GeometryLayout {
    pub rel_header_region: RegionSlot,
    pub rel_data_bitmap_region: RegionSlot,
    pub rel_inode_bitmap_region: RegionSlot,
    pub rel_inode_table_region: RegionSlot,
    pub rel_data_region: RegionSlot,
    pub n_inodes_in_group: u64,
    pub group_stride: u64,
    pub usable_blocks_per_group: u64,
}

#[derive(Debug, Clone, Default)]
pub struct BlockInitialValues {
    pub serialized_header: Vec<u8>,
    pub inode_bitmap_placeholder: Vec<u8>,
    pub data_block_bitmap: Vec<u8>,
    pub inode_table_placeholder: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct GroupLayout {
    pub g_index: u64,
    pub g_offset: u64,
    pub header_region: RegionSlot,
    pub data_bitmap_region: RegionSlot,
    pub inode_bitmap_region: RegionSlot,
    pub inode_table_region: RegionSlot,
    pub data_region: RegionSlot,
}

impl INodeKind {
    pub fn to_byte(&self) -> u8 {
        match self {
            INodeKind::File => 0,
            INodeKind::Directory => 1,
            INodeKind::Symlink => 2,
            INodeKind::Hardlink => 3,
        }
    }

    pub fn from_byte(value: u8) -> eyre::Result<Self> {
        Ok(match value {
            0 => Self::File,
            1 => Self::Directory,
            2 => Self::Symlink,
            3 => Self::Hardlink,
            _ => eyre::bail!("INodeKind of type {value} not understood"),
        })
    }

    pub fn serialized_size() -> usize {
        1
    }
}

impl Extent {
    pub const HEADER_NEXT_OFFSET: u64 = 8;

    pub fn serialize(&self) -> eyre::Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(self.serialized_size());
        buf.extend_from_slice(&(self.data.len() as u64).to_le_bytes());
        buf.extend_from_slice(&self.next.serialize()?);
        buf.extend(&self.data);

        Ok(buf)
    }

    pub fn deserialize(data: &[u8]) -> eyre::Result<Self> {
        let (size, next) = Self::deserialize_header_only(data)?;
        let data = &data[8 + 8..];
        eyre::ensure!(
            size == data.len() as u64,
            "Expected Extent data region to be of size {}, got {} instead",
            size,
            data.len()
        );

        Ok(Extent {
            next,
            data: data.to_vec(),
        })
    }

    pub fn deserialize_header_only(data: &[u8]) -> eyre::Result<(u64, MaybeU64)> {
        let meta_expected_size = 8 + 8;
        let incoming_size = data.len();
        eyre::ensure!(
            incoming_size >= meta_expected_size,
            "Expected Extent data to be at least 8 + 8 (16) bytes"
        );

        let mut addr_start = 0;
        let size = u64::from_le_bytes(data[addr_start..addr_start + 8].try_into()?);
        addr_start += 8;
        let next = MaybeU64::deserialize(data[addr_start..addr_start + 8].try_into()?);

        Ok((size, next))
    }

    pub fn emulate_serialized_size(data_len: usize) -> usize {
        8 + 8 + data_len
    }

    pub fn serialized_size(&self) -> usize {
        Self::emulate_serialized_size(self.data.len())
    }
}

impl INode {
    pub fn serialize(&self) -> eyre::Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(Self::serialized_size());

        buf.extend_from_slice(&self.inumber.to_le_bytes());
        let kind = self.kind.to_byte();
        buf.push(kind);

        buf.extend_from_slice(&self.nlink.to_le_bytes());
        buf.extend_from_slice(&self.total_file_size.to_le_bytes());
        buf.extend_from_slice(&self.extent_addr.serialize()?);

        buf.extend_from_slice(&self.mtime.to_le_bytes());
        buf.extend_from_slice(&self.ctime.to_le_bytes());
        buf.extend_from_slice(&self.extra_metadata);

        Ok(buf)
    }

    pub fn deserialize(data: &[u8]) -> eyre::Result<Self> {
        let expected_size = Self::serialized_size();
        let incoming_size = data.len();
        eyre::ensure!(
            incoming_size == expected_size,
            "Expected INode data size to be {expected_size}, got {incoming_size} instead"
        );

        let mut addr_start = 0;
        let inumber = u64::from_le_bytes(data[addr_start..addr_start + 8].try_into()?);
        addr_start += 8;
        let kind = INodeKind::from_byte(data[8])?;
        addr_start += 1;

        let nlink = u64::from_le_bytes(data[addr_start..addr_start + 8].try_into()?);
        addr_start += 8;

        let total_file_size = u64::from_le_bytes(data[addr_start..addr_start + 8].try_into()?);
        addr_start += 8;
        let extent_addr = MaybeU64::deserialize(data[addr_start..addr_start + 8].try_into()?);
        addr_start += 8;

        let mtime = u64::from_le_bytes(data[addr_start..addr_start + 8].try_into()?);
        addr_start += 8;
        let ctime = u64::from_le_bytes(data[addr_start..addr_start + 8].try_into()?);
        addr_start += 8;

        let extra_metadata: [u8; 32] = data[addr_start..addr_start + 32].try_into()?;

        Ok(INode {
            inumber,
            kind,
            nlink,
            extent_addr,
            total_file_size,
            mtime,
            ctime,
            extra_metadata,
        })
    }

    pub fn serialized_size() -> usize {
        8 + 1 + 8 + 8 + 8 + 8 + 8 + 32
    }
}

impl Display for INode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "- Number of Links: {}", self.nlink)?;
        writeln!(f, "- Kind: {:?}", self.kind)?;
        if !matches!(self.kind, INodeKind::Directory) {
            writeln!(f, "- Size: {} B", self.total_file_size)?;
        }
        writeln!(f, "- Creation time: {}", u64_to_utc_datetime(self.ctime))?;
        writeln!(
            f,
            "- Modification time: {}",
            u64_to_utc_datetime(self.mtime)
        )?;
        write!(
            f,
            "- Immediate Extent address: {} (0x{:08x})\n",
            self.extent_addr.get(),
            self.extent_addr.get()
        )
    }
}

fn serialize_255_utf8_string(s: &str) -> eyre::Result<Vec<u8>> {
    let str_bytes = s.as_bytes();
    if str_bytes.len() > 255 {
        eyre::bail!("Entry cannot exceed 255 bytes");
    }
    if str_bytes
        .iter()
        .any(|b| matches!(b, b'\n' | b'\r' | b'\t' | 0x00))
    {
        eyre::bail!("Entry name contains invalid control characters");
    }

    let mut bytes = [0u8; 256];
    bytes[0] = str_bytes.len() as u8;
    bytes[1..1 + str_bytes.len()].copy_from_slice(str_bytes);

    Ok(bytes.to_vec())
}

fn deserialize_255_utf8_string(data: &[u8]) -> eyre::Result<String> {
    if data.len() != 256 {
        eyre::bail!("Expected exactly 256 bytes, got {}", data.len());
    }
    let length = data[0] as usize;
    if length > 255 {
        eyre::bail!("Invalid stored string length");
    }
    let out = std::str::from_utf8(&data[1..1 + length])
        .wrap_err("Received data has invalid UTF-8 string")?
        .to_string();

    Ok(out)
}

impl Directory {
    pub fn serialize(&self) -> eyre::Result<Vec<u8>> {
        let items = self.entries.len();
        let mut data = Vec::with_capacity(8 + items * (255 + 8));
        data.extend(items.to_le_bytes());
        for (k, v) in &self.entries {
            data.extend(serialize_255_utf8_string(k)?);
            data.extend(v.to_le_bytes());
        }
        Ok(data)
    }

    pub fn deserialize(data: &[u8]) -> eyre::Result<Self> {
        if data.is_empty() {
            return Ok(Directory { entries: vec![] });
        }

        let expected_size = 8;
        let incoming_size = data.len();
        eyre::ensure!(
            incoming_size >= expected_size,
            "Expected Directory data size to be at least {expected_size}, got {incoming_size} instead"
        );

        let mut addr_start = 0;
        let items = u64::from_le_bytes(data[addr_start..addr_start + 8].try_into()?);
        addr_start += 8;

        let mut entries = vec![];
        for _ in 0..items {
            let key = deserialize_255_utf8_string(&data[addr_start..addr_start + 256])?;
            addr_start += 256;
            let inode_addr = u64::from_le_bytes(data[addr_start..addr_start + 8].try_into()?);
            addr_start += 8;

            entries.push((key, inode_addr));
        }

        Ok(Directory { entries })
    }
}

impl Symlink {
    pub fn serialize(&self) -> Vec<u8> {
        normalize_path(self.path.clone()).as_bytes().to_vec()
    }

    pub fn deserialize(data: &[u8]) -> eyre::Result<Self> {
        let path = String::from_utf8(data.try_into()?)
            .wrap_err_with(|| eyre::eyre!("Parsing Symlink path"))?;
        Ok(Self {
            path: PathBuf::from(path),
        })
    }
}

impl Hardlink {
    pub fn serialize(&self) -> Vec<u8> {
        self.inumber.to_le_bytes().into_iter().collect()
    }

    pub fn deserialize(data: &[u8]) -> eyre::Result<Self> {
        let inumber = u64::from_le_bytes(data.try_into()?);
        Ok(Self { inumber })
    }
}

impl XHFSHeader {
    pub fn serialize(&self) -> eyre::Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(Self::serialized_size());
        buf.extend_from_slice(b"XHFS");
        buf.push(self.version);
        buf.extend_from_slice(&self.format.param_data_block_count_per_group.to_le_bytes());
        buf.extend_from_slice(&self.format.param_inode_count_per_group.to_le_bytes());
        buf.extend_from_slice(&self.format.block_size_bytes.to_le_bytes());
        buf.extend_from_slice(&self.format.group_count.to_le_bytes());
        buf.extend_from_slice(&self.chacha20_nonce);
        buf.extend_from_slice(&self.extra_metadata);

        Ok(buf)
    }

    pub fn deserialize(data: &[u8]) -> eyre::Result<Self> {
        let expected_size = Self::serialized_size();
        let incoming_size = data.len();
        eyre::ensure!(
            incoming_size == expected_size,
            "Expected XHFSHeader data size to be {expected_size}, got {incoming_size} instead"
        );
        eyre::ensure!(&data[0..4] == b"XHFS", "Invalid XHFSHeader magic bytes");

        let version = data[4];
        let mut addr_start = 5;
        let param_data_block_count_per_group =
            u64::from_le_bytes(data[addr_start..addr_start + 8].try_into()?);
        addr_start += 8;
        let param_inode_count_per_group =
            u64::from_le_bytes(data[addr_start..addr_start + 8].try_into()?);
        addr_start += 8;

        let block_size_bytes = u64::from_le_bytes(data[addr_start..addr_start + 8].try_into()?);
        addr_start += 8;
        let group_count = u64::from_le_bytes(data[addr_start..addr_start + 8].try_into()?);
        addr_start += 8;
        let chacha20_nonce = data[addr_start..addr_start + 12].try_into()?;
        addr_start += 12;

        let extra_metadata: [u8; 32] = data[addr_start..addr_start + 32].try_into()?;

        Ok(Self {
            version,
            chacha20_nonce,
            format: Format {
                param_data_block_count_per_group,
                param_inode_count_per_group,
                block_size_bytes,
                group_count,
            },
            extra_metadata,
        })
    }

    pub fn serialized_size() -> usize {
        4 + 1 + 8 + 8 + 8 + 8 + 12 + 32
    }

    pub fn template() -> Self {
        Self {
            version: 1,
            chacha20_nonce: Default::default(),
            extra_metadata: [0; 32],
            format: Format {
                param_data_block_count_per_group: 0,
                param_inode_count_per_group: 0,
                block_size_bytes: 0,
                group_count: 0,
            },
        }
    }

    pub fn calculate_relative_geometry(
        &self,
    ) -> eyre::Result<(GeometryLayout, BlockInitialValues)> {
        let serialized_header = self.serialize()?;
        let rel_header_region = RegionSlot {
            start: 0u64.into(),
            end: ((serialized_header.len() - 1) as u64).into(),
        };

        let data_block_bitmap =
            Bitmap::new_from_bits_count(self.format.param_data_block_count_per_group as usize)
                .serialize()?;
        let data_bitmap_start = rel_header_region.end.get() + 1;
        let rel_data_bitmap_region = RegionSlot {
            start: data_bitmap_start.into(),
            end: (data_bitmap_start + data_block_bitmap.len() as u64 - 1).into(),
        };

        let inode_bitmap_placeholder =
            Bitmap::new_from_bits_count(self.format.param_inode_count_per_group as usize)
                .serialize()?;
        let inode_bitmap_start = rel_data_bitmap_region.end.get() + 1;
        let rel_inode_bitmap_region = RegionSlot {
            start: inode_bitmap_start.into(),
            end: (inode_bitmap_start + inode_bitmap_placeholder.len() as u64 - 1).into(),
        };

        let inode_table_len_bytes =
            self.format.param_inode_count_per_group * INode::serialized_size() as u64;
        let inode_table_start = rel_inode_bitmap_region.end.get() + 1;
        let inode_table_placeholder = vec![0u8; inode_table_len_bytes as usize];
        let rel_inode_table_region = RegionSlot {
            start: inode_table_start.into(),
            end: (inode_table_start + inode_table_len_bytes - 1).into(),
        };

        let data_region_start = rel_inode_table_region.end.get() + 1;
        let exact_data_payload_bytes =
            self.format.param_data_block_count_per_group * self.format.block_size_bytes;
        let rel_data_region = RegionSlot {
            start: data_region_start.into(),
            end: (data_region_start + exact_data_payload_bytes - 1).into(),
        };

        let total_group_bytes = rel_data_region.end.get() + 1;
        let group_stride = total_group_bytes;
        let geometry = GeometryLayout {
            rel_header_region,
            rel_data_bitmap_region,
            rel_inode_bitmap_region,
            rel_inode_table_region,
            rel_data_region,
            group_stride,
            usable_blocks_per_group: self.format.param_data_block_count_per_group,
            n_inodes_in_group: self.format.param_inode_count_per_group,
        };

        let templates = BlockInitialValues {
            serialized_header,
            data_block_bitmap,
            inode_bitmap_placeholder,
            inode_table_placeholder,
        };

        Ok((geometry, templates))
    }
}

impl Bitmap {
    pub fn new_from_bits_count(size_in_bits: usize) -> Self {
        let mut map = BitVec::new();
        map.resize(size_in_bits, false);
        Self { map }
    }

    #[inline]
    pub fn set(&mut self, n: usize, value: bool) -> eyre::Result<()> {
        eyre::ensure!(
            n < self.map.len(),
            "Index {} out of bounds for bitmap of size {}",
            n,
            self.map.len()
        );
        self.map.set(n, value);
        Ok(())
    }

    #[inline]
    pub fn get(&self, n: usize) -> eyre::Result<bool> {
        eyre::ensure!(
            n < self.map.len(),
            "Index {} out of bounds for bitmap of size {}",
            n,
            self.map.len()
        );
        Ok(*self.map.get(n).unwrap())
    }

    pub fn runs_of(&self, target_bit: bool, stop_index: Option<usize>) -> Vec<BitRunSlot> {
        let mut runs = vec![];
        let logical_len = stop_index.unwrap_or(self.map.len()).min(self.map.len());
        if logical_len == 0 {
            return runs;
        }

        let actual = self.map[..logical_len].to_bitvec();
        let raw = actual.as_raw_slice();
        let full_bytes = logical_len / 8;
        let rem_bits = logical_len % 8;
        let mut current_run_start = None;
        let mut current_bit_index = 0;
        for (byte_index, &raw_word) in raw.iter().enumerate() {
            let mut word = raw_word;
            // mask away padding bits in the final partial byte
            if byte_index == full_bytes && rem_bits != 0 {
                let mask = 0xFF << (8 - rem_bits);
                word &= mask;
            }
            if !target_bit {
                word = !word;
                // remask after inversion so padding bits stay zero
                if byte_index == full_bytes && rem_bits != 0 {
                    let mask = 0xFF << (8 - rem_bits);
                    word &= mask;
                }
            }
            let valid_bits = if byte_index < full_bytes {
                8
            } else if rem_bits == 0 {
                8
            } else {
                rem_bits
            };

            if word == 0 {
                // match
                if let Some(start) = current_run_start.take() {
                    runs.push(BitRunSlot {
                        start,
                        size: current_bit_index - start,
                    });
                }
                current_bit_index += valid_bits;
                continue;
            }

            // full match
            let full_mask = if valid_bits == 8 {
                u8::MAX
            } else {
                0xFF << (8 - valid_bits)
            };

            if word == full_mask {
                if current_run_start.is_none() {
                    current_run_start = Some(current_bit_index);
                }

                current_bit_index += valid_bits;
                continue;
            }

            // mixed => scan transitions
            let mut processed = 0;
            while processed < valid_bits {
                let zeros = word.leading_zeros() as usize;
                if zeros > 0 {
                    if let Some(start) = current_run_start.take() {
                        runs.push(BitRunSlot {
                            start,
                            size: current_bit_index - start,
                        });
                    }
                    let shift = zeros.min(valid_bits - processed);
                    word <<= shift;
                    current_bit_index += shift;
                    processed += shift;
                }
                if processed >= valid_bits {
                    break;
                }
                let ones = word.leading_ones() as usize;
                if ones > 0 {
                    if current_run_start.is_none() {
                        current_run_start = Some(current_bit_index);
                    }
                    let shift = ones.min(valid_bits - processed);
                    word <<= shift;
                    current_bit_index += shift;
                    processed += shift;
                }
            }
        }

        if let Some(start) = current_run_start.take() {
            runs.push(BitRunSlot {
                start,
                size: current_bit_index - start,
            });
        }

        runs
    }

    pub fn set_range(&mut self, start: usize, length: usize, value: bool) -> eyre::Result<()> {
        let end = start + length;
        if end > self.map.len() {
            return Err(eyre::eyre!(
                "Out of bounds: range {}..{} exceeds bitmap capacity {}",
                start,
                end,
                self.map.len()
            ));
        }
        self.map[start..end].fill(value);
        Ok(())
    }

    pub fn serialize(&self) -> eyre::Result<Vec<u8>> {
        let bit_len = self.map.len() as u64;
        let raw_bytes = self.map.as_raw_slice();
        let mut data = Vec::with_capacity(8 + raw_bytes.len());
        data.extend_from_slice(&bit_len.to_le_bytes());
        data.extend_from_slice(raw_bytes);
        Ok(data)
    }

    pub fn deserialize(data: &[u8]) -> eyre::Result<Self> {
        eyre::ensure!(
            data.len() >= 8,
            "Bitmap buffer is too short to contain the header (min 8 bytes), got {}",
            data.len()
        );

        let bit_len = u64::from_le_bytes(data[0..8].try_into()?) as usize;
        let raw_bytes = &data[8..];
        let mut map: BitVec<u8, Msb0> = BitVec::from_slice(raw_bytes);

        eyre::ensure!(
            map.len() >= bit_len,
            "Bitmap expected {} bits, got {} instead",
            map.len(),
            bit_len
        );
        map.truncate(bit_len);

        Ok(Self { map })
    }

    pub fn find_next_zero_index(&self, start_index: usize) -> Option<usize> {
        if start_index >= self.map.len() {
            return None;
        }
        let remaining_bits = &self.map[start_index..];
        remaining_bits
            .iter_zeros()
            .next()
            .map(|relative_idx| start_index + relative_idx)
    }

    pub fn serialized_size(&self) -> usize {
        8 + self.map.as_raw_slice().len()
    }
}

impl Format {
    pub fn infer_from_free_space(
        total_capacity_bytes: u64,
        data_block_count: u64,
        inode_count: u64,
    ) -> eyre::Result<Self> {
        let block_size_bytes = if total_capacity_bytes <= 512 * 1024 {
            512
        } else if total_capacity_bytes <= 4 * 1024 * 1024 {
            1024
        } else {
            4096
        };

        let max_bits_per_block = block_size_bytes * 8;
        let data_block_count = data_block_count.min(max_bits_per_block).max(1);
        let inode_count = inode_count.min(max_bits_per_block).max(1);
        let header_bytes = XHFSHeader::serialized_size() as u64;
        let data_bitmap_bytes = ((data_block_count + 8 - 1) / 8).max(1);
        let inode_bitmap_bytes = ((inode_count + 8 - 1) / 8).max(1);
        let inode_table_bytes = inode_count * INode::serialized_size() as u64;
        let data_blocks_bytes = data_block_count * block_size_bytes;

        let total_bytes_per_group = header_bytes
            + data_bitmap_bytes
            + inode_bitmap_bytes
            + inode_table_bytes
            + data_blocks_bytes;
        let group_count = total_capacity_bytes / total_bytes_per_group;

        eyre::ensure!(
            total_capacity_bytes >= total_bytes_per_group,
            "Device capacity ({total_capacity_bytes} B) is too small to host a single block group configuration (requires {total_bytes_per_group} B)",
        );

        Ok(Self {
            param_data_block_count_per_group: data_block_count,
            param_inode_count_per_group: inode_count,
            block_size_bytes,
            group_count,
        })
    }

    pub fn validate(&self) -> eyre::Result<()> {
        eyre::ensure!(
            self.block_size_bytes > 0 && self.block_size_bytes.is_power_of_two(),
            "Block size ({}) must be greater than 0 and a power of two",
            self.block_size_bytes
        );
        eyre::ensure!(self.group_count > 0, "Group count must be > 0");
        eyre::ensure!(
            self.param_data_block_count_per_group > 0,
            "Data block count per group must be > 0"
        );
        eyre::ensure!(
            self.param_inode_count_per_group > 0,
            "INode count per group must be > 0"
        );

        let max_bits_per_block = self
            .block_size_bytes
            .checked_mul(8)
            .ok_or_else(|| eyre::eyre!("Block size multiplication overflowed"))?;

        eyre::ensure!(
            self.param_data_block_count_per_group <= max_bits_per_block,
            "Data block count ({}) exceeds the max capacity of a single block bitmap ({} bits) for block size {}",
            self.param_data_block_count_per_group,
            max_bits_per_block,
            self.block_size_bytes
        );

        eyre::ensure!(
            self.param_inode_count_per_group <= max_bits_per_block,
            "INode count ({}) exceeds the max capacity of a single block bitmap ({} bits) for block size {}",
            self.param_inode_count_per_group,
            max_bits_per_block,
            self.block_size_bytes
        );

        Ok(())
    }
}

impl GroupLayout {
    pub fn derive_from_group_index(g_index: u64, geometry: &GeometryLayout) -> Option<Self> {
        // prevent division by zero if geometry isn't initialized properly
        if geometry.group_stride == 0 {
            return None;
        }

        let g_offset = g_index * geometry.group_stride;
        Some(Self {
            g_index,
            g_offset,
            header_region: geometry.rel_header_region.add_offset(g_offset),
            data_bitmap_region: geometry.rel_data_bitmap_region.add_offset(g_offset),
            inode_bitmap_region: geometry.rel_inode_bitmap_region.add_offset(g_offset),
            inode_table_region: geometry.rel_inode_table_region.add_offset(g_offset),
            data_region: geometry.rel_data_region.add_offset(g_offset),
        })
    }

    pub fn derive_from_address(addr: u64, geometry: &GeometryLayout) -> Option<Self> {
        // prevent division by zero if geometry isn't initialized properly
        if geometry.group_stride == 0 {
            return None;
        }
        Self::derive_from_group_index(addr / geometry.group_stride, geometry)
    }

    pub fn derive_from_inode(inumber: u64, geometry: &GeometryLayout) -> Option<Self> {
        if inumber == 0 || geometry.n_inodes_in_group == 0 {
            return None;
        }
        Self::derive_from_group_index((inumber - 1) / geometry.n_inodes_in_group, geometry)
    }
}

impl Display for GeometryLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Geometry Layout (relative):")?;
        writeln!(f, "  Group Stride:        {} B", self.group_stride)?;
        writeln!(f, "  INodes per Group:    {}", self.n_inodes_in_group)?;
        writeln!(f, "  Usable Blocks/Group: {}", self.usable_blocks_per_group)?;
        writeln!(f, "  Header Region:       {}", self.rel_header_region)?;
        writeln!(f, "  Data Bitmap Region:  {}", self.rel_data_bitmap_region)?;
        writeln!(f, "  INode Bitmap Region: {}", self.rel_inode_bitmap_region)?;
        writeln!(f, "  INode Table Region:  {}", self.rel_inode_table_region)?;
        write!(f, "  Data Payload Region: {}", self.rel_data_region)
    }
}

impl Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Format Configuration:")?;
        writeln!(f, "  Block Size:       {} B", self.block_size_bytes)?;
        writeln!(
            f,
            "  Data Blocks per Group: {}",
            self.param_data_block_count_per_group
        )?;
        writeln!(
            f,
            "  INode count per Group: {}",
            self.param_inode_count_per_group
        )?;
        write!(f, "  Total Groups:     {}", self.group_count)
    }
}
