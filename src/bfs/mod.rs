use crate::{
    bfs::{addr::MaybeU64, crypto::Crypto, ds::*},
    device::disk::Controller,
    utils::*,
};
use async_recursion::async_recursion;
use eyre::Context;
use std::{fmt::Debug, io::SeekFrom, path::PathBuf};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt},
    sync::Mutex,
};

pub mod addr;
pub mod crypto;
pub mod ds;

macro_rules! bfs_bail {
    ($msg:literal $(,)?) => {
        return Err(BruteFsError::Error{ err: $msg.into() })
    };
    ($fmt:expr, $($arg:tt)*) => {
        return Err(BruteFsError::Error { err: format!($fmt, $($arg)*) })
    };
}

pub struct BruteFS {
    header_size: usize,
    alloc_guard: Mutex<()>,
    pub ctrl: Controller,
}

#[derive(Debug, Clone, Default)]
pub struct WriteOption {
    pub overwrite: bool,
}

impl BruteFS {
    pub async fn from_formatted(ctrl: Controller, password: Option<String>) -> eyre::Result<Self> {
        let header_size = Self::header_template().serialize()?.len();
        let mut bfs = Self {
            header_size,
            ctrl,
            alloc_guard: Mutex::new(()),
        };

        let header = bfs.get_header().await?;
        if let Some(password) = password {
            bfs.ctrl
                .setup_crypto(Crypto::new(&password, header.chacha20_nonce));
        }
        bfs.ensure_headers().await?;

        Ok(bfs)
    }

    pub async fn format_new(mut ctrl: Controller, password: Option<String>) -> eyre::Result<Self> {
        let header_size = Self::header_template().serialize()?.len();
        let root = INode {
            ctime: utc_now_u64(),
            mtime: utc_now_u64(),
            total_file_size: 0,
            extent_addr: MaybeU64::default(),
            kind: INodeKind::Directory,
        };

        let root_raw = root.serialize()?;

        let mut header = Self::header_template();
        header.extent_freed.global_offset = (header_size + root_raw.len()) as u64;
        header.chacha20_nonce = Crypto::gen_nonce();
        if let Some(password) = &password {
            ctrl.setup_crypto(Crypto::new(password, header.chacha20_nonce));
        }
        ctrl.raw_write(0, &header.serialize()?).await?;
        ctrl.write(header_size, &root_raw).await?;

        let bfs = Self::from_formatted(ctrl, password).await?;
        // bfs.mkdir("/", false).await?;
        Ok(bfs)
    }

    pub async fn format_headers_report(&self) -> eyre::Result<String> {
        let mut out = String::new();

        let header = self.get_header().await?;

        out.push_str(&format!("brutefs version: {}\n", header.version));

        out.push_str(&format!(
            "- Current global offset: {} (0x{:08x})\n",
            header.extent_freed.global_offset, header.extent_freed.global_offset,
        ));

        let regions = self.reusable_regions().await?;
        out.push_str(&format!("- Total known fragments: {}\n", regions.len()));
        let show = 3;
        out.push_str(&format!("Top {show} smallest:\n"));
        for region in regions.iter().take(show).filter(|r| r.capacity > 0) {
            out.push_str(&region.to_string());
        }
        out.push_str(&format!("Top {show} biggest:\n"));
        for region in regions.iter().rev().take(show) {
            out.push_str(&region.to_string());
        }

        let capacity = self.total_capacity()?;
        let rem_capacity = self.total_remaining_capacity().await?;
        out.push_str(&format!("Capacity:  {capacity:>10} B\n"));
        out.push_str(&format!("Remaining: {rem_capacity:>10} B\n"));

        let (ioffset, inode) = self
            .get_root_inode()
            .await
            .wrap_err_with(|| eyre::eyre!("Data is either corrupt or encrypted"))?;

        out.push_str(&format!("Root inode offset {ioffset} (0x{ioffset:08x})\n"));
        out.push_str(&inode.to_string());

        Ok(out)
    }

    pub async fn ensure_headers(&self) -> eyre::Result<()> {
        tracing::debug!("{}", self.format_headers_report().await?);
        Ok(())
    }

    fn header_template() -> BruteFsHeader {
        BruteFsHeader {
            version: 1,
            extent_freed: AddressVector::allocate(1000),
            chacha20_nonce: Default::default(),
        }
    }

    pub async fn get_header(&self) -> eyre::Result<BruteFsHeader> {
        BruteFsHeader::deserialize(&self.ctrl.raw_read(0, self.header_size).await?)
    }

