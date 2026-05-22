use bytes::Bytes;
use dav_server::{
    DavHandler,
    davpath::DavPath,
    fakels::FakeLs,
    fs::{
        DavDirEntry, DavFile, DavMetaData, FsError, FsFuture, FsResult, FsStream,
        GuardedFileSystem, OpenOptions, ReadDirMeta,
    },
};
use futures::{FutureExt, StreamExt, stream};
use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use std::{
    convert::Infallible,
    fmt::Debug,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{net::TcpListener, sync::Mutex};
use xhfs_core::xhfs::{
    XHFS,
    ds::{EntryStat, INodeKind},
};

#[derive(Debug, Clone)]
struct XHFSDirEntry(EntryStat);

#[derive(Debug, Clone)]
pub struct XHFSMetaData(EntryStat);

impl DavMetaData for XHFSMetaData {
    fn len(&self) -> u64 {
        self.0.size.unwrap_or(0) as u64
    }

    fn is_dir(&self) -> bool {
        matches!(self.0.kind, INodeKind::Directory)
    }

    fn modified(&self) -> FsResult<SystemTime> {
        Ok(UNIX_EPOCH + Duration::from_secs(self.0.mtime))
    }

    fn created(&self) -> FsResult<SystemTime> {
        Ok(UNIX_EPOCH + Duration::from_secs(self.0.ctime))
    }
}

impl DavDirEntry for XHFSDirEntry {
    fn name(&self) -> Vec<u8> {
        self.0.name.as_bytes().to_vec()
    }

    fn metadata(&self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        async move {
            let meta = XHFSMetaData(self.0.clone());
            Ok(Box::new(meta) as Box<dyn DavMetaData>)
        }
        .boxed()
    }
}

pub struct XHFSFile {
    xhfs: Arc<XHFS>,
    path: PathBuf,
    offset: Mutex<u64>,
    chunk_size: usize,
}

impl std::fmt::Debug for XHFSFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let current_pos = self.offset.try_lock().map(|g| *g).unwrap_or(0);
        f.debug_struct("XHFSFile")
            .field("path", &self.path)
            .field("current_offset", &current_pos)
            .finish()
    }
}

