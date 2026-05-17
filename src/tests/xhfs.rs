use crate::{
    device::{Device, disk::Controller, kv_device::*, logical::LogicalDevice},
    xhfs::{WriteOption, XHFS, ds::*},
};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;

async fn create_simple_memory_brute_fs(capacity: usize) -> eyre::Result<XHFS> {
    if capacity % 8 != 0 {
        eyre::bail!("Test fs expects % 8");
    }
    let dev1 = KVDevice {
        store: Arc::new(MemoryKV(RwLock::new(HashMap::new()))),
        total_slots: capacity / 8,
        slot_capacity: 8,
    };
    let dev1 = LogicalDevice::new(2, [Arc::from(dev1) as Arc<dyn Device>])?;
    let ctrl = Controller::from([dev1]).await?;
    XHFS::format_new(ctrl, Some("helloworld".to_string())).await
}

#[tokio::test]
async fn test_xhfs_core_ops() -> eyre::Result<()> {
    let bfs = create_simple_memory_brute_fs(128 * 1000 * 1000).await?;

    assert!(bfs.mkdir("/", false).await? == false, "root is not new");

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
    bfs.fwrite(
        "/hello/baz/123.bin",
        vec![1, 2, 3],
        WriteOption { overwrite: false },
    )
    .await?;
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
            ["aaa".to_string(), "bbb".to_string(), "123.bin".to_string()]
        );
    }

    {
        assert!(
            bfs.unlink("/hello/baz").await.is_err(),
            "cannot unlink nested path"
        );
        assert!(
            bfs.unlink("/hello/baz/123.bin").await.is_ok(),
            "unlink leaf file"
        );
        assert!(
            bfs.unlink("/hello/baz/aaa").await.is_ok(),
            "unlink leaf folder"
        );
        assert_eq!(bfs.ls("/hello/baz/").await?, ["bbb".to_string()]);
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

        // also prove dir entry remap works fine
        assert_eq!(
            data1,
            String::from_utf8(bfs.fread("/hello/baz/bbb/content1.txt").await?).unwrap(),
            "reading written content 1 HAS changed"
        );
        assert_eq!(
            data2,
            String::from_utf8(bfs.fread("/hello/baz/bbb/content2.txt").await?).unwrap(),
            "reading written content 2 remains unchanged"
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_xhfs_ref_manips_and_stats() -> eyre::Result<()> {
    let bfs = create_simple_memory_brute_fs(128 * 1000 * 1000).await?;

    bfs.mkdir("/many/entries/a", true).await?;
    bfs.mkdir("/many/entries/b", true).await?;
    bfs.mkdir("/many/entries/c", true).await?;
    bfs.fwrite(
        "/many/entries/d.txt",
        "HELLO".as_bytes().to_vec(),
        WriteOption { overwrite: false },
    )
    .await?;

    {
        // extent 1 = HELLO
        // extent 2 = ABC
        bfs.fappend("/many/entries/d.txt", "ABC".as_bytes().to_vec())
            .await?;
        assert_eq!(
            bfs.fread("/many/entries/d.txt").await?,
            "HELLOABC".as_bytes(),
            "fappend works"
        );
        assert_eq!(
            bfs.fseek("/many/entries/d.txt", 4, 7).await?,
            "OAB".as_bytes(),
            "extent boundaries are contiguous from user POV"
        );
    }

    // non-trivial ref manips
    {
        bfs.fcopy(
            "/many/entries/d.txt",
            "/many/entries/other.txt",
            WriteOption { overwrite: false },
        )
        .await?;
        bfs.fmove("/many/entries", "/many/entries2").await?;
        bfs.create_link(
            "/file.link",
            "/many/entries2/d.txt",
            WriteOption { overwrite: false },
        )
        .await?;
        bfs.create_link(
            "/folder.link",
            "/many/entries2",
            WriteOption { overwrite: false },
        )
        .await?;
    }

    assert_eq!(
        bfs.ls("/many/entries2").await?,
        bfs.ls("/folder.link").await?
    );
    assert_eq!(
        bfs.fread("/many/entries2/d.txt").await?,
        bfs.fread("/file.link").await?
    );
    assert_eq!(
        bfs.fread("/many/entries2/d.txt").await?,
        bfs.fread("/many/entries2/other.txt").await?,
    );

    let dtxt_stats = bfs.stats("/many/entries2/d.txt").await?.unwrap();
    let filelink_stats = bfs.stats("/file.link").await?.unwrap();
    let folder_stats = bfs.stats("/many/entries2").await?.unwrap();

    assert_eq!(
        folder_stats.size, None,
        "size not immediately calculated for folders"
    );
    assert_eq!(dtxt_stats.kind, INodeKind::File, "stats should work");
    assert_eq!(dtxt_stats.size, Some(8), "should give correct data size");
    assert_eq!(
        filelink_stats.kind,
        INodeKind::Symlink,
        "stats should not resolve symlink"
    );

    Ok(())
}
