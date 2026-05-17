use crate::{
    bfs::{addr::MaybeU64, crypto::Crypto, ds::*},
    device::disk::Controller,
    utils::*,
};
use async_recursion::async_recursion;
use eyre::{Context, ContextCompat};
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
    static_format: Format,
    geometry: GeometryLayout,
    pub ctrl: Controller,
}

#[derive(Debug, Clone, Default)]
pub struct WriteOption {
    pub overwrite: bool,
}

impl BruteFS {
    pub async fn from_formatted(ctrl: Controller, password: Option<String>) -> eyre::Result<Self> {
        let header_size = BruteFsHeader::template().serialize()?.len();
        let mut bfs = Self {
            header_size,
            ctrl,
            alloc_guard: Mutex::new(()),
            static_format: Format {
                block_size_bytes: 0,
                blocks_per_group: 0,
                group_count: 0,
            },
            geometry: Default::default(),
        };

        let header = bfs.get_header().await?;
        let total_bytes = bfs
            .ctrl
            .total_capacity()
            .ok_or_else(|| eyre::eyre!("Failed calculating total capacity"))?;

        bfs.geometry = header.calculate_relative_geometry()?.0;
        bfs.static_format = header.format;
        bfs.static_format
            .validate(total_bytes.saturating_sub(header_size) as u64)?;

        if let Some(password) = password {
            bfs.ctrl
                .setup_crypto(Crypto::new(&password, header.chacha20_nonce));
        }
        bfs.ensure_headers().await?;

        Ok(bfs)
    }

    pub async fn format_new(mut ctrl: Controller, password: Option<String>) -> eyre::Result<Self> {
        let mut header = BruteFsHeader::template();
        header.chacha20_nonce = Crypto::gen_nonce();
        if let Some(password) = &password {
            ctrl.setup_crypto(Crypto::new(password, header.chacha20_nonce));
        }

        let total_capacity = ctrl
            .total_capacity()
            .ok_or_else(|| eyre::eyre!("Failed calculating total capacity"))?
            as u64;
        header.format = Format::infer_from_free_space(total_capacity);

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

        Self::from_formatted(ctrl, password).await
    }