    pub async fn update_header(&self, header: BruteFsHeader) -> eyre::Result<()> {
        let mut header = header;
        header.extent_freed.compactify();
        self.ctrl.raw_write(0, &header.serialize()?).await
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

    /// By design, a single instance mounts/owns the file system
    /// so 'clients' will have to interface ops through that instance.
    ///
    /// This avoids implementing a busy flag or some crazy lock mechanism on disk
    /// especially since locks themselves expect their implementations to be even more atomic.
    pub async fn allocate(&self, wanted_size: usize) -> Result<u64, BruteFsError> {
        let _ = self.alloc_guard.lock().await;

        tracing::debug!("Trying to allocate {wanted_size}");
        let mut header = self.get_header().await?;

        let mut addr_to_reuse = None;
        let mut max_slot_size_seen = 0;
        for slot in header.extent_freed.items.iter_mut() {
            if let Some(free_addr) = slot.addr.to_optional() {
                max_slot_size_seen = max_slot_size_seen.max(slot.capacity);
                if wanted_size <= slot.capacity {
                    tracing::debug!("Found free slot at 0x{free_addr}");
                    addr_to_reuse = Some(free_addr);

                    let new_start = free_addr + wanted_size as u64;
                    let new_capacity = slot.capacity - wanted_size;
                    tracing::debug!("Split space left at 0x{new_start} of size {new_capacity}");

                    *slot = AddressSlot {
                        addr: if new_capacity == 0 {
                            MaybeU64::default()
                        } else {
                            MaybeU64::from(new_start)
                        },
                        capacity: new_capacity,
                    };
                    break;
                }
            }
        }

        if let Some(addr) = addr_to_reuse {
            self.update_header(header).await?;
            return Ok(addr);
        }

        // https://en.wikipedia.org/wiki/Region-based_memory_management
        // not freed addresses available in the list,
        // meaning we should fallback to just get the immediate next block
        let remaining = self
            .total_capacity()?
            .saturating_sub(header.extent_freed.global_offset as usize);
        if remaining < wanted_size {
            return Err(BruteFsError::Insufficient {
                wanted: wanted_size,
                max_slot_size: max_slot_size_seen,
                left_contiguous: remaining,
            });
        }

        let addr = header.extent_freed.global_offset;
        header.extent_freed.global_offset += wanted_size as u64;
        self.update_header(header).await?;

        Ok(addr)
    }

    pub fn total_capacity(&self) -> eyre::Result<usize> {
        self.ctrl
            .total_capacity()
            .ok_or_else(|| eyre::eyre!("File system controller not ready"))
    }

    pub async fn total_remaining_capacity(&self) -> eyre::Result<usize> {
        let mut total = self.total_capacity()?;
        let header = self.get_header().await?;
        let global_offset = self.get_header().await?.extent_freed.global_offset;
        let left_contiguous = total.saturating_sub(global_offset as usize);
        let reusable_total = self
            .reusable_regions()
            .await?
            .iter()
            .map(|c| c.capacity)
            .sum::<usize>();

        total = total.saturating_sub(left_contiguous);
        total = total.saturating_sub(header.serialized_size());

        total = total.saturating_add(reusable_total);

        Ok(total)
    }

    pub async fn resolve_path<P: Into<PathBuf>>(&self, path: P) -> eyre::Result<(u64, INode)> {
        let path: PathBuf = path.into();
        tracing::debug!("Resolving {path:?}");
        let components = path_to_string_list(path);

        let (mut inode_addr, mut inode) = self.get_root_inode().await?;
        if components.is_empty() {
            return Ok((inode_addr, inode));
        }

        for (i, component) in components.iter().enumerate() {
            match inode.kind {
                INodeKind::Directory => {
                    tracing::debug!(" > Enter dir {component}");
                    let payload = self.read_full_data_from_extent(inode.extent_addr).await?;
                    let directory = Directory::deserialize(&payload)?;
                    let mut found = None;
                    for (name, child_addr) in directory.entries {
                        if name.eq(component) {
                            found = Some(child_addr);
                            break;
                        }
                    }

                    let child_addr = found.ok_or_else(|| {
                        eyre::eyre!("Path '{}' does not exist", join_absolute(&components[..=i]))
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
                    eyre::bail!(
                        "Encountered file while traversing path '{}'",
                        join_absolute(&components[..=i])
                    );
                }
                INodeKind::Symlink => {
                    eyre::bail!(
                        "Encountered a symbolic link while traversing path '{}'",
                        join_absolute(&components[..=i])
                    );
                }
            }
        }

        Ok((inode_addr, inode))
    }

    async fn resolve_parent<P: Into<PathBuf>>(
        &self,
        path: P,
    ) -> eyre::Result<(u64, INode, String)> {
        let path: PathBuf = path.into();
        tracing::debug!("Resolving parent of {path:?}");
        let parent = path.parent().ok_or_else(|| eyre::eyre!("Missing parent"))?;
        let filename = path
            .file_name()
            .ok_or_else(|| eyre::eyre!("Missing filename"))?
            .to_string_lossy()
            .to_string();
        let (addr, inode) = self.resolve_path(parent).await?;
        Ok((addr, inode, filename))
    }

    #[async_recursion(?Send)]
    pub async fn ls<P: Into<PathBuf>>(&self, path: P) -> eyre::Result<Vec<String>> {
        let path: PathBuf = path.into();
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
                tracing::debug!("Trying to list dir entries from symlink");
                let raw_path = self.read_full_data_from_extent(inode.extent_addr).await?;
                let symlink = SymLink::deserialize(&raw_path)?;
                tracing::debug!(
                    " {} *=> {}",
                    normalize_path(&path),
                    normalize_path(&symlink.path)
                );
                if symlink.path == path {
                    tracing::warn!("Invalid fs state: Symlink pointing to itself detected");
                    eyre::bail!("Symlink pointing to itself detected");
                }
                self.ls(symlink.path).await
            }
        }
    }

    pub async fn fcopy<P: Into<PathBuf> + Clone>(
        &self,
        src: P,
        dest: P,
        opt: WriteOption,
    ) -> Result<(), BruteFsError> {
        let data = self.fread(src).await?;
        let (_dst_parent_addr, _dst_parent_inode, _) = self.resolve_parent(dest.clone()).await?;
        self.fwrite(dest, data, opt).await
    }

    pub async fn fmove<P: Into<PathBuf>>(&self, src: P, dest: P) -> eyre::Result<()> {
        let (src_parent_addr, mut src_parent_inode, src_name) = self.resolve_parent(src).await?;
        let (dst_parent_addr, mut dst_parent_inode, dst_name) = self.resolve_parent(dest).await?;

        let src_payload = self
            .read_full_data_from_extent(src_parent_inode.extent_addr)
            .await?;
        let dst_payload = self
            .read_full_data_from_extent(dst_parent_inode.extent_addr)
            .await?;

        // Same parent:
        // we need to be careful updating dir entries as there is only one (psrc = pdst)
        if src_parent_addr == dst_parent_addr {
            tracing::debug!("fmove found same parent for src and dest");
            let mut dir = Directory::deserialize(&src_payload)?;
            if dir.entries.iter().any(|(name, _)| name == &dst_name) {
                eyre::bail!("Destination already exists: {dst_name}");
            }
            let mut found_inode_addr = None;
            dir.entries.retain(|(name, inode_addr)| {
                if name == &src_name {
                    found_inode_addr = Some(*inode_addr);
                    false
                } else {
                    true
                }
            });
            let inode_addr =
                found_inode_addr.ok_or_else(|| eyre::eyre!("Source entry not found"))?;
            dir.entries.push((dst_name, inode_addr));

            let old_extent = src_parent_inode.extent_addr;
            let new_ext = Extent {
                next: MaybeU64::default(),
                data: dir.serialize()?,
            };
            let addr = self.allocate(new_ext.serialized_size()).await?;
            self.ctrl
                .write(addr as usize, &new_ext.serialize()?)
                .await?;
            src_parent_inode.extent_addr = MaybeU64::from(addr);
            self.ctrl
                .write(src_parent_addr as usize, &src_parent_inode.serialize()?)
                .await?;

            self.free_full_extent(old_extent).await?;
            return Ok(());
        }

        let mut src_dir = Directory::deserialize(&src_payload)?;
        let mut dst_dir = Directory::deserialize(&dst_payload)?;
        if dst_dir.entries.iter().any(|(name, _)| name == &dst_name) {
            eyre::bail!("Destination already exists: {dst_name}");
        }
        let mut found_inode_addr = None;
        src_dir.entries.retain(|(name, inode_addr)| {
            if name == &src_name {
                found_inode_addr = Some(*inode_addr);
                false
            } else {
                true
            }
        });

        let inode_addr = found_inode_addr.ok_or_else(|| eyre::eyre!("Source entry not found"))?;
        dst_dir.entries.push((dst_name, inode_addr));
        let old_src = src_parent_inode.extent_addr;
        let old_dst = dst_parent_inode.extent_addr;
        let new_src = Extent {
            next: MaybeU64::default(),
            data: src_dir.serialize()?,
        };
        let new_dst = Extent {
            next: MaybeU64::default(),
            data: dst_dir.serialize()?,
        };

        let src_addr = self.allocate(new_src.serialized_size()).await?;
        self.ctrl
            .write(src_addr as usize, &new_src.serialize()?)
            .await?;
        src_parent_inode.extent_addr = MaybeU64::from(src_addr);

        let dst_addr = self.allocate(new_dst.serialized_size()).await?;
        self.ctrl
            .write(dst_addr as usize, &new_dst.serialize()?)
            .await?;
        dst_parent_inode.extent_addr = MaybeU64::from(dst_addr);

        self.ctrl
            .write(src_parent_addr as usize, &src_parent_inode.serialize()?)
            .await?;
        self.ctrl
            .write(dst_parent_addr as usize, &dst_parent_inode.serialize()?)
            .await?;

        self.free_all([old_src, old_dst]).await?;
        Ok(())
    }

    pub async fn unlink<P: Into<PathBuf>>(&self, path: P) -> eyre::Result<()> {
        let path: PathBuf = path.into();
        let (parent_addr, mut parent_inode, filename) = self.resolve_parent(&path).await?;
        let payload = self
            .read_full_data_from_extent(parent_inode.extent_addr)
            .await?;

        let mut directory = Directory::deserialize(&payload)?;
        let entry_index = directory
            .entries
            .iter()
            .position(|(name, _)| name == &filename)
            .ok_or_else(|| eyre::eyre!("Path does not exist"))?;

        let (_, inode_addr) = directory.entries.remove(entry_index);
        let inode = INode::deserialize(
            &self
                .ctrl
                .read(inode_addr as usize, INode::serialized_size())
                .await?,
        )?;

        let mut maybe_garbages = vec![];
        match inode.kind {
            INodeKind::File | INodeKind::Symlink => {
                self.free_full_extent(inode.extent_addr).await?;
            }
            INodeKind::Directory => {
                let dir_payload = self.read_full_data_from_extent(inode.extent_addr).await?;
                let dir = Directory::deserialize(&dir_payload)?;
                if !dir.entries.is_empty() {
                    eyre::bail!("Directory is not empty");
                }

                maybe_garbages.push(inode.extent_addr);
            }
        }

        tracing::debug!("Rewrite parent directory entries");
        maybe_garbages.push(parent_inode.extent_addr);

        let new_dir_extent = Extent {
            next: MaybeU64::default(),
            data: directory.serialize()?,
        };
        let new_dir_extent_addr = self.allocate(new_dir_extent.serialized_size()).await?;
        self.ctrl
            .write(new_dir_extent_addr as usize, &new_dir_extent.serialize()?)
            .await?;

        parent_inode.extent_addr = MaybeU64::from(new_dir_extent_addr);
        parent_inode.mtime = utc_now_u64();

        self.ctrl
            .write(parent_addr as usize, &parent_inode.serialize()?)
            .await?;

        self.mark_as_reusable(AddressSlot {
            addr: MaybeU64::from(inode_addr),
            capacity: INode::serialized_size(),
        })
        .await?;

        self.free_all(maybe_garbages).await?;
        Ok(())
    }

    async fn blob_write<P: Into<PathBuf>>(
        &self,
        path: P,
        data: Vec<u8>,
        is_symlink: bool,
        opt: WriteOption,
    ) -> Result<(), BruteFsError> {
        let remaining = self.total_remaining_capacity().await?;
        let inp_len = data.len();
        if inp_len > remaining {
            return Err(BruteFsError::from_report(eyre::eyre!(
                "Insufficient space, operation requires {} B more",
                inp_len.saturating_sub(remaining)
            )));
        }

        let path: PathBuf = path.into();
        let (parent_addr, mut parent_inode, filename) = self.resolve_parent(&path).await?;
        let payload = self
            .read_full_data_from_extent(parent_inode.extent_addr)
            .await?;

        let mut directory = Directory::deserialize(&payload)?;
        for (name, inode_addr) in &directory.entries {
            if name == &filename {
                let mut inode = INode::deserialize(
                    &self
                        .ctrl
                        .read(*inode_addr as usize, INode::serialized_size())
                        .await?,
                )?;
                match inode.kind {
                    INodeKind::File | INodeKind::Symlink => {
                        if !opt.overwrite {
                            return Err(BruteFsError::from_report(eyre::eyre!(
                                "File '{name}' already exists"
                            )));
                        }

                        let old_extent_addr = inode.extent_addr;
                        let extent = Extent {
                            next: MaybeU64::default(),
                            data,
                        };
                        let extent_addr = self.allocate(extent.serialized_size()).await?;

                        self.ctrl
                            .write(extent_addr as usize, &extent.serialize()?)
                            .await?;
                        inode.extent_addr = MaybeU64::from(extent_addr);
                        inode.mtime = utc_now_u64();
                        self.ctrl
                            .write(*inode_addr as usize, &inode.serialize()?)
                            .await?;

                        self.free_full_extent(old_extent_addr).await?;

                        return Ok(());
                    }
                    _ => {
                        return Err(BruteFsError::from_report(eyre::eyre!(
                            "Path '{name}' is not file"
                        )));
                    }
                }
            }
        }

        tracing::debug!("Creating new file");
        let file_size = data.len() as u64;
        let extent = Extent {
            next: MaybeU64::default(),
            data,
        };

        let extent_addr = self.allocate(extent.serialized_size()).await?;
        self.ctrl
            .write(extent_addr as usize, &extent.serialize()?)
            .await?;

        // println!("File size {filename} => {file_size} B");

        let inode = INode {
            ctime: utc_now_u64(),
            mtime: utc_now_u64(),
            total_file_size: file_size,
            extent_addr: MaybeU64::from(extent_addr),
            kind: if is_symlink {
                INodeKind::Symlink
            } else {
                INodeKind::File
            },
        };
        let inode_addr = self.allocate(INode::serialized_size()).await?;
        self.ctrl
            .write(inode_addr as usize, &inode.serialize()?)
            .await?;
        directory.entries.push((filename, inode_addr));

        let new_dir_extent = Extent {
            next: MaybeU64::default(),
            data: directory.serialize()?,
        };
        let new_dir_extent_addr = self.allocate(new_dir_extent.serialized_size()).await?;
        self.ctrl
            .write(new_dir_extent_addr as usize, &new_dir_extent.serialize()?)
            .await?;
        let old_parent_extent = parent_inode.extent_addr;
        parent_inode.extent_addr = MaybeU64::from(new_dir_extent_addr);
        parent_inode.mtime = utc_now_u64();

        self.ctrl
            .write(parent_addr as usize, &parent_inode.serialize()?)
            .await?;

        self.free_full_extent(old_parent_extent).await?;

        Ok(())
    }

    pub async fn fwrite<P: Into<PathBuf>>(
        &self,
        path: P,
        data: Vec<u8>,
        opt: WriteOption,
    ) -> Result<(), BruteFsError> {
        self.blob_write(path, data, false, opt).await
    }

    pub async fn fwrite_stream<P, R>(
        &self,
        path: P,
        mut stream: R,
        mut block_size: usize,
        opt: WriteOption,
    ) -> eyre::Result<()>
    where
        P: Into<PathBuf>,
        R: AsyncRead + AsyncSeek + Unpin,
    {
        let path: PathBuf = path.into();
        self.fwrite(&path, vec![], opt).await?;
        let mut buf = vec![0u8; block_size];
        let mut pos = stream.stream_position().await?;
        loop {
            if buf.len() != block_size {
                buf.resize(block_size, 0);
            }

            let n = stream.read(&mut buf).await?;
            if n == 0 {
                break;
            }

            let chunk = &buf[..n];
            match self.fappend(&path, chunk.to_vec()).await {
                Ok(_) => {
                    pos += n as u64;
                }
                Err(e) => {
                    if let BruteFsError::Insufficient { max_slot_size, .. } = e {
                        let new_size = max_slot_size.saturating_sub(16);
                        if new_size == 0 {
                            return Err(e.into());
                        }
                        block_size = new_size;
                        stream.seek(SeekFrom::Start(pos)).await?;
                        continue;
                    }
                    return Err(e.into());
                }
            }
        }

        Ok(())
    }
    pub async fn create_link<P: Into<PathBuf> + Clone>(
        &self,
        path: P,
        content: P,
        opt: WriteOption,
    ) -> eyre::Result<()> {
        let _ = self.resolve_parent(content.clone()).await?;
        self.blob_write(
            path,
            SymLink {
                path: content.into(),
            }
            .serialize(),
            true,
            opt,
        )
        .await?;

        Ok(())
    }

    pub async fn mkdir<P: Into<PathBuf>>(&self, path: P, recursive: bool) -> eyre::Result<bool> {
        let components = path_to_string_list(path);

        let mut created_new = false;
        let (mut curr_addr, mut curr_inode) = self.get_root_inode().await?;
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
            if !recursive && !at_root {
                eyre::bail!("Directory '{component}' does not exist");
            }

            tracing::debug!("Creating new dir inode at {component}");
            let new_inode = INode {
                ctime: utc_now_u64(),
                mtime: utc_now_u64(),
                total_file_size: 0,
                extent_addr: MaybeU64::default(),
                kind: INodeKind::Directory,
            };

            let inode_addr = self.allocate(INode::serialized_size()).await?;
            self.ctrl
                .write(inode_addr as usize, &new_inode.serialize()?)
                .await?;

            created_new = true;
            directory.entries.push((component.clone(), inode_addr));
            let dir_data = directory.serialize()?;

            let extent_addr = self
                .allocate(
                    Extent {
                        next: MaybeU64::default(),
                        data: dir_data,
                    }
                    .serialized_size(),
                )
                .await?;

            self.ctrl
                .write(
                    extent_addr as usize,
                    &Extent {
                        next: MaybeU64::default(),
                        data: directory.serialize()?,
                    }
                    .serialize()?,
                )
                .await?;

            let old_extent_addr = curr_inode.extent_addr;
            curr_inode.extent_addr = MaybeU64::from(extent_addr);
            curr_inode.mtime = utc_now_u64();
            self.ctrl
                .write(curr_addr as usize, &curr_inode.serialize()?)
                .await?;

            self.free_full_extent(old_extent_addr).await?;

            curr_addr = inode_addr;
            curr_inode = new_inode;
        }

        Ok(created_new)
    }

