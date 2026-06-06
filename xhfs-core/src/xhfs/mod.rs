use crate::{
    device::disk::Controller,
    utils::*,
    xhfs::{addr::MaybeU64, crypto::Crypto, ds::*},
};
use async_stream::try_stream;
use bytes::Bytes;
use eyre::{Context, ContextCompat};
use futures::{Stream, StreamExt};
use std::{
    fmt::Debug,
    io::{self},
    path::PathBuf,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt},
    sync::Mutex,
};
use tokio_util::io::StreamReader;
pub mod addr;
pub mod crypto;
pub mod ds;

macro_rules! xhfs_bail {
    ($msg:literal $(,)?) => {
        return Err(XHFSError::Error{ err: $msg.into() })
    };
    ($fmt:expr, $($arg:tt)*) => {
        return Err(XHFSError::Error { err: format!($fmt, $($arg)*) })
    };
}

pub struct XHFS {
    header_size: usize,
    alloc_guard: Mutex<()>,
    pub static_format: Format,
    pub geometry: GeometryLayout,
    pub ctrl: Controller,
}

#[derive(Debug, Clone, Default)]
pub struct WriteOption {
    pub overwrite: bool,
    pub modified: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct ExtentMetadata {
    pub full_aligned_region: RegionSlot,
    pub full_canon_region: RegionSlot,
    pub full_canon_data_slot: AddressSlot,
    pub next_extent: MaybeU64,
}

#[derive(Debug, Clone)]
pub struct AllocationSlot {
    pub absolute_byte_addr: u64,
    pub block_count: usize,
}

impl XHFS {
    pub async fn from_formatted(ctrl: Controller, password: Option<String>) -> eyre::Result<Self> {
        let header_size = XHFSHeader::template().serialize()?.len();
        let mut bfs = Self {
            header_size,
            ctrl,
            alloc_guard: Mutex::new(()),
            static_format: Format {
                param_data_block_count_per_group: 0,
                param_inode_count_per_group: 0,
                block_size_bytes: 0,
                group_count: 0,
            },
            geometry: Default::default(),
        };

        let header = bfs.get_header().await?;

        bfs.geometry = header.calculate_relative_geometry()?.0;
        bfs.static_format = header.format;
        bfs.static_format.validate()?;

        if let Some(password) = password {
            bfs.ctrl
                .setup_crypto(Crypto::new(&password, header.chacha20_nonce));
        }
        bfs.ensure_headers().await?;

        Ok(bfs)
    }

    pub async fn format_new(mut ctrl: Controller, password: Option<String>) -> eyre::Result<Self> {
        let mut header = XHFSHeader::template();
        header.chacha20_nonce = Crypto::gen_nonce();
        if let Some(password) = &password {
            ctrl.setup_crypto(Crypto::new(password, header.chacha20_nonce));
        }

        let total_capacity = ctrl
            .total_capacity()
            .ok_or_else(|| eyre::eyre!("Failed calculating total capacity"))?
            as u64;
        header.format = Format::infer_from_free_space(total_capacity, 20_480, 4096)?;

        let (g, b) = header.calculate_relative_geometry()?;
        let mut offset = 0;
        for _ in 0..header.format.group_count {
            // header
            {
                let start = g.rel_header_region.start.get();
                let header_addr = offset + start as usize;
                ctrl.raw_write(header_addr, &b.serialized_header).await?;
            }
            // data bitmap
            {
                let start = g.rel_data_bitmap_region.start.get();
                let bitmap_addr = offset + start as usize;
                ctrl.raw_write(bitmap_addr, &b.data_block_bitmap).await?;
            }
            // INode bitmap
            {
                let start = g.rel_inode_bitmap_region.start.get();
                let ibitmap_addr = offset + start as usize;
                ctrl.raw_write(ibitmap_addr, &b.inode_bitmap_placeholder)
                    .await?;
            }
            // INode table
            {
                let start = g.rel_inode_table_region.start.get();
                let itable_addr = offset + start as usize;
                ctrl.raw_write(itable_addr, &b.inode_table_placeholder)
                    .await?;
            }
            // zeroing data region is heavy but we can skip
            offset += g.group_stride as usize;
        }

        let bfs = Self::from_formatted(ctrl, password).await?;

        bfs.register_inode(
            &INode {
                kind: INodeKind::Directory,
                inumber: 1,
                nlink: 1,
                total_file_size: 0,
                extent_addr: MaybeU64::default(),
                mtime: utc_now_u64(),
                ctime: utc_now_u64(),
                extra_metadata: [0; 32],
            },
            false,
        )
        .await?;

        Ok(bfs)
    }

    pub fn resolve_inode_addr(&self, inumber: u64) -> Option<AddressSlot> {
        if let Some(group) = GroupLayout::derive_from_inode(inumber, &self.geometry) {
            let local_inode_index = (inumber - 1) % self.geometry.n_inodes_in_group;
            let start_inode_table_addr = group.inode_table_region.start.get();
            let inode_size = INode::serialized_size();
            let inode_addr = start_inode_table_addr + (inode_size as u64 * local_inode_index);

            return Some(AddressSlot {
                addr: inode_addr.into(),
                capacity: inode_size,
            });
        }
        None
    }

    async fn resolve_inode(&self, inumber: u64) -> eyre::Result<Option<INode>> {
        tracing::debug!("Resolving INode {inumber}");
        if let Some((slot, group)) = self
            .resolve_inode_addr(inumber)
            .zip(GroupLayout::derive_from_inode(inumber, &self.geometry))
        {
            let bitmap_slot = group.inode_bitmap_region.to_addr_slot();
            let raw_bitmap = self
                .ctrl
                .raw_read(bitmap_slot.addr.into(), bitmap_slot.capacity)
                .await?;
            tracing::debug!("Resolving INode {inumber} within bitmap {bitmap_slot}");

            let bitmap = Bitmap::deserialize(&raw_bitmap)?;

            let inode_index_in_group = (inumber - 1) % self.geometry.n_inodes_in_group;
            tracing::debug!("INode {inumber} index within group is {inode_index_in_group}");
            if !bitmap
                .get(inode_index_in_group as usize)
                .wrap_err("Resolving INode within bitmap")?
            {
                return Ok(None);
            }

            // decrypt!
            let raw_inode = self.ctrl.read(slot.addr.into(), slot.capacity).await?;

            return Some(INode::deserialize(&raw_inode)).transpose();
        }

        Ok(None)
    }