    pub fn resolve_inode_addr(&self, inumber: u64) -> Option<AddressSlot> {
        if let Some(group) = GroupLayout::derive_from_inode(inumber, &self.geometry) {
            let local_inode_index = (inumber - 1) % self.geometry.n_inodes_in_group;
            let start_inode_table_addr =
                group.g_offset + self.geometry.rel_inode_table_region.start.get();
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
        if let Some((slot, group)) = self
            .resolve_inode_addr(inumber)
            .zip(GroupLayout::derive_from_inode(inumber, &self.geometry))
        {
            let bitmap_slot = group.inode_bitmap_region.to_addr_slot();
            let raw_bitmap = self
                .ctrl
                .raw_read(bitmap_slot.addr.into(), bitmap_slot.capacity)
                .await?;
            let bitmap = Bitmap::deserialize(&raw_bitmap)?;

            let inode_index_in_group = (inumber - 1) % self.geometry.n_inodes_in_group;
            if !bitmap
                .get(inode_index_in_group as usize)
                .wrap_err("Resolving INode")?
            {
                return Ok(None);
            }

            let raw_inode = self.ctrl.raw_read(slot.addr.into(), slot.capacity).await?;

            return Some(INode::deserialize(&raw_inode)).transpose();
        }

        Ok(None)
    }

    async fn register_node(&self, inode: &INode, overwrite: bool) -> eyre::Result<()> {
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

    pub async fn format_headers_report(&self) -> eyre::Result<String> {
        let mut out = String::new();

        let header = self.get_header().await?;
        out.push_str(&format!("brutefs version: {}\n", header.version));

        let capacity = self.total_capacity()?;
        let rem_capacity = self.total_remaining_capacity().await?;
        out.push_str(&format!("Capacity:  {capacity:>10} B\n"));
        out.push_str(&format!("Remaining: {rem_capacity:>10} B\n"));
        out.push_str(&format!("{}\n", self.static_format));
        out.push_str(&format!("{}\n", self.geometry));

        Ok(out)
    }

    pub async fn ensure_headers(&self) -> eyre::Result<()> {
        tracing::debug!("{}", self.format_headers_report().await?);
        Ok(())
    }

    pub async fn get_header(&self) -> eyre::Result<BruteFsHeader> {
        BruteFsHeader::deserialize(&self.ctrl.raw_read(0, self.header_size).await?)
    }

    pub async fn update_header(&self, header: BruteFsHeader) -> eyre::Result<()> {
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
        tracing::debug!("Resolving {path:?}");
        let components = path_to_string_list(path);
        let mut inode = self.get_root_inode().await?;

        for (i, component) in components.iter().enumerate() {
            match inode.kind {
                INodeKind::Directory => {
                    tracing::debug!(" > Enter dir {component}");
                    let payload = self.read_full_data_from_extent(inode.extent_addr).await?;
                    let directory = Directory::deserialize(&payload)?;

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
                INodeKind::Symlink => {
                    // IDEA: impl symlink resolution traversal loop substitution here
                    // resolving a link should resolve into its containing path?
                    eyre::bail!(
                        "Symbolic links are not yet supported during traversal at '{}'",
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

    #[async_recursion(?Send)]
    pub async fn ls<P: Into<PathBuf>>(&self, path: P) -> eyre::Result<Vec<String>> {
        let path: PathBuf = path.into();
        let inode = self.resolve_path(&path).await?;
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
        let (_dst_parent_inode, _) = self.resolve_parent(dest.clone()).await?;
        self.fwrite(dest, data, opt).await
    }

    #[allow(unused)]
    pub async fn fmove<P: Into<PathBuf>>(&self, src: P, dest: P) -> eyre::Result<()> {
        // let (mut src_parent_inode, src_name) = self.resolve_parent(src).await?;
        // let (mut dst_parent_inode, dst_name) = self.resolve_parent(dest).await?;

        // let src_payload = self
        //     .read_full_data_from_extent(src_parent_inode.extent_addr)
        //     .await?;
        // let dst_payload = self
        //     .read_full_data_from_extent(dst_parent_inode.extent_addr)
        //     .await?;

        // // Same parent:
        // // we need to be careful updating dir entries as there is only one (psrc = pdst)
        // if src_parent_addr == dst_parent_addr {
        //     tracing::debug!("fmove found same parent for src and dest");
        //     let mut dir = Directory::deserialize(&src_payload)?;
        //     if dir.entries.iter().any(|(name, _)| name == &dst_name) {
        //         eyre::bail!("Destination already exists: {dst_name}");
        //     }
        //     let mut found_inode_addr = None;
        //     dir.entries.retain(|(name, inode_addr)| {
        //         if name == &src_name {
        //             found_inode_addr = Some(*inode_addr);
        //             false
        //         } else {
        //             true
        //         }
        //     });
        //     let inode_addr =
        //         found_inode_addr.ok_or_else(|| eyre::eyre!("Source entry not found"))?;
        //     dir.entries.push((dst_name, inode_addr));

        //     let old_extent = src_parent_inode.extent_addr;
        //     let new_ext = Extent {
        //         next: MaybeU64::default(),
        //         data: dir.serialize()?,
        //     };
        //     let addr = self.allocate(new_ext.serialized_size()).await?;
        //     self.ctrl
        //         .write(addr as usize, &new_ext.serialize()?)
        //         .await?;
        //     src_parent_inode.extent_addr = MaybeU64::from(addr);
        //     self.ctrl
        //         .write(src_parent_addr as usize, &src_parent_inode.serialize()?)
        //         .await?;

        //     self.free_full_extent(old_extent).await?;
        //     return Ok(());
        // }

        // let mut src_dir = Directory::deserialize(&src_payload)?;
        // let mut dst_dir = Directory::deserialize(&dst_payload)?;
        // if dst_dir.entries.iter().any(|(name, _)| name == &dst_name) {
        //     eyre::bail!("Destination already exists: {dst_name}");
        // }
        // let mut found_inode_addr = None;
        // src_dir.entries.retain(|(name, inode_addr)| {
        //     if name == &src_name {
        //         found_inode_addr = Some(*inode_addr);
        //         false
        //     } else {
        //         true
        //     }
        // });

        // let inode_addr = found_inode_addr.ok_or_else(|| eyre::eyre!("Source entry not found"))?;
        // dst_dir.entries.push((dst_name, inode_addr));
        // let old_src = src_parent_inode.extent_addr;
        // let old_dst = dst_parent_inode.extent_addr;
        // let new_src = Extent {
        //     next: MaybeU64::default(),
        //     data: src_dir.serialize()?,
        // };
        // let new_dst = Extent {
        //     next: MaybeU64::default(),
        //     data: dst_dir.serialize()?,
        // };

        // let src_addr = self.allocate(new_src.serialized_size()).await?;
        // self.ctrl
        //     .write(src_addr as usize, &new_src.serialize()?)
        //     .await?;
        // src_parent_inode.extent_addr = MaybeU64::from(src_addr);

        // let dst_addr = self.allocate(new_dst.serialized_size()).await?;
        // self.ctrl
        //     .write(dst_addr as usize, &new_dst.serialize()?)
        //     .await?;
        // dst_parent_inode.extent_addr = MaybeU64::from(dst_addr);

        // self.ctrl
        //     .write(src_parent_addr as usize, &src_parent_inode.serialize()?)
        //     .await?;
        // self.ctrl
        //     .write(dst_parent_addr as usize, &dst_parent_inode.serialize()?)
        //     .await?;

        // self.free_all([old_src, old_dst]).await?;
        Ok(())
    }

    #[allow(unused)]
    pub async fn unlink<P: Into<PathBuf>>(&self, path: P) -> eyre::Result<()> {
        // let path: PathBuf = path.into();
        // let (mut parent_inode, filename) = self.resolve_parent(&path).await?;
        // let payload = self
        //     .read_full_data_from_extent(parent_inode.extent_addr)
        //     .await?;

        // let mut directory = Directory::deserialize(&payload)?;
        // let entry_index = directory
        //     .entries
        //     .iter()
        //     .position(|(name, _)| name == &filename)
        //     .ok_or_else(|| eyre::eyre!("Path does not exist"))?;

        // let (_, inode_addr) = directory.entries.remove(entry_index);
        // let inode = INode::deserialize(
        //     &self
        //         .ctrl
        //         .read(inode_addr as usize, INode::serialized_size())
        //         .await?,
        // )?;

        // let mut maybe_garbages = vec![];
        // match inode.kind {
        //     INodeKind::File | INodeKind::Symlink => {
        //         self.free_full_extent(inode.extent_addr).await?;
        //     }
        //     INodeKind::Directory => {
        //         let dir_payload = self.read_full_data_from_extent(inode.extent_addr).await?;
        //         let dir = Directory::deserialize(&dir_payload)?;
        //         if !dir.entries.is_empty() {
        //             eyre::bail!("Directory is not empty");
        //         }

        //         maybe_garbages.push(inode.extent_addr);
        //     }
        // }

        // tracing::debug!("Rewrite parent directory entries");
        // maybe_garbages.push(parent_inode.extent_addr);

        // let new_dir_extent = Extent {
        //     next: MaybeU64::default(),
        //     data: directory.serialize()?,
        // };
        // let new_dir_extent_addr = self.allocate(new_dir_extent.serialized_size()).await?;
        // self.ctrl
        //     .write(new_dir_extent_addr as usize, &new_dir_extent.serialize()?)
        //     .await?;

        // parent_inode.extent_addr = MaybeU64::from(new_dir_extent_addr);
        // parent_inode.mtime = utc_now_u64();

        // self.ctrl
        //     .write(parent_addr as usize, &parent_inode.serialize()?)
        //     .await?;

        // self.mark_as_reusable(AddressSlot {
        //     addr: MaybeU64::from(inode_addr),
        //     capacity: INode::serialized_size(),
        // })
        // .await?;

        // self.free_all(maybe_garbages).await?;
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
        let (mut parent_inode, filename) = self.resolve_parent(&path).await?;
        let payload = self
            .read_full_data_from_extent(parent_inode.extent_addr)
            .await?;

        let mut directory = Directory::deserialize(&payload)?;
        for (name, inumber) in &directory.entries {
            if name == &filename {
                let mut inode = self
                    .resolve_inode(*inumber)
                    .await?
                    .ok_or_else(|| eyre::eyre!("Resolving INode for direntry '{name}'"))?;
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
                        self.update_inode(inode).await?;

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
            inumber: 4,
            nlink: 42,
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

        self.update_inode(parent_inode).await?;

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

    #[allow(unused)]
    pub async fn mkdir<P: Into<PathBuf>>(&self, path: P, recursive: bool) -> eyre::Result<bool> {
        let components = path_to_string_list(path);
        todo!()
    }

    #[async_recursion(?Send)]
    pub async fn fread<P: Into<PathBuf>>(&self, path: P) -> Result<Vec<u8>, BruteFsError> {
        let path: PathBuf = path.into();
        let inode = self.resolve_path(&path).await?;
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
        let inode = self.resolve_path(&path).await?;
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

    // TODO:
    // fprepend?

    #[async_recursion(?Send)]
    pub async fn fappend<P: Into<PathBuf>>(
        &self,
        path: P,
        data: Vec<u8>,
    ) -> Result<(), BruteFsError> {
        let path: PathBuf = path.into();
        let mut inode = self.resolve_path(&path).await?;
        match inode.kind {
            INodeKind::File => {
                inode.total_file_size += data.len() as u64;
                let new_extent = Extent {
                    next: MaybeU64::default(),
                    data,
                };
                self.append_or_allocate_extent(inode.extent_addr, new_extent)
                    .await?;
                self.update_inode(inode).await?;
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

    pub async fn update_inode(&self, mut inode: INode) -> eyre::Result<()> {
        inode.mtime = utc_now_u64();
        self.register_node(&inode, true).await
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
        todo!()
    }

    pub async fn exists<P: Into<PathBuf>>(&self, path: P) -> eyre::Result<bool> {
        Ok(self.stats(path).await?.is_some())
    }

    pub async fn stats<P: Into<PathBuf>>(&self, path: P) -> eyre::Result<Option<EntryStat>> {
        let path: PathBuf = path.into();
        let components = path_to_string_list(path.clone());

        if components.is_empty() {
            let inode = self.get_root_inode().await?;
            return Ok(Some(EntryStat {
                name: "/".to_string(),
                size: None,
                kind: inode.kind,
                mtime: inode.mtime,
                ctime: inode.ctime,
            }));
        }
        let inode = match self.resolve_path(path).await {
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

    #[allow(unused)]
    pub async fn allocate(&self, wanted_size: usize) -> Result<u64, BruteFsError> {
        tracing::debug!("Trying to allocate {wanted_size} B");
        let _guard = self.alloc_guard.lock().await;

        //  BruteFsError::Insufficient {
        //     wanted: usize,
        //     max_slot_size: usize,
        //     min_slot_size: usize,
        // }
        let mut max_slot_size = u32::MIN;
        let mut max_slot_size = u32::MAX;
        for g_index in 0..self.static_format.group_count {
            let group =
                GroupLayout::derive_from_group_index(g_index, &self.geometry).ok_or_else(|| {
                    eyre::eyre!("Failed calculating group layout for index {g_index}")
                })?;

            let slot = group.data_bitmap_region.to_addr_slot();
            let raw_bitmap = self.ctrl.raw_read(slot.addr.into(), slot.capacity).await?;
            let bitmap = Bitmap::deserialize(&raw_bitmap)?;

            // for i in 0..self.geometry.usable_blocks_per_group as usize {
            //     if !bitmap
            //         .get(i)
            //         .wrap_err("Parsing block map allocation state")?
            //     {
            //         free_blocks += 1;
            //     }
            // }
        }
        todo!()
    }

    pub async fn free_full_extent(&self, start_extent_addr: MaybeU64) -> eyre::Result<()> {
        tracing::debug!("Freeing extent at 0x{:x}", start_extent_addr.get());
        todo!()
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

    // TODO:
    // can be streamed
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