    #[async_recursion(?Send)]
    pub async fn fread<P: Into<PathBuf>>(&self, path: P) -> Result<Vec<u8>, BruteFsError> {
        let path: PathBuf = path.into();
        let (_, inode) = self.resolve_path(&path).await?;
        match inode.kind {
            INodeKind::Directory => Err(BruteFsError::from_report(eyre::eyre!(
                "Cannot fread directory"
            ))),
            INodeKind::File => self.read_full_data_from_extent(inode.extent_addr).await,
            INodeKind::Symlink => {
                tracing::debug!("Trying to read file from symlink");
                let raw_path = self.read_full_data_from_extent(inode.extent_addr).await?;
                let symlink = SymLink::deserialize(&raw_path)?;
                tracing::debug!(
                    " {} *=> {}",
                    normalize_path(&path),
                    normalize_path(&symlink.path)
                );
                if symlink.path == path {
                    tracing::warn!("Invalid fs state: Symlink pointing to itself detected");
                    return Err(BruteFsError::from_report(eyre::eyre!(
                        "Symlink pointing to itself detected"
                    )));
                }
                self.fread(symlink.path).await
            }
        }
    }

    #[async_recursion(?Send)]
    pub async fn fseek<P: Into<PathBuf>>(
        &self,
        path: P,
        start: u64,
        end: u64,
    ) -> eyre::Result<Vec<u8>> {
        let path: PathBuf = path.into();
        let (_, inode) = self.resolve_path(&path).await?;
        match inode.kind {
            INodeKind::File => {
                self.seek_full_data_from_extent(inode.extent_addr, start, end)
                    .await
            }
            INodeKind::Symlink => {
                tracing::debug!("Trying to read+seek file from symlink");
                let raw_path = self.read_full_data_from_extent(inode.extent_addr).await?;
                let symlink = SymLink::deserialize(&raw_path)?;
                tracing::debug!(
                    " {} *=> {}",
                    normalize_path(&path),
                    normalize_path(&symlink.path)
                );
                if symlink.path == path {
                    tracing::warn!("Invalid fs state: Symlink pointing to itself detected");
                    eyre::bail!("Symlink pointing to itself detected");
                }
                self.fseek(symlink.path, start, end).await
            }
            INodeKind::Directory => {
                eyre::bail!("Cannot fread directory");
            }
        }
    }