    async fn delete_inode_with_extent(&self, inumber: u64) -> eyre::Result<bool> {
        let mut deletion_stack = vec![inumber];
        let mut current_inumber = inumber;
        while let Some(inode) = self.resolve_inode(current_inumber).await? {
            if matches!(inode.kind, INodeKind::Hardlink) {
                let target_inode = self.follow_link_or_noop(inode).await?;
                if deletion_stack.contains(&target_inode.inumber) {
                    eyre::bail!("Hardlink loop detected during deletion pass");
                }
                deletion_stack.push(target_inode.inumber);
                current_inumber = target_inode.inumber;
            } else {
                break;
            }
        }

        while let Some(target_inumber) = deletion_stack.pop() {
            tracing::debug!("Processing deletion/link-decrement for INode {target_inumber}");
            let group = GroupLayout::derive_from_inode(target_inumber, &self.geometry).ok_or_else(
                || eyre::eyre!("Unable to derive group layout for inumber {target_inumber}"),
            )?;
            let bitmap_slot = group.inode_bitmap_region.to_addr_slot();
            let raw_bitmap = self
                .ctrl
                .raw_read(bitmap_slot.addr.into(), bitmap_slot.capacity)
                .await?;
            let mut bitmap = Bitmap::deserialize(&raw_bitmap)?;

            let inode_index_in_group = (target_inumber - 1) % self.geometry.n_inodes_in_group;
            if !bitmap
                .get(inode_index_in_group as usize)
                .wrap_err("Resolving INode within bitmap for deletion")?
            {
                continue; // already processed or invalid
            }

            let mut worth_deleting = false;
            if let Some(mut inode) = self.resolve_inode(target_inumber).await? {
                inode.nlink = inode.nlink.saturating_sub(1);

                if inode.nlink == 0 {
                    worth_deleting = true;
                    tracing::debug!(
                        "Cascading deletion to extent chain starting at 0x{:x}",
                        inode.extent_addr.get()
                    );
                    self.free_full_extent(inode.extent_addr).await?;
                } else {
                    self.register_inode(&inode, true).await?;
                }
            }

            if worth_deleting {
                bitmap.set(inode_index_in_group as usize, false)?;
                self.ctrl
                    .raw_write(bitmap_slot.addr.into(), &bitmap.serialize()?)
                    .await?;
            }
        }

        Ok(true)
    }

    pub async fn allocate_inode_near(
        &self,
        hint_inumber: u64,
        size: u64,
        extent_addr: MaybeU64,
        kind: INodeKind,
    ) -> eyre::Result<INode> {
        let _guard = self.alloc_guard.lock().await;
        let starting_group = GroupLayout::derive_from_inode(hint_inumber, &self.geometry)
            .ok_or_else(|| {
                eyre::eyre!("Unable to derive group layer from hint inumber {hint_inumber}")
            })?;

        let mut chosen_inumber = None;
        let mut chosen_group = None;
        let mut chosen_bitmap = None;
        for i in 0..self.static_format.group_count {
            let current_g_index = (starting_group.g_index + i) % self.static_format.group_count;
            let group = GroupLayout::derive_from_group_index(current_g_index, &self.geometry)
                .ok_or_else(|| {
                    eyre::eyre!("Failed calculating group layout for index {current_g_index}")
                })?;

            let bitmap_slot = group.inode_bitmap_region.to_addr_slot();
            let raw_bitmap = self
                .ctrl
                .raw_read(bitmap_slot.addr.into(), bitmap_slot.capacity)
                .await?;
            let bitmap = Bitmap::deserialize(&raw_bitmap)?;

            let free_slots = bitmap.runs_of(false, Some(self.geometry.n_inodes_in_group as usize));
            if let Some(slot) = free_slots.first() {
                let absolute_inumber =
                    (current_g_index * self.geometry.n_inodes_in_group) + (slot.start as u64) + 1;
                chosen_inumber = Some(absolute_inumber);
                chosen_group = Some(group);
                chosen_bitmap = Some(bitmap);
                break;
            }
        }

        let (inumber, group, mut bitmap) = match (chosen_inumber, chosen_group, chosen_bitmap) {
            (Some(num), Some(grp), Some(map)) => (num, grp, map),
            _ => eyre::bail!("Insufficient bitmap size, required 1 byte"),
        };

        let inode = INode {
            inumber,
            kind,
            nlink: 1,
            total_file_size: size,
            extent_addr,
            mtime: utc_now_u64(),
            ctime: utc_now_u64(),
            extra_metadata: [0; 32],
        };
        let inode_slot = self.resolve_inode_addr(inumber).ok_or_else(|| {
            eyre::eyre!(
                "Could not map table space layout coordinates for newly chosen inumber {inumber}"
            )
        })?;

        self.ctrl
            .write_owned(inode_slot.addr.into(), inode.serialize()?)
            .await?;
        let inode_index_in_group = (inumber - 1) % self.geometry.n_inodes_in_group;
        bitmap.set(inode_index_in_group as usize, true)?;

        let bitmap_slot = group.inode_bitmap_region.to_addr_slot();
        self.ctrl
            .raw_write(bitmap_slot.addr.into(), &bitmap.serialize()?)
            .await?;

        Ok(inode)
    }

    async fn register_inode(&self, inode: &INode, overwrite: bool) -> eyre::Result<()> {
        let _guard = self.alloc_guard.lock().await;
        let group = GroupLayout::derive_from_inode(inode.inumber, &self.geometry)
            .wrap_err("Unable to derive group when registering INode")?;

        let bitmap_slot = group.inode_bitmap_region.to_addr_slot();
        let raw_bitmap = self
            .ctrl
            .raw_read(bitmap_slot.addr.into(), bitmap_slot.capacity)
            .await?;
        let mut bitmap = Bitmap::deserialize(&raw_bitmap)?;

        let inode_index_in_group = (inode.inumber - 1) % self.geometry.n_inodes_in_group;
        let is_already_allocated = bitmap
            .get(inode_index_in_group as usize)
            .wrap_err("Scanning bitmap state")?;
        if !overwrite && is_already_allocated {
            eyre::bail!(
                "INode number {} exists already in group {}",
                inode.inumber,
                group.g_index
            );
        }

        let slot = self.resolve_inode_addr(inode.inumber).ok_or_else(|| {
            eyre::eyre!(
                "Could not map table space layout coordinates for inumber {}",
                inode.inumber
            )
        })?;

        // encrypt! otherwise people can reverse engineer dir entries
        self.ctrl
            .write(slot.addr.into(), &inode.serialize()?)
            .await?;
        // commit
        bitmap.set(inode_index_in_group as usize, true)?;
        self.ctrl
            .raw_write(bitmap_slot.addr.into(), &bitmap.serialize()?)
            .await?;

        Ok(())
    }

    #[inline]
    pub async fn update_inode_mtime(&self, mut inode: INode, mtime: u64) -> eyre::Result<INode> {
        inode.mtime = mtime;
        self.register_inode(&inode, true).await?;
        Ok(inode)
    }

    #[inline]
    pub async fn update_inode_mtime_now(&self, inode: INode) -> eyre::Result<INode> {
        self.update_inode_mtime(inode, utc_now_u64()).await
    }

    #[inline]
    pub async fn increment_inode_nlink(&self, mut inode: INode) -> eyre::Result<()> {
        inode.nlink += 1;
        self.register_inode(&inode, true).await
    }

    pub async fn format_headers_report(&self) -> eyre::Result<String> {
        let mut out = String::new();

        let header = self.get_header().await?;
        out.push_str(&format!("XHFS version: {}\n", header.version));

        let capacity = self.total_capacity()?;
        let rem_capacity = self.total_remaining_capacity().await?;
        out.push_str(&format!(
            "Capacity:  {capacity:>15} B ({})\n",
            bytesize::ByteSize(capacity as u64)
        ));
        out.push_str(&format!(
            "Remaining: {rem_capacity:>15} B ({})\n",
            bytesize::ByteSize(rem_capacity as u64)
        ));
        out.push_str(&format!("{}\n", self.static_format));
        out.push_str(&format!("{}\n", self.geometry));

        Ok(out)
    }

