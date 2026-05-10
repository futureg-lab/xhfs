use crate::block::Device;
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::{
    fs::OpenOptions,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};

pub struct FsDevice {
    file: PathBuf,
    size: usize,
    offset: usize,
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
        eyre::ensure!(
            addr >= self.offset,
            "address {} before device offset {}",
            addr,
            self.offset
        );

        let local_addr = addr - self.offset;
        eyre::ensure!(
            local_addr + data.len() <= self.size,
            "write out of bounds: addr={} len={} size={}",
            local_addr,
            data.len(),
            self.size
        );
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .open(&self.file)
            .await?;
        file.seek(std::io::SeekFrom::Start(local_addr as u64))
            .await?;

        file.write_all(data).await?;
        file.flush().await?;

        Ok(())
    }

    async fn read(&self, addr: usize, size: usize) -> eyre::Result<Vec<u8>> {
        eyre::ensure!(
            addr >= self.offset,
            "address {} before device offset {}",
            addr,
            self.offset
        );

        let local_addr = addr - self.offset;
        eyre::ensure!(
            local_addr + size <= self.size,
            "read out of bounds: addr={} size={} device_size={}",
            local_addr,
            size,
            self.size
        );

        let mut file = OpenOptions::new().read(true).open(&self.file).await?;
        file.seek(std::io::SeekFrom::Start(local_addr as u64))
            .await?;

        let mut buf = vec![0u8; size];
        let read = file.read(&mut buf).await?;
        if read != size {
            eyre::bail!("short read: expected {} bytes got {}", size, read);
        }

        Ok(buf)
    }
}