    #[async_recursion(?Send)]
    pub async fn fread_stream<P: Into<PathBuf>>(
        &self,
        path: P,
        block: u64,
        offset: u64,
    ) -> eyre::Result<Vec<u8>> {
        let path: PathBuf = path.into();
        let (_, inode) = self.resolve_path(&path).await?;
        match inode.kind {
            INodeKind::File => {
                // TODO: advance by block since this is not cheap
                // maybe instead return a stream to be consumed like
                // stream = fread_stream(..).await
                // while let Some(next) =  stream {
                // self.seek_full_data_from_extent(inode.extent_addr, offset, inode.total_file_size)
                //     .await
                todo!()
            }
            INodeKind::Symlink => {
                tracing::debug!("Trying to read+seek file from symlink");
                let raw_path = self.read_full_data_from_extent(inode.extent_addr).await?;
                let symlink = SymLink::deserialize(&raw_path)?;
                tracing::debug!(
                    " {} *=> {}",
                    normalize_path(&path),
                    normalize_path(&symlink.path)
                );
                if symlink.path == path {
                    tracing::warn!("Invalid fs state: Symlink pointing to itself detected");
                    eyre::bail!("Symlink pointing to itself detected");
                }
                self.fread_stream(symlink.path, block, offset).await
            }
            INodeKind::Directory => {
                eyre::bail!("Cannot fread directory");
            }
        }
    }

