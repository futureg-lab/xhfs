use bytes::{Buf, Bytes};
use dav_server::{
    DavHandler,
    davpath::DavPath,
    fakels::FakeLs,
    fs::{
        DavDirEntry, DavFile, DavMetaData, FsError, FsFuture, FsResult, FsStream,
        GuardedFileSystem, OpenOptions, ReadDirMeta,
    },
};
use eyre::Context;
use futures::{FutureExt, stream};
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
use tokio::net::TcpListener;
use xhfs_core::xhfs::{
    WriteOption, XHFS,
    ds::{EntryStat, INodeKind, XHFSError},
};

macro_rules! read_only_guard {
    ($inp:expr) => {
        if $inp {
            return std::future::ready(Err(FsError::Forbidden)).boxed();
        }
    };
}
macro_rules! read_only_guard_raw {
    ($inp:expr) => {
        if $inp {
            return Err(FsError::Forbidden);
        }
    };
}

struct EConv(XHFSError);
impl From<EConv> for FsError {
    fn from(value: EConv) -> Self {
        tracing::error!("XHFS error occured: {}", value.0);
        match value.0 {
            XHFSError::Insufficient { .. } => FsError::TooLarge,
            XHFSError::Error { .. } => FsError::GeneralFailure,
        }
    }
}

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
    offset: u64,
    chunk_size: usize,
    read_only: bool,
    estat: EntryStat,
    write_buffer: Vec<u8>,
}

impl std::fmt::Debug for XHFSFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XHFSFile")
            .field("path", &self.path)
            .field("current_offset", &self.offset)
            .finish()
    }
}