    pub async fn ensure_headers(&self) -> eyre::Result<()> {
        tracing::debug!("{}", self.format_headers_report().await?);
        Ok(())
    }

    pub async fn get_header(&self) -> eyre::Result<XHFSHeader> {
        XHFSHeader::deserialize(&self.ctrl.raw_read(0, self.header_size).await?)
    }

    pub async fn update_header(&self, header: XHFSHeader) -> eyre::Result<()> {
        let header = header;
        self.ctrl.raw_write(0, &header.serialize()?).await
    }

    pub async fn get_root_inode(&self) -> eyre::Result<INode> {
        self.resolve_inode(1)
            .await?
            .ok_or_else(|| eyre::eyre!("Could not find root inode"))
    }

    pub fn total_capacity(&self) -> eyre::Result<usize> {
        self.ctrl
            .total_capacity()
            .wrap_err("Failed retrieving total capacity")
    }

    pub async fn total_remaining_capacity(&self) -> eyre::Result<usize> {
        let mut total = 0;
        for g_index in 0..self.static_format.group_count {
            let group =
                GroupLayout::derive_from_group_index(g_index, &self.geometry).ok_or_else(|| {
                    eyre::eyre!("Failed calculating group layout for index {g_index}")
                })?;

            let slot = group.data_bitmap_region.to_addr_slot();
            let raw_bitmap = self.ctrl.raw_read(slot.addr.into(), slot.capacity).await?;
            let bitmap = Bitmap::deserialize(&raw_bitmap)?;

            let free_blocks = bitmap
                .runs_of(false, Some(self.geometry.usable_blocks_per_group as usize))
                .iter()
                .fold(0, |a, x| a + x.size);

            total += free_blocks * self.static_format.block_size_bytes as usize;
        }

        Ok(total)
    }

    pub async fn resolve_path<P: Into<PathBuf>>(&self, path: P) -> eyre::Result<INode> {
        let path: PathBuf = path.into();
        tracing::debug!("Resolving path {path:?}");
        let components = path_to_string_list(path);
        let mut inode = self.get_root_inode().await?;
        for (i, component) in components.iter().enumerate() {
            match inode.kind {
                INodeKind::Directory => {
                    tracing::debug!(" > Enter dir {component}");
                    let directory = {
                        let payload = self.read_full_data_from_extent(inode.extent_addr).await?;
                        Directory::deserialize(&payload)?
                    };

                    let child_inumber = directory
                        .entries
                        .iter()
                        .find(|(name, _)| name.eq(component))
                        .map(|(_, inumber)| *inumber)
                        .ok_or_else(|| {
                            eyre::eyre!(
                                "Path '{}' does not exist",
                                join_absolute(&components[..=i])
                            )
                        })?;

                    // IMMEDIATELY resolve the child target so it can be checked on the next pass
                    // or loop exit
                    inode = self.resolve_inode(child_inumber).await?.wrap_err_with(|| {
                        eyre::eyre!("Could not find INode child entry {child_inumber}")
                    })?;
                }
                INodeKind::File => {
                    eyre::bail!(
                        "Encountered file '{}' while expecting a directory container to reach components downstream",
                        join_absolute(&components[..i])
                    );
                }
                INodeKind::Symlink | INodeKind::Hardlink => {
                    eyre::bail!(
                        "Encountered link at path '{}'",
                        join_absolute(&components[..i])
                    );
                }
            }
        }

        Ok(inode)
    }

    async fn resolve_parent<P: Into<PathBuf>>(&self, path: P) -> eyre::Result<(INode, String)> {
        let path: PathBuf = path.into();
        tracing::debug!("Resolving parent of {path:?}");
        let parent = path.parent().ok_or_else(|| eyre::eyre!("Missing parent"))?;
        let filename = path
            .file_name()
            .ok_or_else(|| eyre::eyre!("Missing filename"))?
            .to_string_lossy()
            .to_string();
        let inode = self.resolve_path(parent).await?;
        Ok((inode, filename))
    }

    pub async fn ls<P: Into<PathBuf>>(&self, path: P) -> eyre::Result<Vec<String>> {
        let mut current_path: PathBuf = path.into();
        loop {
            let inode = self.resolve_path(&current_path).await?;

            match inode.kind {
                INodeKind::Directory => {
                    let payload = self.read_full_data_from_extent(inode.extent_addr).await?;
                    let directory = Directory::deserialize(&payload)?;
                    return Ok(directory
                        .entries
                        .into_iter()
                        .map(|(name, _)| name)
                        .collect());
                }
                INodeKind::Symlink => {
                    tracing::debug!("Trying to list dir entries from symlink");
                    let raw_path = self.read_full_data_from_extent(inode.extent_addr).await?;
                    let symlink = Symlink::deserialize(&raw_path)?;
                    tracing::debug!(
                        " {} *=> {}",
                        normalize_path(&current_path),
                        normalize_path(&symlink.path)
                    );
                    if symlink.path == current_path {
                        tracing::warn!("Invalid fs state: Symlink pointing to itself detected");
                        eyre::bail!("Symlink pointing to itself detected");
                    }
                    current_path = symlink.path;
                }
                INodeKind::File => {
                    eyre::bail!("Cannot ls a file");
                }
                INodeKind::Hardlink => {
                    eyre::bail!("Cannot ls a Hardlink as it only refers to a file");
                }
            }
        }
    }

    pub async fn fcopy<P: Into<PathBuf> + Clone>(
        &self,
        src: P,
        dest: P,
        opt: WriteOption,
    ) -> Result<(), XHFSError> {
        let data = self.fread(src).await?;
        self.fwrite(dest, data, opt).await
    }

    pub async fn fcopy_stream<P: Into<PathBuf> + Clone>(
        &self,
        src: P,
        dest: P,
        chunk_size: usize,
        opt: WriteOption,
    ) -> Result<(), XHFSError> {
        let stream = self.fread_stream(src, chunk_size).await?;
        let stream = into_reader(stream);
        self.fwrite_stream_unbounded(dest, stream, chunk_size, opt)
            .await
    }

