use crate::{
    bfs::BruteFS,
    device::{
        Device,
        kv_device::{KVDevice, MemoryKV},
        logical::LogicalDevice,
    },
    disk::Controller,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::RwLock;

async fn create_simple_memory_brute_fs(capacity: usize) -> eyre::Result<BruteFS> {
    if capacity % 8 != 0 {
        eyre::bail!("Test fs expects % 8");
    }
    let dev1 = KVDevice {
        store: Arc::new(RwLock::new(MemoryKV(HashMap::new()))),
        total_slots: capacity / 8,
        slot_capacity: 8,
    };
    let dev1 = LogicalDevice::new(2, [Arc::from(dev1) as Arc<dyn Device>])?;
    let ctrl = Controller::from([dev1]).await?;
    BruteFS::new(ctrl)
}

#[tokio::test]
async fn test_base_brutefs() -> eyre::Result<()> {
    let mut bfs = create_simple_memory_brute_fs(128 * 1000 * 1000).await?;
    bfs.format().await?;

    assert_eq!(
        bfs.total_capacity()?,
        128 * 1000 * 1000,
        "size should be coherent"
    );

    {
        let offset = 16057; // header + root INode
        let addr = bfs.allocate(100).await?;
        assert_eq!(addr, offset, "allocate init");
        let addr = bfs.allocate(4).await?;
        assert_eq!(addr, offset + 100, "allocate test 1");
        let addr = bfs.allocate(42).await?;
        assert_eq!(addr, offset + 100 + 4, "allocate test 2");
    }

    Ok(())
}

async fn print_ls<P: Into<PathBuf>>(p: P, bfs: BruteFS) -> eyre::Result<()> {
    for entry in bfs.ls(p.into()).await? {
        println!("{entry}");
    }
    Ok(())
}

#[tokio::test]
async fn test_base_fs_ops() -> eyre::Result<()> {
    let mut bfs = create_simple_memory_brute_fs(128 * 1000 * 1000).await?;
    bfs.format().await?;

    bfs.mkdir("/hello/world".into(), true).await?;

    print_ls("/", bfs).await?;

    Ok(())
}