    // TODO:
    // fprepend?

    #[async_recursion(?Send)]
    pub async fn fappend<P: Into<PathBuf>>(
        &self,
        path: P,
        data: Vec<u8>,
    ) -> Result<(), BruteFsError> {
        let path: PathBuf = path.into();
        let (iaddr, mut inode) = self.resolve_path(&path).await?;
        match inode.kind {
            INodeKind::File => {
                inode.total_file_size += data.len() as u64;
                let new_extent = Extent {
                    next: MaybeU64::default(),
                    data,
                };
                self.append_or_allocate_extent(inode.extent_addr, new_extent)
                    .await?;
                self.update_inode(iaddr, inode).await?;
            }
            INodeKind::Symlink => {
                tracing::debug!("Trying tofappend file from symlink");
                let raw_path = self.read_full_data_from_extent(inode.extent_addr).await?;
                let symlink = SymLink::deserialize(&raw_path)?;
                tracing::debug!(
                    " {} *=> {}",
                    normalize_path(&path),
                    normalize_path(&symlink.path)
                );
                if symlink.path == path {
                    tracing::warn!("Invalid fs state: Symlink pointing to itself detected");
                    bfs_bail!("Symlink pointing to itself detected");
                }
                self.fappend(symlink.path, data).await?;
            }
            INodeKind::Directory => {
                bfs_bail!("Cannot append data to directory");
            }
        };

        Ok(())
    }