    pub async fn fmove<P: Into<PathBuf>>(&self, src: P, dest: P) -> eyre::Result<()> {
        let src: PathBuf = src.into();
        let dest: PathBuf = dest.into();
        if dest.starts_with(&src) {
            eyre::bail!("Cannot move a directory into itself or its own subdirectory");
        }

        let (mut src_parent_inode, src_name) = self.resolve_parent(&src).await?;
        let (mut dst_parent_inode, dst_name) = self.resolve_parent(&dest).await?;
        let src_payload = self
            .read_full_data_from_extent(src_parent_inode.extent_addr)
            .await?;
        let dst_payload = self
            .read_full_data_from_extent(dst_parent_inode.extent_addr)
            .await?;

        // Same parent:
        // we need to be careful updating dir entries as there is only one (psrc = pdst)
        if src_parent_inode.inumber == dst_parent_inode.inumber {
            tracing::debug!("fmove found same parent for src and dest");
            let mut dir = Directory::deserialize(&src_payload)?;
            if dir.entries.iter().any(|(name, _)| name == &dst_name) {
                eyre::bail!("Destination already exists: {dst_name}");
            }
            let mut found_inumber = None;
            dir.entries.retain(|(name, inumber)| {
                if name == &src_name {
                    found_inumber = Some(*inumber);
                    false
                } else {
                    true
                }
            });
            let inumber = found_inumber.ok_or_else(|| eyre::eyre!("Source entry not found"))?;
            dir.entries.push((dst_name, inumber));

            let old_extent_addr = src_parent_inode.extent_addr;
            let new_extent_addr = self.allocate_and_write_extent(dir.serialize()?).await?;
            src_parent_inode.extent_addr = new_extent_addr;

            // commit
            self.update_inode_mtime_now(src_parent_inode).await?;
            // misc: guard against block reuse optimization
            if new_extent_addr != old_extent_addr {
                self.free_full_extent(old_extent_addr).await?;
            }

            return Ok(());
        }

        let mut src_dir = Directory::deserialize(&src_payload)?;
        let mut dst_dir = Directory::deserialize(&dst_payload)?;
        if dst_dir.entries.iter().any(|(name, _)| name == &dst_name) {
            eyre::bail!("Destination already exists: {dst_name}");
        }
        let mut found_inumber = None;
        src_dir.entries.retain(|(name, inumber)| {
            if name == &src_name {
                found_inumber = Some(*inumber);
                false
            } else {
                true
            }
        });

        let inumber = found_inumber.ok_or_else(|| eyre::eyre!("Source entry not found"))?;
        dst_dir.entries.push((dst_name, inumber));
        let old_src_extent = src_parent_inode.extent_addr;
        let old_dst_extent = dst_parent_inode.extent_addr;

        let new_src_extent = self.allocate_and_write_extent(src_dir.serialize()?).await?;
        let new_dst_extent = self.allocate_and_write_extent(dst_dir.serialize()?).await?;

        src_parent_inode.extent_addr = new_src_extent;
        dst_parent_inode.extent_addr = new_dst_extent;

        // commit sequence

        // We update DESTINATION directory first
        // so that if we crash right here, the file safely lives in both places (clone/Link state)
        self.update_inode_mtime_now(dst_parent_inode).await?;
        // then update SOURCE directory second
        // at this point the file is fully unlinked from its origin
        self.update_inode_mtime_now(src_parent_inode).await?;

        let mut free_plan = vec![];
        if new_src_extent != old_src_extent {
            free_plan.push(old_src_extent);
        }
        if new_dst_extent != old_dst_extent {
            free_plan.push(old_dst_extent);
        }
        if !free_plan.is_empty() {
            self.free_all(free_plan).await?;
        }

        Ok(())
    }

    pub async fn unlink<P: Into<PathBuf>>(&self, path: P) -> eyre::Result<()> {
        let path: PathBuf = path.into();
        let (mut parent_inode, filename) = self.resolve_parent(&path).await?;
        let old_parent_extent_addr = parent_inode.extent_addr;
        let mut directory = {
            let payload = self
                .read_full_data_from_extent(parent_inode.extent_addr)
                .await?;
            Directory::deserialize(&payload)
        }?;

        let entry_index = directory
            .entries
            .iter()
            .position(|(name, _)| name == &filename)
            .ok_or_else(|| eyre::eyre!("Path does not exist"))?;

        let (_, inumber) = directory.entries.remove(entry_index);
        let inode = self
            .resolve_inode(inumber)
            .await?
            .ok_or_else(|| eyre::eyre!("Resolving INode number {inumber} in direntry"))?;

        if let INodeKind::Directory = inode.kind {
            let dir_payload = self.read_full_data_from_extent(inode.extent_addr).await?;
            let dir = Directory::deserialize(&dir_payload)?;
            if !dir.entries.is_empty() {
                eyre::bail!("Directory is not empty");
            }
        }

        // commit sequence
        {
            let deleted = self.delete_inode_with_extent(inode.inumber).await?;
            if !deleted {
                eyre::bail!("Failed to delete target inode or already unallocated");
            }
            // NOTE+FIX(VERY VERY IMPORTANT DETAIL):
            // For every remaining INode, a full disk cleanup will leak 1 block (exactly 4096 B)
            // e.g. write foo.mp4, then rm foo.mp4, the remaining space will be missing 1 block
            // why? because we got a phantom extent of size 0 hiding in INode the INode directory
            // directory.len() => 0 => will waste 4KB
            if !directory.entries.is_empty() {
                let new_dir_extent_addr = self
                    .allocate_and_write_extent(directory.serialize()?)
                    .await?;
                parent_inode.extent_addr = new_dir_extent_addr;
            } else {
                parent_inode.extent_addr = MaybeU64::from(0);
            }

            parent_inode.mtime = utc_now_u64();
            self.register_inode(&parent_inode, true).await?;
            self.free_full_extent(old_parent_extent_addr).await?;
        }

        Ok(())
    }

    async fn blob_write<P: Into<PathBuf>>(
        &self,
        path: P,
        data: Vec<u8>,
        payload_type: INodeKind,
        opt: WriteOption,
    ) -> Result<(), XHFSError> {
        let remaining = self.total_remaining_capacity().await?;
        let inp_len = data.len();
        if inp_len >= remaining {
            return Err(XHFSError::from_report(eyre::eyre!(
                "Insufficient space, input size is {}, remaining {}, operation requires {} more",
                PrettySize(inp_len as u64),
                PrettySize(remaining as u64),
                PrettySize((inp_len.saturating_sub(remaining) + 1) as u64)
            )));
        }

        let path: PathBuf = path.into();
        tracing::debug!("Trying to write into {}", path.display());
        let (mut parent_inode, filename) = self.resolve_parent(&path).await?;
        let payload = self
            .read_full_data_from_extent(parent_inode.extent_addr)
            .await?;

        let mut directory = Directory::deserialize(&payload)?;
        tracing::debug!("Looking for dir entries of the same name as '{filename}'");
        for (name, inumber) in &directory.entries {
            if name == &filename {
                tracing::debug!("FOUND dir entry of the same name as '{filename}'");
                let mut inode = self
                    .resolve_inode(*inumber)
                    .await?
                    .ok_or_else(|| eyre::eyre!("Resolving INode for direntry '{name}'"))?;
                match inode.kind {
                    INodeKind::File | INodeKind::Symlink => {
                        if !opt.overwrite {
                            return Err(XHFSError::from_report(eyre::eyre!(
                                "File '{name}' already exists"
                            )));
                        }

                        let old_extent_addr = inode.extent_addr;
                        inode.total_file_size = data.len() as u64;
                        inode.extent_addr = self.allocate_and_write_extent(data).await?;
                        if let Some(mtime) = opt.modified {
                            self.update_inode_mtime(inode, mtime).await?;
                        } else {
                            self.update_inode_mtime_now(inode).await?;
                        }

                        self.free_full_extent(old_extent_addr).await?;

                        return Ok(());
                    }
                    _ => {
                        return Err(XHFSError::from_report(eyre::eyre!(
                            "Path '{name}' is not file"
                        )));
                    }
                }
            }
        }

        let size = data.len();
        tracing::debug!("Creating new file of size {size} B");
        // FIXME:
        // When extent payload is huge and we cut, it remains allocated!
        // => we should have a marker
        // fstream doesn't have the issue but it remains nice to have still..
        let extent_addr = self.allocate_and_write_extent(data).await?;

        if matches!(payload_type, INodeKind::Directory) {
            xhfs_bail!("Expected file like entry, got a folder instead");
        }

        // assume we never run of INodes
        let inode = self
            .allocate_inode_near(parent_inode.inumber, size as u64, extent_addr, payload_type)
            .await?;

        directory.entries.push((filename, inode.inumber));

        let new_dir_extent_addr = self
            .allocate_and_write_extent(directory.serialize()?)
            .await?;

        let old_parent_extent = parent_inode.extent_addr;
        parent_inode.extent_addr = new_dir_extent_addr;

        // commit
        self.update_inode_mtime_now(parent_inode).await?;
        self.free_full_extent(old_parent_extent).await?;

        Ok(())
    }