impl DavFile for XHFSFile {
    fn read_bytes(&mut self, count: usize) -> FsFuture<'_, Bytes> {
        let xhfs = self.xhfs.clone();
        let path = self.path.clone();
        async move {
            if !self.write_buffer.is_empty() {
                let flush_buf = std::mem::take(&mut self.write_buffer);
                xhfs.fappend(&path, flush_buf, None).await.map_err(EConv)?;
                self.write_buffer = Vec::with_capacity(self.chunk_size);
            }
            let start = self.offset;
            let end = start + count as u64;
            let data = xhfs.fseek(&path, start, end).await.map_err(|e| {
                tracing::error!("Read failed: {e:#}");
                FsError::GeneralFailure
            })?;
            self.offset += data.len() as u64;

            Ok(Bytes::from(data))
        }
        .boxed()
    }

    fn seek(&mut self, pos: std::io::SeekFrom) -> FsFuture<'_, u64> {
        async move {
            match pos {
                std::io::SeekFrom::Start(n) => {
                    self.offset = n;
                    Ok(n)
                }
                std::io::SeekFrom::Current(n) => {
                    let new_pos = (self.offset as i64).saturating_add(n);
                    if new_pos < 0 {
                        return Err(FsError::GeneralFailure);
                    }
                    self.offset = new_pos as u64;
                    Ok(self.offset)
                }
                std::io::SeekFrom::End(n) => {
                    let total_size = self.estat.size.unwrap_or(0) as i64;
                    let new_pos = total_size.saturating_add(n);
                    if new_pos < 0 {
                        return Err(FsError::GeneralFailure);
                    }
                    self.offset = new_pos as u64;
                    Ok(self.offset)
                }
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
                    tracing::error!("File metadata fetch failed: {err:#}");
                    Err(FsError::GeneralFailure)
                }
            }
        }
        .boxed()
    }

    fn write_bytes(&'_ mut self, buf: Bytes) -> FsFuture<'_, ()> {
        read_only_guard!(self.read_only);
        async move {
            self.write_buffer.extend_from_slice(&buf);
            if self.write_buffer.len() >= self.chunk_size {
                let flush_buf = std::mem::take(&mut self.write_buffer);
                self.xhfs
                    .fappend(&self.path, flush_buf, None)
                    .await
                    .map_err(EConv)?;
                self.write_buffer = Vec::with_capacity(self.chunk_size);
            }
            self.offset += buf.len() as u64;
            Ok(())
        }
        .boxed()
    }

    fn write_buf(&'_ mut self, mut buf: Box<dyn Buf + Send>) -> FsFuture<'_, ()> {
        read_only_guard!(self.read_only);
        async move {
            while buf.has_remaining() {
                let chunk = buf.chunk();
                self.write_buffer.extend_from_slice(chunk);
                self.offset += chunk.len() as u64;
                buf.advance(chunk.len());
                if self.write_buffer.len() >= self.chunk_size {
                    let flush_buf = std::mem::take(&mut self.write_buffer);
                    self.xhfs
                        .fappend(&self.path, flush_buf, None)
                        .await
                        .map_err(EConv)?;
                    self.write_buffer = Vec::with_capacity(self.chunk_size);
                }
            }
            Ok(())
        }
        .boxed()
    }

    fn flush(&mut self) -> FsFuture<'_, ()> {
        async move {
            if !self.write_buffer.is_empty() {
                let flush_buf = std::mem::take(&mut self.write_buffer);
                self.xhfs
                    .fappend(&self.path, flush_buf, None)
                    .await
                    .map_err(EConv)?;
                self.write_buffer = Vec::with_capacity(self.chunk_size);
            }
            Ok(())
        }
        .boxed()
    }

    fn redirect_url(&'_ mut self) -> FsFuture<'_, Option<String>> {
        std::future::ready(Ok(None)).boxed()
    }
}

#[derive(Clone)]
struct XHFSAdapter {
    xhfs: Arc<XHFS>,
    read_only: bool,
    chunk_size: usize,
}

impl GuardedFileSystem<()> for XHFSAdapter {
    fn open<'a>(
        &'a self,
        path: &'a DavPath,
        options: OpenOptions,
        _creds: &'a (),
    ) -> FsFuture<'a, Box<dyn DavFile>> {
        tracing::warn!("open: {path:?}, options = {options:?}");
        let xhfs = self.xhfs.clone();
        let path_buf = path.as_pathbuf();
        let chunk_size = self.chunk_size;
        let read_only = self.read_only;
        async move {
            tracing::info!("WebDAV open {path_buf:?}");
            if options.write && options.truncate {
                read_only_guard_raw!(read_only);
                xhfs.fwrite(
                    &path_buf,
                    vec![],
                    WriteOption {
                        overwrite: options.truncate,
                        ..Default::default()
                    },
                )
                .await
                .map_err(EConv)?;
            }

            match xhfs.stats(path_buf.clone(), true).await {
                Ok(Some(estat)) => {
                    if matches!(estat.kind, INodeKind::Directory) {
                        tracing::warn!("Attempted to open directory {path_buf:?}");
                        return Err(FsError::Forbidden);
                    }
                    let file_handle = XHFSFile {
                        xhfs,
                        path: path_buf,
                        offset: 0,
                        chunk_size,
                        read_only,
                        estat,
                        write_buffer: Vec::with_capacity(chunk_size),
                    };
                    Ok(Box::new(file_handle) as Box<dyn DavFile>)
                }
                Ok(None) => {
                    tracing::warn!("File not found {path_buf:?}");
                    Err(FsError::NotFound)
                }
                Err(e) => {
                    tracing::error!(error = ?e, "Metadata lookup failed {path_buf:?}");
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
        tracing::warn!("metadata: {path:?}");
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
                    tracing::error!("Backend metadata lookup failure: {e:#}");
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
        tracing::warn!("ls: {path:?}");
        // TODO:
        // stream dir listing later in core
        let xhfs = self.xhfs.clone();
        let path_buf = path.as_pathbuf();
        async move {
            if path.file_name_bytes() == b".localized" {
                return Err(FsError::NotFound);
            }
            let item_names = xhfs.ls(path_buf.clone()).await.map_err(|e| {
                tracing::error!(error = ?e, "Failed listing path collection targets");
                FsError::NotFound
            })?;

            let mut entries = vec![];
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
        creds: &'a (),
    ) -> FsFuture<'a, Box<dyn DavMetaData>> {
        tracing::warn!("symlink metadata: {path:?}");
        self.metadata(path, creds)
    }

    fn create_dir<'a>(&'a self, path: &'a DavPath, _creds: &'a ()) -> FsFuture<'a, ()> {
        read_only_guard!(self.read_only);
        tracing::warn!("create dir: {path:?}");
        let xhfs = self.xhfs.clone();
        let path_buf = path.as_pathbuf();
        async move {
            xhfs.mkdir(&path_buf, true).await.map_err(|e| {
                tracing::error!(error = ?e, "mkdir failed for path {path_buf:?}");
                FsError::GeneralFailure
            })?;
            Ok(())
        }
        .boxed()
    }

    fn remove_dir<'a>(&'a self, path: &'a DavPath, _creds: &'a ()) -> FsFuture<'a, ()> {
        read_only_guard!(self.read_only);
        tracing::warn!("remove dir: {path:?}");
        let xhfs = self.xhfs.clone();
        let path_buf = path.as_pathbuf();
        async move {
            // TODO: native recursive
            // The default implementation is using a combination of ls and unlink
            xhfs.unlink(&path_buf).await.map_err(|e| {
                tracing::error!(error = ?e, "unlink folder failed for path {path_buf:?}");
                FsError::GeneralFailure
            })?;
            Ok(())
        }
        .boxed()
    }

    fn remove_file<'a>(&'a self, path: &'a DavPath, _creds: &'a ()) -> FsFuture<'a, ()> {
        read_only_guard!(self.read_only);
        let xhfs = self.xhfs.clone();
        let path_buf = path.as_pathbuf();
        async move {
            xhfs.unlink(&path_buf).await.map_err(|e| {
                tracing::error!(error = ?e, "unlink file failed for path {path_buf:?}");
                FsError::GeneralFailure
            })?;
            Ok(())
        }
        .boxed()
    }

    fn rename<'a>(
        &'a self,
        from: &'a DavPath,
        to: &'a DavPath,
        _creds: &'a (),
    ) -> FsFuture<'a, ()> {
        read_only_guard!(self.read_only);
        let xhfs = self.xhfs.clone();
        let from_path_buf = from.as_pathbuf();
        let to_path_buf = to.as_pathbuf();
        async move {
            xhfs.fmove(from_path_buf, to_path_buf).await.map_err(|e| {
                tracing::error!("fmove failed: {e:#}");
                FsError::GeneralFailure
            })?;
            Ok(())
        }
        .boxed()
    }

    fn copy<'a>(&'a self, from: &'a DavPath, to: &'a DavPath, _creds: &'a ()) -> FsFuture<'a, ()> {
        read_only_guard!(self.read_only);
        tracing::warn!("fcopy {from:?} => {to:?}");
        let xhfs = self.xhfs.clone();
        let from_path_buf = from.as_pathbuf();
        let to_path_buf = to.as_pathbuf();
        async move {
            xhfs.fcopy_stream(
                from_path_buf,
                to_path_buf,
                self.chunk_size,
                WriteOption {
                    overwrite: true,
                    ..Default::default()
                },
            )
            .await
            .map_err(EConv)?;
            Ok(())
        }
        .boxed()
    }

    fn have_props<'a>(
        &'a self,
        path: &'a DavPath,
        _creds: &'a (),
    ) -> std::pin::Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        tracing::warn!("Have props? {path}");
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
                tracing::error!("Failed listing path collection targets: {e:#}");
                FsError::NotFound
            })?;
            let remaining = xhfs.total_remaining_capacity().await.map_err(|e| {
                tracing::error!("Failed calculating capacity: {e:#}");
                FsError::NotFound
            })?;
            Ok((total_capacity as u64, Some(remaining as u64)))
        }
        .boxed()
    }
}

pub async fn webdav_main(
    addr: String,
    port: u16,
    xhfs: Arc<XHFS>,
    read_only: bool,
) -> eyre::Result<()> {
    println!("-------");
    println!("{}", xhfs.format_headers_report().await?);
    println!("-------");
    let addr = format!("{addr}:{port}").parse::<SocketAddr>()?;
    let xhfs_adapter = Box::new(XHFSAdapter {
        xhfs,
        read_only,
        chunk_size: 64 * 1024, // standard 64KB disk read windows
    });
    let dav_server = DavHandler::builder()
        .filesystem(xhfs_adapter)
        .locksystem(FakeLs::new())
        .build_handler();

    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("Binding {addr}:{port}"))?;
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
                eprintln!("Error executing HTTP session processing context: {err:#}");
            }
        });
    }
}
