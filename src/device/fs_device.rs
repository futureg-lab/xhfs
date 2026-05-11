use crate::device::Device;
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};

pub struct FsDevice {
    file: PathBuf,
    size: usize,
}

impl FsDevice {
    pub async fn new<P: Into<PathBuf>>(file: P, size: usize) -> eyre::Result<Self> {
        let file: PathBuf = file.into();
        fs::write(&file, vec![0u8; size])
            .await
            .map_err(|e| eyre::eyre!(e))?;
        Ok(Self { file, size })
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
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .open(&self.file)
            .await?;
        file.seek(std::io::SeekFrom::Start(addr as u64)).await?;

        file.write_all(data).await?;
        file.flush().await?;

        Ok(())
    }

    async fn read(&self, addr: usize, size: usize) -> eyre::Result<Vec<u8>> {
        let mut file = OpenOptions::new().read(true).open(&self.file).await?;
        file.seek(std::io::SeekFrom::Start(addr as u64)).await?;

        let mut buf = vec![0u8; size];
        let read = file.read(&mut buf).await?;
        if read != size {
            eyre::bail!("short read: expected {} bytes got {}", size, read);
        }

        Ok(buf)
    }
}