    #[inline]
    pub async fn fwrite<P: Into<PathBuf>>(
        &self,
        path: P,
        data: Vec<u8>,
        opt: WriteOption,
    ) -> Result<(), XHFSError> {
        self.blob_write(path, data, INodeKind::File, opt).await
    }

    pub async fn fwrite_stream_unbounded<P, R>(
        &self,
        path: P,
        mut stream: R,
        chunk_size: usize,
        opt: WriteOption,
    ) -> Result<(), XHFSError>
    where
        P: Into<PathBuf>,
        R: AsyncRead + Unpin,
    {
        let path: PathBuf = path.into();
        self.fwrite(&path, vec![], opt.clone()).await?;
        let mut buf = vec![0u8; chunk_size];
        {
            // Handle first chunk so that we have an extent to append to
            // in the next loop, the reason is that fappend is doing an extra resolve_path
            // producing more reads than necessary
            let n = stream.read(&mut buf).await.map_err(XHFSError::from_error)?;
            if n == 0 {
                return Ok(());
            }
            self.fappend(&path, buf[..n].to_vec(), opt.modified).await?;
        }

        let mut inode_with_extent = self.resolve_path(&path).await?;
        loop {
            if buf.len() != chunk_size {
                buf.resize(chunk_size, 0);
            }

            let n = stream.read(&mut buf).await.map_err(XHFSError::from_error)?;
            if n == 0 {
                break;
            }

            inode_with_extent = self
                .fappend_inode(inode_with_extent.clone(), buf[..n].to_vec(), opt.modified)
                .await?;
        }
        Ok(())
    }

    pub async fn fwrite_stream<P, R>(
        &self,
        path: P,
        mut stream: R,
        block_size: usize,
        opt: WriteOption,
    ) -> Result<(), XHFSError>
    where
        P: Into<PathBuf>,
        R: AsyncRead + AsyncSeek + Unpin,
    {
        let path: PathBuf = path.into();
        let inp_len = stream
            .seek(std::io::SeekFrom::End(0))
            .await
            .map_err(XHFSError::from_error)? as usize;
        stream
            .seek(std::io::SeekFrom::Start(0))
            .await
            .map_err(XHFSError::from_error)?;

        tracing::debug!("Incoming stream total size: {inp_len} bytes");
        let remaining = self.total_remaining_capacity().await?;
        if inp_len >= remaining {
            return Err(XHFSError::from_report(eyre::eyre!(
                "Insufficient space, input size is {}, remaining {}, operation requires {} more",
                PrettySize(inp_len as u64),
                PrettySize(remaining as u64),
                PrettySize((inp_len.saturating_sub(remaining) + 1) as u64)
            )));
        }

        self.fwrite_stream_unbounded(path, stream, block_size, opt)
            .await
    }

    pub async fn create_symlink<P: Into<PathBuf> + Clone>(
        &self,
        path: P,
        content: P,
        opt: WriteOption,
    ) -> eyre::Result<()> {
        let _ = self.resolve_parent(content.clone()).await?;
        self.blob_write(
            path,
            Symlink {
                path: content.into(),
            }
            .serialize(),
            INodeKind::Symlink,
            opt,
        )
        .await?;

        Ok(())
    }

    pub async fn create_hardlink<P: Into<PathBuf> + Clone>(
        &self,
        path: P,
        content: P,
        opt: WriteOption,
    ) -> eyre::Result<()> {
        let inode = self.resolve_path(content).await?;
        if !matches!(inode.kind, INodeKind::File) {
            eyre::bail!(
                "Unexpected {:?}, Hardlink only attaches to a file",
                inode.kind
            );
        }

        self.blob_write(
            path,
            Hardlink {
                inumber: inode.inumber,
            }
            .serialize(),
            INodeKind::Hardlink,
            opt,
        )
        .await?;

        tracing::debug!("Hardlink creation succeded, updating target inode nlink");
        self.increment_inode_nlink(inode).await?;

        Ok(())
    }

    pub async fn mkdir<P: Into<PathBuf>>(&self, path: P, recursive: bool) -> eyre::Result<bool> {
        let components = path_to_string_list(path);
        let mut created_new = false;
        let mut curr_inode = self.get_root_inode().await?;
        let at_root = components.len() <= 1;
        for component in components.into_iter() {
            match curr_inode.kind {
                INodeKind::Directory => {}
                _ => eyre::bail!("Non-directory in mkdir path"),
            }
            let payload = self
                .read_full_data_from_extent(curr_inode.extent_addr)
                .await?;

            let mut directory = Directory::deserialize(&payload)?;
            let mut found = None;
            for (name, inumber) in &directory.entries {
                if name == &component {
                    found = Some(*inumber);
                    break;
                }
            }

            if let Some(inumber) = found {
                curr_inode = self
                    .resolve_inode(inumber)
                    .await?
                    .wrap_err_with(|| eyre::eyre!("Resolving INode {inumber}"))?;
                continue;
            }
            if !recursive && !at_root {
                eyre::bail!("Directory '{component}' does not exist");
            }

            tracing::debug!("Creating new dir inode at {component}");
            let new_inode = self
                .allocate_inode_near(
                    curr_inode.inumber,
                    0,
                    MaybeU64::default(),
                    INodeKind::Directory,
                )
                .await?;

            created_new = true;
            directory
                .entries
                .push((component.clone(), new_inode.inumber));
            let dir_data = directory.serialize()?;

            let old_extent_addr = curr_inode.extent_addr;
            curr_inode.extent_addr = self.allocate_and_write_extent(dir_data).await?;
            curr_inode.mtime = utc_now_u64();

            // commit
            self.register_inode(&curr_inode, true).await?;
            self.free_full_extent(old_extent_addr).await?;

            curr_inode = new_inode;
        }

        Ok(created_new)
    }

    async fn follow_link_or_noop(&self, inode: INode) -> eyre::Result<INode, XHFSError> {
        match inode.kind {
            INodeKind::Hardlink => {
                tracing::debug!("Trying to read file from Hardlink");
                let inumber = self.read_full_data_from_extent(inode.extent_addr).await?;
                let hardlink = Hardlink::deserialize(&inumber)?;
                if hardlink.inumber == inode.inumber {
                    tracing::warn!("Invalid fs state: Hardlink pointing to itself detected");
                    return Err(XHFSError::from_report(eyre::eyre!(
                        "Hardlink pointing to itself detected"
                    )));
                }
                let referenced = self.resolve_inode(hardlink.inumber).await?.ok_or_else(|| {
                    eyre::eyre!("Referenced entry INode #{} not found", hardlink.inumber)
                })?;

                Ok(referenced)
            }
            INodeKind::Symlink => {
                tracing::debug!("Trying to read file from Symlink");
                let raw_path = self.read_full_data_from_extent(inode.extent_addr).await?;
                let symlink = Symlink::deserialize(&raw_path)?;
                let referenced = self.resolve_path(symlink.path).await?;
                Ok(referenced)
            }
            _ => Ok(inode),
        }
    }