    pub async fn update_inode(&self, addr: u64, mut inode: INode) -> eyre::Result<()> {
        inode.mtime = utc_now_u64();
        self.ctrl.write(addr as usize, &inode.serialize()?).await
    }

    pub async fn read_extent(&self, addr: u64) -> Result<Extent, BruteFsError> {
        let extent_header = self.ctrl.read(addr as usize, 8).await?;
        let curr_extent_data_size = u64::from_le_bytes(
            extent_header[0..8]
                .try_into()
                .map_err(BruteFsError::from_error)?,
        );
        let out = Extent::deserialize(
            &self
                .ctrl
                .read(addr as usize, 8 + 8 + curr_extent_data_size as usize)
                .await?,
        )?;
        Ok(out)
    }

    pub async fn read_extent_metadata(&self, addr: u64) -> eyre::Result<(RegionSlot, AddressSlot)> {
        let extent_header = self.ctrl.read(addr as usize, 16).await?;
        let curr_extent_data_size = u64::from_le_bytes(extent_header[0..8].try_into()?);
        let next_extent = MaybeU64::from(u64::from_le_bytes(extent_header[8..16].try_into()?));
        Ok((
            RegionSlot {
                start: MaybeU64::from(addr),
                end: MaybeU64::from(addr + 8 + 8 + curr_extent_data_size as u64),
            },
            AddressSlot {
                addr: next_extent,
                capacity: curr_extent_data_size as usize,
            },
        ))
    }

