use std::sync::Arc;

use tracing_subscriber::EnvFilter;

use crate::{
    device::{Device, LogicalDevice, fs_device::FsDevice},
    disk::Controller,
};

pub mod device;
pub mod disk;

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
        vec![
            Arc::from(FsDevice {
                file: "A.bin".into(),
                size: 10,
            }) as Arc<dyn Device>,
            Arc::from(FsDevice {
                file: "B.bin".into(),
                size: 10,
            }) as Arc<dyn Device>,
        ],
    )?;
    let dev2 = LogicalDevice::new(
        2,
        vec![Arc::from(FsDevice {
            file: "C.bin".into(),
            size: 20,
        }) as Arc<dyn Device>],
    )?;

    let ctrl = Controller::from(vec![dev1, dev2]).await?;
    ctrl.write(8, "ABCDEF".as_bytes()).await?;

    let data = String::from_utf8(ctrl.read(8, 6).await?)?;
    println!("{data:?}");

    Ok(())
}