    pub async fn fread<P: Into<PathBuf>>(&self, path: P) -> Result<Vec<u8>, XHFSError> {
        let path: PathBuf = path.into();
        let inode = self.resolve_path(&path).await?;
        tracing::debug!("Trying to read file from path {}", path.display());
        match inode.kind {
            INodeKind::File => self.read_full_data_from_extent(inode.extent_addr).await,
            INodeKind::Symlink | INodeKind::Hardlink => {
                tracing::debug!(" {:?} detected, following the payload", inode.kind);
                let referenced_inode = self.follow_link_or_noop(inode).await?;
                self.read_full_data_from_extent(referenced_inode.extent_addr)
                    .await
            }
            INodeKind::Directory => Err(XHFSError::from_report(eyre::eyre!(
                "Cannot fread directory"
            ))),
        }
    }

    // IDEA:
    // impl Seekable trait
    pub async fn fread_stream<P: Into<PathBuf>>(
        &self,
        path: P,
        chunk_size: usize,
    ) -> Result<impl Stream<Item = Result<Vec<u8>, XHFSError>>, XHFSError> {
        let path: PathBuf = path.into();
        let inode = self.resolve_path(&path).await?;
        tracing::debug!("Trying to stream read a file from path {}", path.display());
        match inode.kind {
            INodeKind::File => Ok(Box::pin(
                self.read_stream_from_extent_stream(inode.extent_addr, chunk_size),
            )),
            INodeKind::Symlink | INodeKind::Hardlink => {
                tracing::debug!(" {:?} detected, following the payload", inode.kind);
                let referenced_inode = self.follow_link_or_noop(inode).await?;
                Ok(Box::pin(self.read_stream_from_extent_stream(
                    referenced_inode.extent_addr,
                    chunk_size,
                )))
            }
            INodeKind::Directory => Err(XHFSError::from_report(eyre::eyre!(
                "Cannot fread directory"
            ))),
        }
    }

    pub async fn fseek<P: Into<PathBuf>>(
        &self,
        path: P,
        start: u64,
        end: u64,
    ) -> eyre::Result<Vec<u8>> {
        let path: PathBuf = path.into();
        let inode = self.resolve_path(&path).await?;
        let inode = self.follow_link_or_noop(inode).await?;
        match inode.kind {
            INodeKind::File => {
                self.seek_full_data_from_extent(inode.extent_addr, start, end)
                    .await
            }
            INodeKind::Directory => {
                eyre::bail!("Cannot fread directory");
            }
            INodeKind::Symlink | INodeKind::Hardlink => {
                eyre::bail!(
                    "Unexpected {:?}, should have been solved upstream",
                    inode.kind
                )
            }
        }
    }

    pub async fn fappend_inode(
        &self,
        mut inode: INode,
        data: Vec<u8>,
        mtime: Option<u64>,
    ) -> Result<INode, XHFSError> {
        loop {
            match inode.kind {
                INodeKind::File => {
                    inode.total_file_size += data.len() as u64;
                    inode.extent_addr = self
                        .append_or_allocate_extent(inode.extent_addr, data)
                        .await?;
                    if let Some(mtime) = mtime {
                        self.update_inode_mtime(inode.clone(), mtime).await?;
                    } else {
                        self.update_inode_mtime_now(inode.clone()).await?;
                    }
                    return Ok(inode);
                }
                INodeKind::Symlink => {
                    inode = self.follow_link_or_noop(inode).await?;
                }
                INodeKind::Hardlink => {
                    tracing::debug!("Trying to fappend file from Hardlink");
                    let referenced = self.follow_link_or_noop(inode.clone()).await?;
                    tracing::debug!(
                        " INode #{} *=> INode #{}",
                        inode.inumber,
                        referenced.inumber
                    );
                    if referenced.inumber == inode.inumber {
                        xhfs_bail!("Hardlink pointing to itself detected");
                    }
                    inode = referenced;
                }
                INodeKind::Directory => {
                    xhfs_bail!("Cannot append data to directory");
                }
            }
        }
    }

    #[inline]
    pub async fn fappend<P: Into<PathBuf>>(
        &self,
        path: P,
        data: Vec<u8>,
        mtime: Option<u64>,
    ) -> Result<(), XHFSError> {
        let path: PathBuf = path.into();
        let inode = self.resolve_path(&path).await?;
        self.fappend_inode(inode, data, mtime).await?;
        Ok(())
    }

    #[inline]
    pub async fn exists<P: Into<PathBuf>>(&self, path: P) -> eyre::Result<bool> {
        Ok(self.stats(path, false).await?.is_some())
    }

    pub async fn stats<P: Into<PathBuf>>(
        &self,
        path: P,
        follow_hardlink: bool,
    ) -> eyre::Result<Option<EntryStat>> {
        let path: PathBuf = path.into();
        let components = path_to_string_list(path.clone());

        if components.is_empty() {
            let inode = self.get_root_inode().await?;
            return Ok(Some(EntryStat {
                name: "/".to_string(),
                size: None,
                nlink: inode.nlink,
                kind: inode.kind,
                mtime: inode.mtime,
                ctime: inode.ctime,
            }));
        }
        let inode = match self.resolve_path(path).await {
            Ok(inode) => {
                if follow_hardlink && matches!(inode.kind, INodeKind::Hardlink) {
                    self.follow_link_or_noop(inode).await?
                } else {
                    inode
                }
            }
            Err(_) => return Ok(None),
        };
        let name = components.last().unwrap().clone();

        Ok(Some(EntryStat {
            name,
            size: match inode.kind {
                INodeKind::Directory => None,
                _ => Some(inode.total_file_size as usize),
            },
            nlink: inode.nlink,
            kind: inode.kind,
            mtime: inode.mtime,
            ctime: inode.ctime,
        }))
    }

    pub async fn allocate(
        &self,
        mut wanted_blocks: usize,
    ) -> Result<Vec<AllocationSlot>, XHFSError> {
        tracing::debug!("Planning allocation for {wanted_blocks} blocks");
        let _guard = self.alloc_guard.lock().await;
        let mut planned_allocations = vec![];
        let mut bitmaps_to_commit = vec![];

        // SIMPLE: contiguous block search
        'outer: for g_index in 0..self.static_format.group_count {
            let group = GroupLayout::derive_from_group_index(g_index, &self.geometry).unwrap();
            let bitmap_slot = group.data_bitmap_region.to_addr_slot();
            let raw_bitmap = self
                .ctrl
                .raw_read(bitmap_slot.addr.into(), bitmap_slot.capacity)
                .await?;
            let bitmap = Bitmap::deserialize(&raw_bitmap)?;

            let zero_runs =
                bitmap.runs_of(false, Some(self.geometry.usable_blocks_per_group as usize));
            for slot in zero_runs {
                if slot.size >= wanted_blocks {
                    let mut mut_bitmap = bitmap.clone();
                    mut_bitmap.set_range(slot.start, wanted_blocks, true)?;

                    let data_offset = group.data_region.start.get();
                    let absolute_byte_addr =
                        data_offset + (slot.start as u64) * self.static_format.block_size_bytes;

                    planned_allocations.push(AllocationSlot {
                        absolute_byte_addr,
                        block_count: wanted_blocks,
                    });
                    bitmaps_to_commit.push((bitmap_slot.addr, mut_bitmap));
                    wanted_blocks = 0;
                    break 'outer;
                }
            }
        }

