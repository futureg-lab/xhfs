use crate::{
    addr::MaybeU64,
    bfs::{AddressSlot, BruteFS, WriteOption},
    device::{
        Device,
        kv_device::{KVDevice, MemoryKV},
        logical::LogicalDevice,
    },
    disk::Controller,
};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
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
    BruteFS::format_new(ctrl).await
}

#[tokio::test]
async fn test_base_allocate_brutefs() -> eyre::Result<()> {
    let bfs = create_simple_memory_brute_fs(128 * 1000 * 1000).await?;

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
        bfs.mark_as_reusable(AddressSlot {
            addr: MaybeU64::from(addr),
            capacity: 42,
        })
        .await?;
        assert_eq!(
            bfs.get_header().await?.extent_freed.items[0],
            AddressSlot {
                addr: MaybeU64::from(addr),
                capacity: 42,
            }
        );
        let addr1 = bfs.allocate(42).await?;
        assert_eq!(
            bfs.get_header().await?.extent_freed.items[0],
            AddressSlot::default()
        );
        let addr2 = bfs.allocate(42).await?;

        assert_eq!(addr1, offset + 100 + 4, "re-use allocated in test 2");
        assert_eq!(
            addr2,
            offset + 100 + 4 + 42,
            "allocate from global offset test 2"
        );
    }

    Ok(())
}

#[allow(unused)]
async fn print_ls<P: Into<PathBuf>>(p: P, bfs: BruteFS) -> eyre::Result<()> {
    for entry in bfs.ls(p).await? {
        println!("{entry}");
    }
    Ok(())
}

#[tokio::test]
async fn test_base_fs_ops() -> eyre::Result<()> {
    let bfs = create_simple_memory_brute_fs(128 * 1000 * 1000).await?;

    assert!(bfs.mkdir("/", false).await? == true, "created new");
    assert!(bfs.mkdir("/", false).await? == false, "exists already");

    bfs.mkdir("/hello", true).await?;
    assert!(
        bfs.mkdir("/hello/not/recursive", false).await.is_err(),
        "not recursive fail if many components"
    );
    bfs.mkdir("/hello/foo", true).await?;
    bfs.mkdir("/hello/bar", true).await?;
    bfs.mkdir("/hello/bar", true).await?;
    bfs.mkdir("/hello/baz/aaa", true).await?;
    bfs.mkdir("/hello/baz/bbb", true).await?;
    bfs.mkdir("/world", true).await?;

    {
        assert_eq!(
            bfs.ls("/").await?,
            ["hello".to_string(), "world".to_string()]
        );
        assert_eq!(
            bfs.ls("/hello").await?,
            ["foo".to_string(), "bar".to_string(), "baz".to_string()]
        );
        assert_eq!(bfs.ls("/world").await?, [] as [String; 0]);
        assert_eq!(
            bfs.ls("/hello/baz/").await?,
            ["aaa".to_string(), "bbb".to_string()]
        );
    }

    assert!(
        bfs.fread("/hello/notexist.exe").await.is_err(),
        "file does not exist works"
    );

    let data1 = "This is the content";
    let data2 = "Another content";
    {
        bfs.fwrite(
            "/hello/baz/bbb/content1.txt",
            data1.as_bytes().to_vec(),
            WriteOption { overwrite: false },
        )
        .await?;
        bfs.fwrite(
            "/hello/baz/bbb/content2.txt",
            data2.as_bytes().to_vec(),
            WriteOption { overwrite: false },
        )
        .await?;
        assert_eq!(
            data1,
            String::from_utf8(bfs.fread("/hello/baz/bbb/content1.txt").await?).unwrap(),
            "reading written content 1"
        );
        assert_eq!(
            data2,
            String::from_utf8(bfs.fread("/hello/baz/bbb/content2.txt").await?).unwrap(),
            "reading written content 2"
        );
    }

    {
        let data1 = "Another content";
        bfs.fwrite(
            "/hello/baz/bbb/content1.txt",
            data1.as_bytes().to_vec(),
            WriteOption { overwrite: true },
        )
        .await?;

        assert_eq!(
            data1,
            String::from_utf8(bfs.fread("/hello/baz/bbb/content1.txt").await?).unwrap(),
            "reading written content 1"
        );
        assert_eq!(
            data2,
            String::from_utf8(bfs.fread("/hello/baz/bbb/content2.txt").await?).unwrap(),
            "reading written content 2 has unchanged"
        );
    }

    Ok(())
}
