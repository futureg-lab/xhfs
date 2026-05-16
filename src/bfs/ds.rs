use crate::bfs::addr::MaybeU64;
use crate::utils::{normalize_path, u64_to_utc_datetime};
use eyre::Context;
use std::fmt::Display;
use std::{fmt::Debug, path::PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum INodeKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct INode {
    pub kind: INodeKind,
    pub index: u64,
    pub nlink: u64,
    pub total_file_size: u64,
    pub extent_addr: MaybeU64,
    pub mtime: u64,
    pub ctime: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryStat {
    pub name: String,
    pub kind: INodeKind,
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
pub struct SymLink {
    pub path: PathBuf,
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
pub enum BruteFsError {
    Insufficient {
        wanted: usize,
        max_slot_size: usize,
        left_contiguous: usize,
    },
    Error {
        err: String,
    },
}

impl Display for BruteFsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BruteFsError::Insufficient {
                wanted,
                max_slot_size,
                left_contiguous,
            } => {
                write!(
                    f,
                    "Insufficient space, operation requires {wanted} B, max fragment available {max_slot_size} B, max contiguous bloc {left_contiguous}"
                )
            }
            BruteFsError::Error { err } => write!(f, "{err}"),
        }
    }
}

impl BruteFsError {
    pub fn from_error(error: impl std::error::Error) -> BruteFsError {
        BruteFsError::Error {
            err: error.to_string(),
        }
    }

    pub fn from_report(error: eyre::Report) -> BruteFsError {
        BruteFsError::Error {
            err: error.to_string(),
        }
    }
}

impl std::error::Error for BruteFsError {}

impl From<eyre::Report> for BruteFsError {
    fn from(value: eyre::Report) -> Self {
        BruteFsError::Error {
            err: value.to_string(),
        }
    }
}

impl From<Box<dyn std::error::Error + Send + Sync>> for BruteFsError {
    fn from(value: Box<dyn std::error::Error + Send + Sync>) -> Self {
        BruteFsError::Error {
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
            "  - 0x{:08x} -- 0x{:08x} ({:>10} B)\n",
            self.start.get(),
            self.end.get(),
            self.end.get().saturating_sub(self.start.get())
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressVector {
    pub global_offset: u64,
    pub items: Vec<AddressSlot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BruteFsHeader {
    pub version: u8,
    pub chacha20_nonce: [u8; 12],
}

impl INodeKind {
    pub fn to_byte(&self) -> u8 {
        match self {
            INodeKind::File => 0,
            INodeKind::Directory => 1,
            INodeKind::Symlink => 2,
        }
    }

    pub fn from_byte(value: u8) -> eyre::Result<Self> {
        Ok(match value {
            0 => Self::File,
            1 => Self::Directory,
            2 => Self::Symlink,
            _ => eyre::bail!("INodeKind of type {value} not understood"),
        })
    }

    pub fn serialized_size() -> usize {
        1
    }
}

impl Extent {
    pub fn serialize(&self) -> eyre::Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(self.serialized_size());
        buf.extend_from_slice(&(self.data.len() as u64).to_le_bytes());
        buf.extend_from_slice(&self.next.serialize()?);
        buf.extend(&self.data);

        Ok(buf)
    }

    pub fn deserialize(data: &[u8]) -> eyre::Result<Self> {
        let meta_expected_size = 8 + 8;
        let incoming_size = data.len();
        if incoming_size < meta_expected_size {
            eyre::bail!("Expected Extent data to be at least 8 + 8 (16) bytes");
        }

        let mut addr_start = 0;
        let size = u64::from_le_bytes(data[addr_start..addr_start + 8].try_into()?);
        addr_start += 8;

        let next = MaybeU64::deserialize(data[addr_start..addr_start + 8].try_into()?);
        addr_start += 8;

        let data = &data[addr_start..];
        if size != data.len() as u64 {
            eyre::bail!(
                "Expected Extent data region to be of size {}, got {} instead",
                size,
                data.len()
            );
        }

        Ok(Extent {
            next,
            data: data.to_vec(),
        })
    }

    pub fn serialized_size(&self) -> usize {
        8 + 8 + self.data.len()
    }
}

impl INode {
    pub fn serialize(&self) -> eyre::Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(Self::serialized_size());

        buf.extend_from_slice(&self.index.to_le_bytes());
        let kind = self.kind.to_byte();
        buf.push(kind);

        buf.extend_from_slice(&self.nlink.to_le_bytes());
        buf.extend_from_slice(&self.total_file_size.to_le_bytes());
        buf.extend_from_slice(&self.extent_addr.serialize()?);

        buf.extend_from_slice(&self.mtime.to_le_bytes());
        buf.extend_from_slice(&self.ctime.to_le_bytes());

        Ok(buf)
    }

    pub fn deserialize(data: &[u8]) -> eyre::Result<Self> {
        let expected_size = Self::serialized_size();
        let incoming_size = data.len();
        if incoming_size != expected_size {
            eyre::bail!(
                "Expected INode data size to be {expected_size}, got {incoming_size} instead"
            );
        }

        let mut addr_start = 0;
        let index = u64::from_le_bytes(data[addr_start..addr_start + 8].try_into()?);
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

        Ok(INode {
            index,
            kind,
            nlink,
            extent_addr,
            total_file_size,
            mtime,
            ctime,
        })
    }

    pub fn serialized_size() -> usize {
        8 + 1 + 8 + 8 + 8 + 8 + 8
    }
}

impl Display for INode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "- Kind: {:?}\n", self.kind)?;
        if !matches!(self.kind, INodeKind::Directory) {
            write!(f, "- Size: {} B\n", self.total_file_size)?;
        }
        write!(f, "- Creation time: {}\n", u64_to_utc_datetime(self.ctime))?;
        write!(
            f,
            "- Modification time: {}\n",
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
        if incoming_size < expected_size {
            eyre::bail!(
                "Expected Directory data size to be at least {expected_size}, got {incoming_size} instead"
            );
        }

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

impl SymLink {
    pub fn serialize(&self) -> Vec<u8> {
        normalize_path(self.path.clone()).as_bytes().to_vec()
    }

    pub fn deserialize(data: &[u8]) -> eyre::Result<Self> {
        let path = String::from_utf8(data.try_into()?).wrap_err_with(|| eyre::eyre!("dsads"))?;
        Ok(Self {
            path: PathBuf::from(path),
        })
    }
}

impl BruteFsHeader {
    pub fn serialize(&self) -> eyre::Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(self.serialized_size());
        buf.extend_from_slice(b"brutefs");
        buf.push(self.version);
        buf.extend_from_slice(&self.chacha20_nonce);
        Ok(buf)
    }

    pub fn deserialize(data: &[u8]) -> eyre::Result<Self> {
        let magic = b"brutefs";
        let min_expected_size = 7 + 1 + 12;
        let incoming_size = data.len();
        if incoming_size < min_expected_size {
            eyre::bail!(
                "Expected BruteFsHeader data size to be at least {min_expected_size}, got {incoming_size} instead"
            );
        }
        if &data[0..7] != magic {
            eyre::bail!("Invalid BruteFsHeader magic bytes");
        }

        Ok(Self {
            version: data[7],
            chacha20_nonce: data[8..8 + 12].try_into()?,
        })
    }

    pub fn serialized_size(&self) -> usize {
        7 + 1 + 12
    }
}
