use std::sync::Arc;

pub mod fs_device;

use async_trait::async_trait;
use futures::{
    StreamExt, TryStreamExt,
    stream::{self},
};

#[async_trait]
pub trait Device {
    fn name(&self) -> String;
    async fn capacity(&self) -> eyre::Result<usize>;
    async fn init(&self) -> eyre::Result<()>;
    async fn write(&self, addr: usize, data: &[u8]) -> eyre::Result<()>;
    async fn read(&self, addr: usize, size: usize) -> eyre::Result<Vec<u8>>;
}

#[derive(Clone)]
pub struct LogicalDevice {
    max_concurrent: usize,
    replica: Vec<Arc<dyn Device>>,
}

impl LogicalDevice {
    pub fn new(max_concurrent: usize, devices: Vec<Arc<dyn Device>>) -> eyre::Result<Self> {
        if devices.is_empty() {
            eyre::bail!("Device cannot be empty");
        }

        Ok(LogicalDevice {
            max_concurrent,
            replica: devices,
        })
    }

    pub async fn allocate_if_unset(&self) -> eyre::Result<usize> {
        let sizes = stream::iter(self.replica.iter().map(|device| async move {
            {
                device.init().await?;
                let name = device.name();
                device.capacity().await.map(|c| (name, c))
            }
        }))
        .buffer_unordered(self.max_concurrent)
        .try_collect::<Vec<(String, usize)>>()
        .await?;

        let expected = sizes[0].1;
        for (device, size) in sizes {
            if size != expected {
                eyre::bail!(
                    "Fatal error: expected allocated capacity {expected} on device {device}, got {size} instead"
                );
            }
        }

        Ok(expected)
    }

    pub async fn write(&self, addr: usize, data: &[u8]) -> eyre::Result<()> {
        stream::iter(
            self.replica
                .iter()
                .map(|device| async move { device.write(addr, data).await }),
        )
        .buffer_unordered(self.max_concurrent)
        .try_collect::<()>()
        .await
    }

    pub async fn read(&self, addr: usize, size: usize) -> eyre::Result<Vec<u8>> {
        let mut stream = stream::iter(self.replica.iter().map(|replica| replica.read(addr, size)))
            .buffer_unordered(self.max_concurrent);
        let mut errors = vec![];
        while let Some(result) = stream.next().await {
            match result {
                Ok(data) => return Ok(data),
                Err(err) => errors.push(err),
            }
        }

        eyre::bail!(
            errors
                .into_iter()
                .map(|e| format!("{e:#}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}
