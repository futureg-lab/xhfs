use std::sync::Arc;

use tracing_subscriber::EnvFilter;

use crate::{
    device::{Device, fs_device::FsDevice, logical::LogicalDevice},
    disk::Controller,
};

pub mod device;
pub mod disk;
pub mod bfs;

#[cfg(test)]
mod tests;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("brutefs=DEBUG"))
        .unwrap();

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .without_time()
        .init();

    let dev1 = LogicalDevice::new(
        2,
        [
            Arc::from(FsDevice::new("A.bin", 10).await?) as Arc<dyn Device>,
            Arc::from(FsDevice::new("B.bin", 10).await?) as Arc<dyn Device>,
        ],
    )?;
    let dev2 = LogicalDevice::new(
        2,
        [Arc::from(FsDevice::new("C.bin", 20).await?) as Arc<dyn Device>],
    )?;

    let ctrl = Controller::from([dev1, dev2]).await?;
    println!("Total size {:?}", ctrl.total_capacity());
    ctrl.write(8, "ABCDEF".as_bytes()).await?;

    let data = String::from_utf8(ctrl.read(8, 6).await?)?;
    println!("{data:?}");

    Ok(())
}