        // FALLBACK: collect fragments across groups
        if wanted_blocks > 0 {
            for g_index in 0..self.static_format.group_count {
                if wanted_blocks == 0 {
                    break;
                }
                let group = GroupLayout::derive_from_group_index(g_index, &self.geometry).unwrap();
                let bitmap_slot = group.data_bitmap_region.to_addr_slot();
                let raw_bitmap = self
                    .ctrl
                    .raw_read(bitmap_slot.addr.into(), bitmap_slot.capacity)
                    .await?;
                let mut bitmap = Bitmap::deserialize(&raw_bitmap)?;

                let zero_runs =
                    bitmap.runs_of(false, Some(self.geometry.usable_blocks_per_group as usize));
                let mut bitmap_changed = false;
                for slot in zero_runs {
                    if wanted_blocks == 0 {
                        break;
                    }
                    let blocks_to_take = slot.size.min(wanted_blocks);
                    bitmap.set_range(slot.start, blocks_to_take, true)?;
                    bitmap_changed = true;

                    let data_offset = group.data_region.start.get();
                    let absolute_byte_addr =
                        data_offset + (slot.start as u64) * self.static_format.block_size_bytes;
                    tracing::warn!(
                        "PLANNING ALLOCATED: group index {}, start block {}, count {}",
                        g_index,
                        slot.start,
                        blocks_to_take
                    );
                    planned_allocations.push(AllocationSlot {
                        absolute_byte_addr,
                        block_count: blocks_to_take,
                    });
                    wanted_blocks -= blocks_to_take;
                }

                if bitmap_changed {
                    bitmaps_to_commit.push((bitmap_slot.addr, bitmap));
                }
            }
        }

        if wanted_blocks > 0 {
            return Err(XHFSError::Insufficient {
                operation: "allocate".to_string(),
                wanted: wanted_blocks,
            });
        }

        // commit!
        for (bitmap_addr, updated_bitmap) in bitmaps_to_commit {
            self.ctrl
                .raw_write(bitmap_addr.into(), &updated_bitmap.serialize()?)
                .await?;
        }

        Ok(planned_allocations)
    }

    pub async fn mark_as_reusable(&self, addr_slot: AddressSlot) -> eyre::Result<()> {
        let addr = addr_slot.addr.get();
        let size = addr_slot.capacity;
        tracing::debug!("Marking slot 0x{addr:x} of size {size} as reusable");

        let group = GroupLayout::derive_from_address(addr, &self.geometry)
            .ok_or_else(|| eyre::eyre!("Could not derive group for address slot {addr_slot}"))?;

        let bitmap_slot = group.data_bitmap_region.to_addr_slot();
        let raw_bitmap = self
            .ctrl
            .raw_read(bitmap_slot.addr.into(), bitmap_slot.capacity)
            .await?;
        let mut bitmap = Bitmap::deserialize(&raw_bitmap)?;

        let block_size = self.static_format.block_size_bytes as u64;
        let block_start = (addr - group.data_region.start.get()) / block_size;
        // make trailing partial block fully freed
        let calculated_blocks = (size as u64 + block_size - 1) / block_size;
        let blocks_count = calculated_blocks.max(1);
        // ensure we don't spil past this group's legal bitmap bounds
        let max_blocks = self.geometry.usable_blocks_per_group as u64;
        if block_start + blocks_count > max_blocks {
            eyre::bail!(
                "Bitmap boundary violation in group! Trying to free blocks {}..{} but max is {}",
                block_start,
                block_start + blocks_count,
                max_blocks
            );
        }
        bitmap.set_range(block_start as usize, blocks_count as usize, false)?;

        // commit
        tracing::warn!("FREED: start block {block_start}, count {blocks_count}");
        self.ctrl
            .raw_write(bitmap_slot.addr.into(), &bitmap.serialize()?)
            .await
    }

    pub async fn allocate_and_write_extent(&self, data: Vec<u8>) -> Result<MaybeU64, XHFSError> {
        tracing::debug!("Allocating and writing {} B", data.len());

        let metadata_overhead = Extent::emulate_serialized_size(0);
        let block_size = self.static_format.block_size_bytes as usize;

        // PREFLIGHT
        let mut remaining_payload = data.len();
        let mut exact_blocks_needed = 0;
        let max_usable_blocks_per_group = self.geometry.usable_blocks_per_group as usize;
        while remaining_payload > 0 {
            let max_slot_bytes = max_usable_blocks_per_group * block_size;
            if max_slot_bytes <= metadata_overhead {
                return Err(XHFSError::Error {
                    err: "Group capacity smaller than extent metadata overhead".to_string(),
                });
            }
            let max_payload_per_slot = max_slot_bytes - metadata_overhead;
            let chunk_size = std::cmp::min(remaining_payload, max_payload_per_slot);
            let extent_bytes = Extent::emulate_serialized_size(chunk_size);
            let blocks_for_chunk = (extent_bytes + block_size - 1) / block_size;

            exact_blocks_needed += blocks_for_chunk;
            remaining_payload -= chunk_size;
        }

        let allocation_slots = self.allocate(exact_blocks_needed).await?;
        let mut data_slice = &data[..];
        let mut serialization_plan = vec![];
        for slot in allocation_slots {
            if data_slice.is_empty() {
                break;
            }
            let slot_bytes_capacity = slot.block_count * block_size;
            if slot_bytes_capacity <= metadata_overhead {
                continue;
            }
            let max_payload_for_slot = slot_bytes_capacity - metadata_overhead;
            let chunk_size = std::cmp::min(data_slice.len(), max_payload_for_slot);
            let (current_chunk, remaining) = data_slice.split_at(chunk_size);
            serialization_plan.push((slot.absolute_byte_addr, current_chunk.to_vec()));
            data_slice = remaining;
        }

        if !data_slice.is_empty() {
            return Err(XHFSError::Error {
                err: format!(
                    "Layout mismatch: {} bytes left unallocated",
                    data_slice.len()
                ),
            });
        }

        // commit! (reverse)
        let mut next_link = MaybeU64::default();
        for (absolute_byte_addr, chunk_data) in serialization_plan.into_iter().rev() {
            let extent = Extent {
                next: next_link,
                data: chunk_data,
            };
            self.ctrl
                .write_owned(
                    absolute_byte_addr as usize,
                    extent
                        .serialize()
                        .map_err(|e| XHFSError::Error { err: e.to_string() })?,
                )
                .await
                .map_err(|e| XHFSError::Error { err: e.to_string() })?;
            next_link = MaybeU64::from(absolute_byte_addr);
        }

        Ok(next_link)
    }