    pub async fn mark_as_reusable(&self, new_slot_value: AddressSlot) -> eyre::Result<()> {
        tracing::debug!(
            "Marking slot 0x{:x} of size {} as reusable",
            new_slot_value.addr.get(),
            new_slot_value.capacity
        );
        let mut header = self.get_header().await?;
        let mut found = false;
        for slot in header.extent_freed.items.iter_mut() {
            if slot.is_free() {
                *slot = new_slot_value;
                found = true;
                break;
            }
        }

        if found {
            self.update_header(header).await?;
        }

        return Ok(());
    }

    pub async fn exists<P: Into<PathBuf>>(&self, path: P) -> eyre::Result<bool> {
        Ok(self.stats(path).await?.is_some())
    }

    pub async fn stats<P: Into<PathBuf>>(&self, path: P) -> eyre::Result<Option<EntryStat>> {
        let path: PathBuf = path.into();
        let components = path_to_string_list(path.clone());

        if components.is_empty() {
            let (_, inode) = self.get_root_inode().await?;
            return Ok(Some(EntryStat {
                name: "/".to_string(),
                size: None,
                kind: inode.kind,
                mtime: inode.mtime,
                ctime: inode.ctime,
            }));
        }
        let (_, inode) = match self.resolve_path(path).await {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };
        let name = components.last().unwrap().clone();

        Ok(Some(EntryStat {
            name,
            size: match inode.kind {
                INodeKind::Directory => None,
                _ => Some(inode.total_file_size as usize),
            },
            kind: inode.kind,
            mtime: inode.mtime,
            ctime: inode.ctime,
        }))
    }