impl DavFile for XHFSFile {
    fn read_bytes(&mut self, count: usize) -> FsFuture<'_, Bytes> {
        let xhfs = self.xhfs.clone();
        let path = self.path.clone();
        let chunk_size = self.chunk_size;
        tracing::info!("Reading bytes {count}");
        async move {
            let current_pos = {
                let offset_guard = self.offset.lock().await;
                *offset_guard
            };

            let mut stream = xhfs.fread_stream(path, chunk_size).await.map_err(|e| {
                tracing::error!("Error pulling chunk stream from block layer: {e:?}");
                FsError::NotFound
            })?;

            let mut skip_remaining = current_pos;
            let mut collected_bytes = vec![];
            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        let chunk_len = chunk.len() as u64;

                        if skip_remaining >= chunk_len {
                            skip_remaining -= chunk_len;
                            continue;
                        }

                        let start_idx = skip_remaining as usize;
                        skip_remaining = 0;

                        let available_in_chunk = chunk.len() - start_idx;
                        let needed = count - collected_bytes.len();
                        let take_len = std::cmp::min(available_in_chunk, needed);

                        collected_bytes.extend_from_slice(&chunk[start_idx..start_idx + take_len]);

                        if collected_bytes.len() >= count {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::error!("Underlying block device read failed: {e:?}");
                        return Err(FsError::GeneralFailure);
                    }
                }
            }

            {
                let mut offset_guard = self.offset.lock().await;
                *offset_guard += collected_bytes.len() as u64;
            }

            Ok(Bytes::from(collected_bytes))
        }
        .boxed()
    }

    fn seek(&mut self, pos: std::io::SeekFrom) -> FsFuture<'_, u64> {
        let xhfs = self.xhfs.clone();
        let path = self.path.clone();
        async move {
            let mut offset_guard = self.offset.lock().await;
            match pos {
                std::io::SeekFrom::Start(n) => {
                    *offset_guard = n;
                    Ok(n)
                }
                std::io::SeekFrom::Current(n) => {
                    let new_pos = (*offset_guard as i64).saturating_add(n);
                    if new_pos < 0 {
                        return Err(FsError::GeneralFailure);
                    }
                    *offset_guard = new_pos as u64;
                    Ok(*offset_guard)
                }
                std::io::SeekFrom::End(n) => match xhfs.stats(path, true).await {
                    Ok(Some(entry_stat)) => {
                        let total_size = entry_stat.size.unwrap_or(0) as i64;
                        let new_pos = total_size.saturating_add(n);
                        if new_pos < 0 {
                            return Err(FsError::GeneralFailure);
                        }
                        *offset_guard = new_pos as u64;
                        Ok(*offset_guard)
                    }
                    _ => Err(FsError::NotFound),
                },
            }
        }
        .boxed()
    }

    fn metadata(&mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        let xhfs = self.xhfs.clone();
        let path = self.path.clone();

        async move {
            match xhfs.stats(path, true).await {
                Ok(Some(stat)) => Ok(Box::new(XHFSMetaData(stat)) as Box<dyn DavMetaData>),
                Ok(None) => Err(FsError::NotFound),
                Err(err) => {
                    tracing::error!("File metadata fetch failed: {err:?}");
                    Err(FsError::GeneralFailure)
                }
            }
        }
        .boxed()
    }

    fn flush(&mut self) -> FsFuture<'_, ()> {
        async move { Ok(()) }.boxed()
    }

    fn write_buf(&'_ mut self, _buf: Box<dyn bytes::Buf + Send>) -> FsFuture<'_, ()> {
        async move { Err(FsError::Forbidden) }.boxed()
    }

    fn write_bytes(&'_ mut self, _buf: bytes::Bytes) -> FsFuture<'_, ()> {
        async move { Err(FsError::Forbidden) }.boxed()
    }

    fn redirect_url(&'_ mut self) -> FsFuture<'_, Option<String>> {
        std::future::ready(Ok(None)).boxed()
    }
}

#[derive(Clone)]
pub struct XHFSAdapter {
    pub xhfs: Arc<XHFS>,
    pub chunk_size: usize,
}

impl GuardedFileSystem<()> for XHFSAdapter {
    fn open<'a>(
        &'a self,
        path: &'a DavPath,
        _options: OpenOptions,
        _creds: &'a (),
    ) -> FsFuture<'a, Box<dyn DavFile>> {
        let xhfs = self.xhfs.clone();
        let path_buf = path.as_pathbuf();
        // if path_buf.is_absolute() {
        //     path_buf = path_buf
        //         .strip_prefix("/")
        //         .unwrap_or(&path_buf)
        //         .to_path_buf();
        // }

        let chunk_size = self.chunk_size;
        async move {
            tracing::info!("WebDAV open {path_buf:?}");
            match xhfs.stats(path_buf.clone(), true).await {
                Ok(Some(stat)) => {
                    if matches!(stat.kind, INodeKind::Directory) {
                        tracing::warn!("Attempted to open directory {:?}", path_buf);
                        return Err(FsError::Forbidden);
                    }

                    let file_handle = XHFSFile {
                        xhfs,
                        path: path_buf,
                        offset: tokio::sync::Mutex::new(0),
                        chunk_size,
                    };

                    Ok(Box::new(file_handle) as Box<dyn DavFile>)
                }

                Ok(None) => {
                    tracing::warn!("File not found {:?}", path_buf);
                    Err(FsError::NotFound)
                }

                Err(err) => {
                    tracing::error!("Metadata lookup failed {:?}: {:?}", path_buf, err);
                    Err(FsError::GeneralFailure)
                }
            }
        }
        .boxed()
    }

    fn metadata<'a>(
        &'a self,
        path: &'a DavPath,
        _creds: &'a (),
    ) -> FsFuture<'a, Box<dyn DavMetaData>> {
        let xhfs = self.xhfs.clone();
        let path_buf = path.as_pathbuf();
        async move {
            match xhfs.stats(path_buf, true).await {
                Ok(Some(entry_stat)) => {
                    let meta = XHFSMetaData(entry_stat);
                    Ok(Box::new(meta) as Box<dyn DavMetaData>)
                }
                Ok(None) => Err(FsError::NotFound),
                Err(e) => {
                    tracing::error!("Backend metadata lookup failure: {e:?}");
                    Err(FsError::GeneralFailure)
                }
            }
        }
        .boxed()
    }

    fn read_dir<'a>(
        &'a self,
        path: &'a DavPath,
        _meta: ReadDirMeta,
        _creds: &'a (),
    ) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
        // TODO:
        // stream dir listing later in core
        let xhfs = self.xhfs.clone();
        let path_buf = path.as_pathbuf();
        async move {
            if path.file_name_bytes() == b".localized" {
                return Err(FsError::NotFound);
            }
            let item_names = xhfs.ls(path_buf.clone()).await.map_err(|e| {
                tracing::error!("Failed listing path collection targets: {e:?}");
                FsError::NotFound
            })?;

            let mut entries: Vec<Box<dyn DavDirEntry>> = vec![];
            for name in item_names {
                let mut entry_path = path_buf.clone();
                entry_path.push(&name);
                if let Ok(Some(entry_stat)) = xhfs.stats(entry_path, true).await {
                    entries.push(Box::new(XHFSDirEntry(entry_stat)) as Box<dyn DavDirEntry>);
                }
            }

            let stream_items = entries.into_iter().map(Ok);
            Ok(Box::pin(stream::iter(stream_items)) as FsStream<Box<dyn DavDirEntry>>)
        }
        .boxed()
    }

    fn symlink_metadata<'a>(
        &'a self,
        path: &'a DavPath,
        _creds: &'a (),
    ) -> FsFuture<'a, Box<dyn DavMetaData>> {
        // self.metadata(path, credentials)
        todo!("symlink")
    }

    fn create_dir<'a>(&'a self, path: &'a DavPath, _creds: &'a ()) -> FsFuture<'a, ()> {
        let xhfs = self.xhfs.clone();
        let path_buf = path.as_pathbuf();

        // FIXME:
        // https://doc.rust-lang.org/nomicon/hrtb.html

        // let handle = tokio::spawn(async move {
        //     xhfs.mkdir(path_buf, true).await.map_err(|err| {
        //         tracing::error!("mkdir failed: {:?}", err);
        //         FsError::GeneralFailure
        //     });
        // });

        // async move {
        //     // xhfs.mkdir(path_buf, true).await.map_err(|err| {
        //     //     tracing::error!("mkdir failed: {:?}", err);
        //     //     FsError::GeneralFailure
        //     // })?;

        //     Ok(())
        // }
        // .boxed()
        todo!("create_dir")
    }

    fn remove_dir<'a>(&'a self, path: &'a DavPath, _creds: &'a ()) -> FsFuture<'a, ()> {
        // FIXME:
        // https://doc.rust-lang.org/nomicon/hrtb.html
        todo!("remove_dir")
    }

    fn remove_file<'a>(&'a self, path: &'a DavPath, _creds: &'a ()) -> FsFuture<'a, ()> {
        todo!("remove_file")
    }

    fn rename<'a>(
        &'a self,
        from: &'a DavPath,
        to: &'a DavPath,
        _creds: &'a (),
    ) -> FsFuture<'a, ()> {
        todo!("rename_file")
    }

    fn copy<'a>(&'a self, from: &'a DavPath, to: &'a DavPath, _creds: &'a ()) -> FsFuture<'a, ()> {
        todo!("copy")
    }

    fn have_props<'a>(
        &'a self,
        path: &'a DavPath,
        _creds: &'a (),
    ) -> std::pin::Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(std::future::ready(false))
    }

    fn patch_props<'a>(
        &'a self,
        path: &'a DavPath,
        dprop: Vec<(bool, dav_server::fs::DavProp)>,
        _creds: &'a (),
    ) -> FsFuture<'a, Vec<(hyper::StatusCode, dav_server::fs::DavProp)>> {
        tracing::warn!("Patch prop {path} {dprop:?}");
        async move { Err(FsError::Forbidden) }.boxed()
    }

    fn get_props<'a>(
        &'a self,
        path: &'a DavPath,
        b: bool,
        _creds: &'a (),
    ) -> FsFuture<'a, Vec<dav_server::fs::DavProp>> {
        tracing::warn!("Get props (plural) {path} {b}");
        async move { Err(FsError::Forbidden) }.boxed()
    }

    fn get_prop<'a>(
        &'a self,
        path: &'a DavPath,
        dprop: dav_server::fs::DavProp,
        _creds: &'a (),
    ) -> FsFuture<'a, Vec<u8>> {
        tracing::warn!("Get props {path} {dprop:?}");
        async move { Err(FsError::Forbidden) }.boxed()
    }

    fn get_quota<'a>(&'a self, _creds: &'a ()) -> FsFuture<'a, (u64, Option<u64>)> {
        let xhfs = self.xhfs.clone();
        async move {
            let total_capacity = xhfs.total_capacity().map_err(|e| {
                tracing::error!("Failed listing path collection targets: {e:?}");
                FsError::NotFound
            })?;
            let remaining = xhfs.total_remaining_capacity().await.map_err(|e| {
                tracing::error!("Failed listing path collection targets: {e:?}");
                FsError::NotFound
            })?;
            Ok((total_capacity as u64, Some(remaining as u64)))
        }
        .boxed()
    }
}

pub async fn webdav_main(addr: String, port: u16, xhfs_instance: Arc<XHFS>) -> eyre::Result<()> {
    let addr: SocketAddr = format!("{addr}:{port}").parse()?;
    let adapter = XHFSAdapter {
        xhfs: xhfs_instance,
        chunk_size: 64 * 1024, // standard 64KB disk read windows
    };

    let dav_server = DavHandler::builder()
        .filesystem(Box::new(adapter))
        .locksystem(FakeLs::new())
        .build_handler();

    let listener = TcpListener::bind(addr).await.unwrap();
    println!("Listening for WebDAV requests at {addr}");
    loop {
        let (stream, _) = listener.accept().await.unwrap();
        let dav_server = dav_server.clone();
        let io = TokioIo::new(stream);

        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .serve_connection(
                    io,
                    service_fn({
                        move |req| {
                            let dav_server = dav_server.clone();
                            async move { Ok::<_, Infallible>(dav_server.handle(req).await) }
                        }
                    }),
                )
                .await
            {
                eprintln!("Error executing HTTP session processing context: {err:?}");
            }
        });
    }
}
