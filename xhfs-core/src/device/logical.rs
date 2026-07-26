use crate::device::{ConcreteDevice, Device};
use futures::{
    StreamExt, TryStreamExt,
    stream::{self},
};

#[derive(Clone)]
pub struct LogicalDevice {
    max_concurrent: usize,
    replica: Vec<ConcreteDevice>,
}

impl LogicalDevice {
    pub fn new<D>(max_concurrent: usize, devices: D) -> eyre::Result<Self>
    where
        D: IntoIterator<Item = ConcreteDevice>,
        D::IntoIter: ExactSizeIterator,
    {
        if max_concurrent < 1 {
            eyre::bail!("Max concurrent device cannot be less than 1");
        }

        let devices = devices.into_iter();
        if devices.len() == 0 {
            eyre::bail!("Device count cannot be 0");
        }

        Ok(LogicalDevice {
            max_concurrent,
            replica: devices.collect(),
        })
    }

    pub async fn validate_layout(&self) -> eyre::Result<usize> {
        let sizes = stream::iter(self.replica.iter().map(|device| async move {
            {
                let name = device.name();
                device.capacity().await.map(|c| (name, c))
            }
        }))
        .buffer_unordered(self.max_concurrent)
        .try_collect::<Vec<(String, usize)>>()
        .await?;

        let expected = sizes[0].1;
        for (device, size) in sizes {
            if size < expected {
                eyre::bail!(
                    "Fatal error: expected allocated capacity >={expected} on device {device}, got {size} instead"
                );
            }
        }

        Ok(expected)
    }

    pub async fn write(&self, addr: usize, data: &[u8]) -> eyre::Result<()> {
        #[allow(clippy::redundant_iter_cloned)]
        // clippy does not understand downstream type erasure due to async Device traits for some reason
        let mut results = stream::iter(
            self.replica
                .iter()
                .cloned()
                .map(|device| async move { device.write(addr, data).await }),
        )
        .buffer_unordered(self.max_concurrent);

        let mut total_replicas = 0;
        let mut failed_replicas = 0;
        while let Some(result) = results.next().await {
            total_replicas += 1;
            if let Err(e) = result {
                failed_replicas += 1;
                tracing::warn!(error = ?e, "Failed to write to device replica #{total_replicas}");
            }
        }
        if total_replicas > 0 && failed_replicas == total_replicas {
            eyre::bail!("All {total_replicas} replicas failed to write");
        }

        Ok(())
    }

    pub async fn read(&self, addr: usize, size: usize) -> eyre::Result<Vec<u8>> {
        let mut errors = vec![];
        for (i, replica) in self.replica.iter().enumerate() {
            match replica.read(addr, size).await {
                Ok(data) => return Ok(data),
                Err(e) => {
                    tracing::warn!(error = ?e, "Failed to reading device replica #{}", i + 1);
                    errors.push(format!("Failing device read #{}: {:#}", i + 1, e))
                }
            }
        }
        eyre::bail!(errors.join("\n"))
    }
}