    pub async fn reusable_regions(&self) -> eyre::Result<Vec<AddressSlot>> {
        let header = self.get_header().await?;
        return Ok(header
            .extent_freed
            .items
            .into_iter()
            .filter(|slot| !slot.is_free() && slot.capacity > 0)
            .collect::<Vec<_>>());
    }

    pub async fn free_full_extent(&self, start_extent_addr: MaybeU64) -> eyre::Result<()> {
        tracing::debug!("Freeing extent at 0x{:x}", start_extent_addr.get());
        let mut addr = start_extent_addr;
        while let Some(next_addr) = addr.to_optional() {
            let extent = self.read_extent(next_addr).await?;

            self.mark_as_reusable(AddressSlot {
                addr: MaybeU64::from(next_addr),
                capacity: extent.data.len(),
            })
            .await?;

            addr = extent.next;
        }

        Ok(())
    }

    pub async fn free_all<A>(&self, addresses: A) -> eyre::Result<()>
    where
        A: IntoIterator<Item = MaybeU64>,
        A::IntoIter: ExactSizeIterator,
    {
        for addr in addresses {
            self.free_full_extent(addr).await?;
        }
        Ok(())
    }

    // useful for extending dir entries
    pub async fn append_or_allocate_extent(
        &self,
        start_extent_addr: MaybeU64,
        new_extent: Extent,
    ) -> Result<u64, BruteFsError> {
        let mut last_extent = None;
        let mut addr = start_extent_addr;
        while let Some(next_addr) = addr.to_optional() {
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
            prev_extent.next = MaybeU64::from(new_extent_addr);
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

    pub async fn read_full_data_from_extent(
        &self,
        mut addr: MaybeU64,
    ) -> Result<Vec<u8>, BruteFsError> {
        let mut data = vec![];
        while let Some(next_addr) = addr.to_optional() {
            tracing::debug!("Resolving extent 0x{next_addr:x}");
            let extent = self.read_extent(next_addr).await?;
            addr = extent.next;
            data.extend(extent.data);
        }
        Ok(data)
    }

    pub async fn find_full_extent_metadata(
        &self,
        mut addr: MaybeU64,
        stop: Option<u32>,
    ) -> eyre::Result<Vec<RegionSlot>> {
        let mut blocks = vec![];
        let max = stop.unwrap_or(u32::MAX);
        let mut i = 1;
        while let Some(next_addr) = addr.to_optional() {
            if i - 1 >= max {
                break;
            }
            tracing::debug!("Resolving {i}-th extent metadata 0x{next_addr:x}");
            let (block_slot, addr_slot) = self.read_extent_metadata(next_addr).await?;
            addr = addr_slot.addr;
            blocks.push(block_slot);
            i += 1;
        }
        Ok(blocks)
    }

    pub async fn seek_full_data_from_extent(
        &self,
        addr: MaybeU64,
        addr_start: u64,
        addr_end: u64,
    ) -> eyre::Result<Vec<u8>> {
        let mut buf = vec![];
        let mut cursor: u64 = 0;
        let mut addr = addr;

        while let Some(next_addr) = addr.to_optional() {
            let extent = self.read_extent(next_addr).await?;
            addr = extent.next;
            let data = extent.data;
            let extent_start = cursor;
            let extent_end = cursor + data.len() as u64;

            if extent_end <= addr_start {
                cursor = extent_end;
                continue;
            }
            if extent_start >= addr_end {
                break;
            }

            let start_in_ext = addr_start.saturating_sub(extent_start) as usize;
            let end_in_ext = (addr_end.saturating_sub(extent_start) as usize).min(data.len());
            if start_in_ext < data.len() && start_in_ext < end_in_ext {
                buf.extend_from_slice(&data[start_in_ext..end_in_ext]);
            }
            cursor = extent_end;
        }

        Ok(buf)
    }
}