    pub async fn free_full_extent(&self, start_extent_addr: MaybeU64) -> eyre::Result<()> {
        let _guard = self.alloc_guard.lock().await;
        tracing::debug!(
            "Freeing extent chain starting at 0x{:x}",
            start_extent_addr.get()
        );

        let mut addr = start_extent_addr;
        while let Some(current_addr) = addr.to_optional() {
            let meta = self.read_extent_metadata(current_addr).await?;
            self.mark_as_reusable(AddressSlot {
                addr: MaybeU64::from(current_addr),
                capacity: meta.full_aligned_region.size_span() as usize,
            })
            .await?;
            addr = meta.next_extent;
        }

        Ok(())
    }

    async fn free_all<A>(&self, addresses: A) -> eyre::Result<()>
    where
        A: IntoIterator<Item = MaybeU64>,
        A::IntoIter: ExactSizeIterator,
    {
        for addr in addresses {
            self.free_full_extent(addr).await?;
        }
        Ok(())
    }

    async fn append_or_allocate_extent(
        &self,
        start_extent_addr: MaybeU64,
        data: Vec<u8>,
    ) -> Result<MaybeU64, XHFSError> {
        let mut last_extent = None;
        let mut addr = start_extent_addr;

        while let Some(current_addr) = addr.to_optional() {
            last_extent = Some(current_addr);
            let meta = self.read_extent_metadata(current_addr).await?;
            addr = meta.next_extent;
        }

        let mut all_extent_start = start_extent_addr;
        if let Some(prev_extent_addr) = last_extent {
            let new_next_extent_addr = self.allocate_and_write_extent(data).await?;
            self.update_extent_next(prev_extent_addr, new_next_extent_addr)
                .await?;
        } else {
            // new
            all_extent_start = self.allocate_and_write_extent(data).await?;
        }

        Ok(all_extent_start)
    }

    pub fn read_stream_from_extent_stream(
        &self,
        mut addr: MaybeU64,
        chunk_size: usize,
    ) -> impl Stream<Item = Result<Vec<u8>, XHFSError>> {
        try_stream! {
            let mut buffer = vec![];
            while let Some(next_addr) = addr.to_optional() {
                tracing::debug!("Resolving extent 0x{next_addr:x}");
                let extent = self.read_extent(next_addr).await?;
                tracing::debug!("  Extent at 0x{:x} is of size {} B", next_addr, extent.data.len());
                addr = extent.next;
                buffer.extend(extent.data);

                while buffer.len() >= chunk_size {
                    let chunk = buffer.drain(..chunk_size).collect::<Vec<u8>>();
                    yield chunk;
                }
            }

            if !buffer.is_empty() {
                yield buffer;
            }
        }
    }

    async fn read_full_data_from_extent(&self, addr: MaybeU64) -> Result<Vec<u8>, XHFSError> {
        let mut stream = Box::pin(
            self.read_stream_from_extent_stream(addr, self.static_format.block_size_bytes as usize),
        );
        let mut data = vec![];
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result?;
            data.extend(chunk);
        }

        Ok(data)
    }

    pub async fn read_extent(&self, addr: u64) -> Result<Extent, XHFSError> {
        let meta = self.read_extent_metadata(addr).await?;
        let data = self
            .ctrl
            .read(
                meta.full_canon_data_slot.addr.into(),
                meta.full_canon_data_slot.capacity,
            )
            .await?;
        Ok(Extent {
            next: meta.next_extent,
            data,
        })
    }

    pub async fn read_extent_metadata(&self, addr: u64) -> eyre::Result<ExtentMetadata> {
        let full_offset = (Extent::HEADER_CAP_OFFSET + Extent::HEADER_CAP_OFFSET) as usize;
        eyre::ensure!(full_offset == 16);
        let extent_header = self.ctrl.read(addr as usize, full_offset).await?;
        let curr_extent_data_size = u64::from_le_bytes(extent_header[0..8].try_into()?);
        let next_extent = MaybeU64::from(u64::from_le_bytes(extent_header[8..16].try_into()?));
        let raw_footprint = 16 + curr_extent_data_size as u64;
        let block_size = self.static_format.block_size_bytes as u64;
        let aligned_capacity = ((raw_footprint + block_size - 1) / block_size) * block_size;

        Ok(ExtentMetadata {
            full_aligned_region: RegionSlot {
                start: MaybeU64::from(addr),
                end: MaybeU64::from(addr + aligned_capacity),
            },
            full_canon_region: RegionSlot {
                start: MaybeU64::from(addr),
                end: MaybeU64::from(addr + raw_footprint),
            },
            full_canon_data_slot: AddressSlot {
                addr: MaybeU64::from(addr + full_offset as u64),
                capacity: curr_extent_data_size as usize,
            },
            next_extent,
        })
    }

    pub async fn find_full_extent_metadata(
        &self,
        mut addr: MaybeU64,
        stop: Option<u32>,
    ) -> eyre::Result<Vec<ExtentMetadata>> {
        let mut blocks = vec![];
        let max = stop.unwrap_or(u32::MAX);
        let mut i = 1;
        while let Some(next_addr) = addr.to_optional() {
            if i - 1 >= max {
                break;
            }
            tracing::debug!("Resolving {i}-th extent metadata 0x{next_addr:x}");
            let meta = self.read_extent_metadata(next_addr).await?;
            addr = meta.next_extent;
            blocks.push(meta);
            i += 1;
        }
        Ok(blocks)
    }

    async fn update_extent_next(&self, start_extent_addr: u64, next: MaybeU64) -> eyre::Result<()> {
        self.ctrl
            .write(
                (start_extent_addr + Extent::HEADER_NEXT_OFFSET) as usize,
                &next.get().to_le_bytes(),
            )
            .await
    }

    async fn seek_full_data_from_extent(
        &self,
        addr: MaybeU64,
        addr_start: u64,
        addr_end: u64,
    ) -> eyre::Result<Vec<u8>> {
        // tracing::warn!("   Seeking 0x{addr_start:08x} - 0x{addr_end:08x}");
        let mut buf = vec![];
        let mut cursor = 0;
        let mut addr = addr;
        while let Some(next_addr) = addr.to_optional() {
            let meta = self.read_extent_metadata(next_addr).await?;
            addr = meta.next_extent;
            let extent_start = cursor;
            let extent_end = cursor + meta.full_canon_data_slot.capacity as u64;
            if extent_end <= addr_start {
                cursor = extent_end;
                continue;
            }
            if extent_start >= addr_end {
                break;
            }

            let start_in_ext = addr_start.saturating_sub(extent_start) as usize;
            let end_in_ext = (addr_end.saturating_sub(extent_start) as usize)
                .min(meta.full_canon_data_slot.capacity);
            if start_in_ext < meta.full_canon_data_slot.capacity && start_in_ext < end_in_ext {
                // TODO:
                // streamable
                let data = self
                    .ctrl
                    .read(
                        meta.full_canon_data_slot.addr.get() as usize + start_in_ext,
                        end_in_ext.saturating_sub(start_in_ext) as usize,
                    )
                    .await?;
                buf.extend_from_slice(&data);
            }
            cursor = extent_end;
        }

        Ok(buf)
    }
}

pub fn into_reader<S>(stream: S) -> impl AsyncRead + Unpin
where
    S: Stream<Item = Result<Vec<u8>, XHFSError>> + Unpin,
{
    StreamReader::new(stream.map(|chunk| {
        chunk
            .map(Bytes::from)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }))
}
