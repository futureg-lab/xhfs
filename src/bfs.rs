use crate::disk::Controller;
use eyre::Context;
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum INodeKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaybeU64 {
    Some(u64),
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct INode {
    pub kind: INodeKind,
    pub extent_addr: MaybeU64,
    pub mtime: u64,
    pub ctime: u64,
    pub utime: u64,
    // pub extra_meta: INodeExtraMetadata,
    // pub password: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AddressSlot {
    pub addr: MaybeU64,
    pub capacity: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressVector {
    pub global_offset: u64,
    pub items: Vec<AddressSlot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BruteFsHeader {
    pub version: u8,
    pub extent_freed: AddressVector,
}

pub struct BruteFS {
    header_size: usize,
    ctrl: Controller,
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

    pub fn serialized_size(&self) -> usize {
        1
    }
}

impl Default for MaybeU64 {
    fn default() -> Self {
        MaybeU64::None
    }
}

impl MaybeU64 {
    pub fn get(&self) -> u64 {
        match self {
            MaybeU64::Some(addr) => *addr,
            MaybeU64::None => 0,
        }
    }

    pub fn from(addr: u64) -> Self {
        match addr {
            0 => Self::None,
            _ => Self::Some(addr),
        }
    }

    pub fn serialize(&self) -> eyre::Result<Vec<u8>> {
        Ok(match self {
            MaybeU64::Some(addr) => {
                if *addr == 0 {
                    eyre::bail!("address cannot be both existing and be 0")
                }
                addr.to_le_bytes().to_vec()
            }
            MaybeU64::None => 0u64.to_le_bytes().to_vec(),
        })
    }

    pub fn deserialize(data: [u8; 8]) -> MaybeU64 {
        let addr = u64::from_le_bytes(data).try_into().unwrap();
        match addr {
            0 => MaybeU64::None,
            _ => MaybeU64::Some(addr),
        }
    }

    pub fn serialized_size(&self) -> usize {
        8
    }
}

impl Extent {
    pub fn serialize(&self) -> eyre::Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(self.serialized_size());
        buf.extend_from_slice(&self.data.len().to_le_bytes());
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

        let kind = self.kind.to_byte();
        buf.push(kind);

        buf.extend_from_slice(&self.extent_addr.serialize()?);

        buf.extend_from_slice(&self.mtime.to_le_bytes());
        buf.extend_from_slice(&self.ctime.to_le_bytes());
        buf.extend_from_slice(&self.utime.to_le_bytes());

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

        let kind = INodeKind::from_byte(data[0])?;

        let mut addr_start = 1;
        let extent_addr = MaybeU64::deserialize(data[addr_start..addr_start + 8].try_into()?);
        addr_start += 8;

        let mtime = u64::from_le_bytes(data[addr_start..addr_start + 8].try_into()?);
        addr_start += 8;
        let ctime = u64::from_le_bytes(data[addr_start..addr_start + 8].try_into()?);
        addr_start += 8;
        let utime = u64::from_le_bytes(data[addr_start..addr_start + 8].try_into()?);

        Ok(INode {
            kind,
            mtime,
            ctime,
            utime,
            extent_addr,
        })
    }

    pub fn serialized_size() -> usize {
        1 + 8 + 8 + 8 + 8
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
        // if data.is_empty() {
        //     return Ok(Directory { entries: vec![] });
        // }

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

impl AddressVector {
    pub fn allocate(count: usize) -> Self {
        AddressVector {
            global_offset: 0,
            items: vec![AddressSlot::default(); count],
        }
    }

    pub fn serialize(&self) -> eyre::Result<Vec<u8>> {
        let count = self.items.len();
        let mut buf = Vec::with_capacity(self.serialized_size());
        buf.extend_from_slice(&count.to_le_bytes());
        buf.extend_from_slice(&self.global_offset.to_le_bytes());
        for slot in &self.items {
            buf.extend_from_slice(&slot.addr.get().to_le_bytes());
            buf.extend_from_slice(&slot.capacity.to_le_bytes());
        }

        Ok(buf)
    }

    pub fn deserialize(data: &[u8]) -> eyre::Result<Self> {
        if data.len() < 8 {
            eyre::bail!("Expected AddressVector to be contain at least the the data count");
        }

        let mut addr_start = 0;
        let items = u64::from_le_bytes(data[addr_start..addr_start + 8].try_into()?);
        addr_start += 8;

        let global_offset = u64::from_le_bytes(data[addr_start..addr_start + 8].try_into()?);
        addr_start += 8;

        let u64_count = (data.len() - addr_start) / 8;
        if u64_count != 2 * items as usize {
            eyre::bail!("Expected a count of {items} address entries, got {u64_count} instead")
        }

        // TODO:
        // This is a hot path! find a better way if possible
        let slice = &data[addr_start..];
        if slice.len() % 16 != 0 {
            eyre::bail!("Invalid slice length (not multiple of 16)");
        }

        let mut items = Vec::with_capacity(slice.len() / 16);
        for chunk in slice.chunks_exact(16) {
            let addr_bytes: [u8; 8] = chunk[0..8].try_into().expect("valid size");
            let size_bytes: [u8; 8] = chunk[8..16].try_into().expect("valid size");
            let addr = MaybeU64::deserialize(addr_bytes);
            let capacity = u64::from_le_bytes(size_bytes) as usize;

            items.push(AddressSlot { addr, capacity });
        }

        Ok(Self {
            global_offset,
            items,
        })
    }

    pub fn serialized_size(&self) -> usize {
        1 * 8 + self.items.len() * (8 + 8)
    }
}

impl BruteFsHeader {
    pub fn serialize(&self) -> eyre::Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(self.serialized_size());
        buf.extend_from_slice(b"brutefs");
        buf.push(self.version);
        buf.extend_from_slice(&self.extent_freed.serialize()?);
        Ok(buf)
    }

    pub fn deserialize(data: &[u8]) -> eyre::Result<Self> {
        let magic = b"brutefs";
        let min_expected_size = 7 + 1;
        let incoming_size = data.len();
        if incoming_size < min_expected_size {
            eyre::bail!(
                "Expected BruteFsHeader data size to be at least {min_expected_size}, got {incoming_size} instead"
            );
        }
        if &data[0..7] != magic {
            eyre::bail!("Invalid BruteFsHeader magic bytes");
        }

        let version = data[7];

        Ok(Self {
            version,
            extent_freed: AddressVector::deserialize(&data[8..])?,
        })
    }

    pub fn serialized_size(&self) -> usize {
        7 + 1 + self.extent_freed.serialized_size()
    }
}

#[derive(Debug, Clone, Default)]
pub struct WriteOption {
    overwrite: bool,
}

impl BruteFS {
    pub fn new(ctrl: Controller) -> eyre::Result<Self> {
        Ok(Self {
            ctrl,
            header_size: Self::header_template().serialize()?.len(),
        })
    }

    fn header_template() -> BruteFsHeader {
        BruteFsHeader {
            version: 1,
            extent_freed: AddressVector::allocate(1000),
        }
    }

    pub async fn format(&mut self) -> eyre::Result<()> {
        let root = INode {
            ctime: utc_now_u64(),
            mtime: utc_now_u64(),
            utime: utc_now_u64(),
            extent_addr: MaybeU64::None,
            kind: INodeKind::Directory,
        };

        let root_raw = root.serialize()?;
        self.ctrl.write(self.header_size, &root_raw).await?;

        let mut header = Self::header_template();
        header.extent_freed.global_offset = (self.header_size + root_raw.len()) as u64;
        self.ctrl.write(0, &header.serialize()?).await?;

        Ok(())
    }

    pub async fn get_header(&self) -> eyre::Result<BruteFsHeader> {
        BruteFsHeader::deserialize(&self.ctrl.read(0, self.header_size).await?)
    }

    pub async fn get_root_inode(&self) -> eyre::Result<(u64, INode)> {
        let root_inode_addr = self.header_size;
        Ok((
            root_inode_addr as u64,
            INode::deserialize(
                &self
                    .ctrl
                    .read(root_inode_addr, INode::serialized_size())
                    .await?,
            )?,
        ))
    }

    pub async fn allocate(&self, wanted_size: usize) -> eyre::Result<u64> {
        let header_raw = self.ctrl.read(0, self.header_size).await?;
        let mut header = BruteFsHeader::deserialize(&header_raw)?;

        // TODO:
        // can be done faster with offset and online scan
        // (reusing the deserialize for loop code for AddressVector)

        // try reusing freed and potentially fragmented region first
        let mut addr_to_reuse = None;

        for slot in header.extent_freed.items.iter_mut() {
            if let MaybeU64::Some(free_addr) = slot.addr {
                if wanted_size <= slot.capacity {
                    addr_to_reuse = Some(free_addr);

                    let new_start = free_addr + wanted_size as u64;
                    let new_capacity = slot.capacity - wanted_size;
                    *slot = AddressSlot {
                        addr: if new_capacity == 0 {
                            MaybeU64::None
                        } else {
                            MaybeU64::Some(new_start)
                        },
                        capacity: new_capacity,
                    };
                    break;
                }
            }
        }

        if let Some(addr) = addr_to_reuse {
            self.ctrl.write(0, &header.serialize()?).await?;
            return Ok(addr);
        }

        // https://en.wikipedia.org/wiki/Region-based_memory_management
        // not freed addresses available in the list,
        // meaning we should fallback to just get the immediate next block
        let remaining = self
            .total_capacity()?
            .saturating_sub(header.extent_freed.global_offset as usize);
        if remaining < wanted_size {
            eyre::bail!("Could not allocate {wanted_size} bytes, only {remaining} left");
        }

        let addr = header.extent_freed.global_offset;
        header.extent_freed.global_offset += wanted_size as u64;
        self.ctrl.write(0, &header.serialize()?).await?;

        Ok(addr)
    }

    pub fn total_capacity(&self) -> eyre::Result<usize> {
        self.ctrl
            .total_capacity()
            .ok_or_else(|| eyre::eyre!("File system controller not ready"))
    }

    async fn resolve_path(&self, path: &Path) -> eyre::Result<(u64, INode)> {
        let components = path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();

        let (mut inode_addr, mut inode) = self.get_root_inode().await?;
        if components.is_empty() {
            return Ok((inode_addr, inode));
        }

        for component in components {
            match inode.kind {
                INodeKind::Directory => {
                    let payload = self.read_full_data_from_extent(inode.extent_addr).await?;
                    let directory = Directory::deserialize(&payload)?;
                    let mut found = None;
                    for (name, child_addr) in directory.entries {
                        if name == component {
                            found = Some(child_addr);
                            break;
                        }
                    }

                    let child_addr = found.ok_or_else(|| {
                        eyre::eyre!("Path component '{}' does not exist", component)
                    })?;
                    inode_addr = child_addr;
                    inode = INode::deserialize(
                        &self
                            .ctrl
                            .read(child_addr as usize, INode::serialized_size())
                            .await?,
                    )?;
                }
                INodeKind::File => {
                    eyre::bail!("Encountered file while traversing path");
                }
                INodeKind::Symlink => {
                    todo!()
                }
            }
        }

        Ok((inode_addr, inode))
    }

    async fn resolve_parent(&self, path: &Path) -> eyre::Result<(u64, INode, String)> {
        let parent = path.parent().ok_or_else(|| eyre::eyre!("Missing parent"))?;
        let filename = path
            .file_name()
            .ok_or_else(|| eyre::eyre!("Missing filename"))?
            .to_string_lossy()
            .to_string();
        let (addr, inode) = self.resolve_path(parent).await?;
        Ok((addr, inode, filename))
    }

    pub async fn ls(&self, path: PathBuf) -> eyre::Result<Vec<String>> {
        let (_, inode) = self.resolve_path(&path).await?;
        match inode.kind {
            INodeKind::Directory => {
                let payload = self.read_full_data_from_extent(inode.extent_addr).await?;
                let directory = Directory::deserialize(&payload)?;
                Ok(directory
                    .entries
                    .into_iter()
                    .map(|(name, _)| name)
                    .collect())
            }
            INodeKind::File => {
                eyre::bail!("Cannot ls a file");
            }
            INodeKind::Symlink => {
                todo!()
            }
        }
    }

    pub async fn fwrite(&self, path: PathBuf, data: Vec<u8>, opt: WriteOption) -> eyre::Result<()> {
        let (parent_addr, mut parent_inode, filename) = self.resolve_parent(&path).await?;
        let payload = self
            .read_full_data_from_extent(parent_inode.extent_addr)
            .await?;

        let mut directory = Directory::deserialize(&payload)?;

        // existing?
        for (name, inode_addr) in &directory.entries {
            if name == &filename {
                let mut inode = INode::deserialize(
                    &self
                        .ctrl
                        .read(*inode_addr as usize, INode::serialized_size())
                        .await?,
                )?;
                match inode.kind {
                    INodeKind::File => {
                        if !opt.overwrite {
                            eyre::bail!("File already exists");
                        }

                        self.free_full_extent(inode.extent_addr).await?;

                        let extent = Extent {
                            next: MaybeU64::None,
                            data,
                        };
                        let extent_addr = self.allocate(extent.serialized_size()).await?;

                        self.ctrl
                            .write(extent_addr as usize, &extent.serialize()?)
                            .await?;
                        inode.extent_addr = MaybeU64::Some(extent_addr);
                        inode.mtime = utc_now_u64();
                        self.ctrl
                            .write(*inode_addr as usize, &inode.serialize()?)
                            .await?;
                        return Ok(());
                    }
                    _ => eyre::bail!("Path is not file"),
                }
            }
        }

        // create new file
        let extent = Extent {
            next: MaybeU64::None,
            data,
        };

        let extent_addr = self.allocate(extent.serialized_size()).await?;
        self.ctrl
            .write(extent_addr as usize, &extent.serialize()?)
            .await?;

        let inode = INode {
            ctime: utc_now_u64(),
            mtime: utc_now_u64(),
            utime: utc_now_u64(),
            extent_addr: MaybeU64::Some(extent_addr),
            kind: INodeKind::File,
        };
        let inode_addr = self.allocate(INode::serialized_size()).await?;
        self.ctrl
            .write(inode_addr as usize, &inode.serialize()?)
            .await?;
        directory.entries.push((filename, inode_addr));

        self.free_full_extent(parent_inode.extent_addr).await?;

        let new_dir_extent = Extent {
            next: MaybeU64::None,
            data: directory.serialize()?,
        };
        let new_dir_extent_addr = self.allocate(new_dir_extent.serialized_size()).await?;
        self.ctrl
            .write(new_dir_extent_addr as usize, &new_dir_extent.serialize()?)
            .await?;
        parent_inode.extent_addr = MaybeU64::Some(new_dir_extent_addr);

        self.ctrl
            .write(parent_addr as usize, &parent_inode.serialize()?)
            .await?;

        Ok(())
    }

    pub async fn mkdir(&self, path: PathBuf, recursive: bool) -> eyre::Result<()> {
        let components = path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();

        let (mut curr_addr, mut curr_inode) = self.get_root_inode().await?;
        for component in components {
            match curr_inode.kind {
                INodeKind::Directory => {}
                _ => eyre::bail!("Non-directory in mkdir path"),
            }
            let payload = self
                .read_full_data_from_extent(curr_inode.extent_addr)
                .await?;

            let mut directory = Directory::deserialize(&payload)?;
            let mut found = None;
            for (name, inode_addr) in &directory.entries {
                if name == &component {
                    found = Some(*inode_addr);
                    break;
                }
            }

            if let Some(inode_addr) = found {
                curr_addr = inode_addr;
                curr_inode = INode::deserialize(
                    &self
                        .ctrl
                        .read(inode_addr as usize, INode::serialized_size())
                        .await?,
                )?;
                continue;
            }
            if !recursive {
                eyre::bail!("Directory '{component}' does not exist");
            }

            // create new dir inode
            let new_inode = INode {
                ctime: utc_now_u64(),
                mtime: utc_now_u64(),
                utime: utc_now_u64(),
                extent_addr: MaybeU64::None,
                kind: INodeKind::Directory,
            };

            let inode_addr = self.allocate(INode::serialized_size()).await?;
            self.ctrl
                .write(inode_addr as usize, &new_inode.serialize()?)
                .await?;
            directory.entries.push((component.clone(), inode_addr));
            let dir_data = directory.serialize()?;

            self.free_full_extent(curr_inode.extent_addr).await?;

            let extent_addr = self
                .allocate(
                    Extent {
                        next: MaybeU64::None,
                        data: dir_data,
                    }
                    .serialized_size(),
                )
                .await?;

            self.ctrl
                .write(
                    extent_addr as usize,
                    &Extent {
                        next: MaybeU64::None,
                        data: directory.serialize()?,
                    }
                    .serialize()?,
                )
                .await?;

            curr_inode.extent_addr = MaybeU64::Some(extent_addr);
            self.ctrl
                .write(curr_addr as usize, &curr_inode.serialize()?)
                .await?;

            curr_addr = inode_addr;
            curr_inode = new_inode;
        }

        Ok(())
    }

    pub async fn fread(&self, path: PathBuf) -> eyre::Result<Vec<u8>> {
        let (_, inode) = self.resolve_path(&path).await?;
        match inode.kind {
            INodeKind::File => self.read_full_data_from_extent(inode.extent_addr).await,
            INodeKind::Directory => {
                eyre::bail!("Cannot fread directory");
            }
            INodeKind::Symlink => {
                todo!()
            }
        }
    }

    pub async fn read_extent(&self, addr: u64) -> eyre::Result<Extent> {
        let extent_header = self.ctrl.read(addr as usize, 8 + 8).await?;
        let curr_extent_data_size = u64::from_le_bytes(extent_header[8..16].try_into()?);
        Extent::deserialize(
            &self
                .ctrl
                .read(addr as usize, 8 + 8 + curr_extent_data_size as usize)
                .await?,
        )
    }

    pub async fn mark_as_reusable(&self, new_slot_value: AddressSlot) -> eyre::Result<()> {
        let mut header = self.get_header().await?;
        let mut found = false;
        for slot in header.extent_freed.items.iter_mut() {
            if matches!(
                slot,
                AddressSlot {
                    addr: MaybeU64::None,
                    ..
                }
            ) {
                *slot = new_slot_value;
                found = true;
                break;
            }
        }

        if found {
            self.ctrl.write(0, &header.serialize()?).await?;
        }

        return Ok(());
    }

    pub async fn free_full_extent(&self, start_extent_addr: MaybeU64) -> eyre::Result<()> {
        let mut addr = start_extent_addr;
        while let MaybeU64::Some(next_addr) = addr {
            let extent = self.read_extent(next_addr).await?;

            self.mark_as_reusable(AddressSlot {
                addr: MaybeU64::Some(next_addr),
                capacity: extent.data.len(),
            })
            .await?;

            addr = extent.next;
        }

        Ok(())
    }

    // useful for extending dir entries
    pub async fn append_or_allocate_extent(
        &self,
        start_extent_addr: MaybeU64,
        new_extent: Extent,
    ) -> eyre::Result<u64> {
        let mut last_extent = None;
        let mut addr = start_extent_addr;
        while let MaybeU64::Some(next_addr) = addr {
            let extent = self.read_extent(next_addr).await?;
            addr = extent.next;
            last_extent = Some((next_addr, extent));
        }

        let mut all_extent_start = start_extent_addr.get();
        if let Some((prev_addr, mut prev_extent)) = last_extent {
            // TODO: HIGHLY inefficient
            // later just swap the address instead of a full block write
            // for now this should work for the sane of proving correctness
            let new_extent_addr = self.allocate(new_extent.serialized_size()).await?;
            prev_extent.next = MaybeU64::Some(new_extent_addr);
            self.ctrl
                .write(prev_addr as usize, &prev_extent.serialize()?)
                .await?;

            self.ctrl
                .write(new_extent_addr as usize, &new_extent.serialize()?)
                .await?;
        } else {
            // new
            let new_extent_addr = self.allocate(new_extent.serialized_size()).await?;
            self.ctrl
                .write(new_extent_addr as usize, &new_extent.serialize()?)
                .await?;
            all_extent_start = new_extent_addr;
        }

        Ok(all_extent_start)
    }

    pub async fn read_full_data_from_extent(&self, addr: MaybeU64) -> eyre::Result<Vec<u8>> {
        let mut data = vec![];
        let mut addr = addr;
        while let MaybeU64::Some(next_addr) = addr {
            let extent = self.read_extent(next_addr).await?;
            addr = extent.next;
            data.extend(extent.data);
        }
        Ok(data)
    }
}

fn utc_now_u64() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs()
}

fn u64_to_utc_datetime(timestamp: u64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp as i64, 0).expect("Invalid timestamp")
}
