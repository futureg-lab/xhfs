use crate::device::Device;
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct FsDevice {
    file: Arc<Mutex<File>>,
    size: usize,
}

impl FsDevice {
    pub async fn new<P: Into<PathBuf>>(
        file: P,
        size: usize,
        overwrite: bool,
    ) -> eyre::Result<Self> {
        let path: PathBuf = file.into();
        if overwrite && path.exists() {
            fs::remove_file(&path).await?;
        }
        let tokio_file = match fs::metadata(&path).await {
            Ok(meta) => {
                let existing_size = meta.len() as usize;
                if existing_size == size {
                    tracing::warn!("Reusing existing {path:?}");
                    OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&path)
                        .await?
                } else {
                    eyre::bail!(
                        "File exists but has wrong size (expected {size}, got {existing_size})",
                    );
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!("Creating new file {path:?}");
                fs::write(&path, vec![0u8; size])
                    .await
                    .map_err(|e| eyre::eyre!(e))?;

                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .await?
            }
            Err(e) => return Err(eyre::eyre!(e)),
        };

        Ok(Self {
            file: Arc::new(Mutex::new(tokio_file)),
            size,
        })
    }
}

#[async_trait]
impl Device for FsDevice {
    fn name(&self) -> String {
        "fs".to_string()
    }

    async fn capacity(&self) -> eyre::Result<usize> {
        Ok(self.size)
    }

    async fn write(&self, addr: usize, data: &[u8]) -> eyre::Result<()> {
        let mut file = self.file.lock().await;
        file.seek(std::io::SeekFrom::Start(addr as u64)).await?;

        file.write_all(data).await?;
        file.flush().await?;

        Ok(())
    }

    async fn read(&self, addr: usize, size: usize) -> eyre::Result<Vec<u8>> {
        let mut file = self.file.lock().await;
        file.seek(std::io::SeekFrom::Start(addr as u64)).await?;

        let mut buf = vec![0u8; size];

        // Note:
        // read_exact will internally loop, repoll, and handle
        // It will only fail if it hits an actual physical EOF early
        // When the file block is too large, we can hit a short read with basic read
        tokio::io::AsyncReadExt::read_exact(&mut *file, &mut buf).await?;

        Ok(buf)
    }
}
