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
        // TODO:
        // each logical device should the events
        // and retry entries that has failed to write
        // it should show user friendly logs outlining which backend fails

        stream::iter(
            self.replica
                .iter()
                .cloned()
                .map(|device| async move { device.write(addr, data).await }),
        )
        .buffer_unordered(self.max_concurrent)
        .try_collect::<()>()
        .await
    }

    pub async fn read(&self, addr: usize, size: usize) -> eyre::Result<Vec<u8>> {
        let mut tasks = futures::stream::FuturesUnordered::new();
        for replica in self.replica.clone() {
            tasks.push(async move { replica.read(addr, size).await });
        }
        let mut errors = vec![];
        while let Some(result) = tasks.next().await {
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
