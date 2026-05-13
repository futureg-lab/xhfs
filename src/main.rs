use std::sync::Arc;

use tracing_subscriber::EnvFilter;

use crate::{
    bfs::{BruteFS, WriteOption},
    device::{Device, fs_device::FsDevice, logical::LogicalDevice},
    disk::Controller,
};

pub mod addr;
pub mod bfs;
pub mod device;
pub mod disk;
pub mod utils;

#[cfg(test)]
mod tests;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("brutefs=WARN"))
        .unwrap();

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .without_time()
        .init();

    let dev1 = LogicalDevice::new(
        2,
        [
            Arc::from(FsDevice::new("A.bin", 10 * 1000 * 1000).await?) as Arc<dyn Device>,
            Arc::from(FsDevice::new("B.bin", 10 * 1000 * 1000).await?) as Arc<dyn Device>,
        ],
    )?;
    let dev2 = LogicalDevice::new(
        2,
        [Arc::from(FsDevice::new("C.bin", 20 * 1000 * 1000).await?) as Arc<dyn Device>],
    )?;

    let ctrl = Controller::from([dev1, dev2]).await?;
    println!("Total size {:?}", ctrl.total_capacity());

    let bfs = BruteFS::format_new(ctrl).await?;
    println!("Root {:?}", bfs.get_root_inode().await?);
    bfs.mkdir("/", true).await?;
    println!("----");
    bfs.mkdir("/hello", true).await?;
    bfs.mkdir("/hello/foo", true).await?;
    bfs.mkdir("/hello/bar", true).await?;
    bfs.mkdir("/hello/baz/aaa", true).await?;
    bfs.mkdir("/hello/baz/bbb", true).await?;
    bfs.mkdir("/world", true).await?;
    // bfs.mkdir("/world".into(), true).await?;
    bfs.create_link("/thelink", "/hello/baz/", WriteOption { overwrite: false })
        .await?;
    for entry in bfs.ls("/hello/baz/").await? {
        println!("{entry}");
    }

    for entry in bfs.ls("/thelink").await? {
        println!("{entry}");
    }

    println!("{:?}", bfs.stats("/thelink").await?);
    println!("{:?}", bfs.stats("/hello/baz/").await?);

    Ok(())
}
