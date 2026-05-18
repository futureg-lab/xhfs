use crate::{
    device::{Device, disk::Controller, kv_device::*, logical::LogicalDevice},
    xhfs::{WriteOption, XHFS, ds::*},
};
use std::{collections::HashMap, io::Cursor, sync::Arc};
use tokio::sync::RwLock;

async fn create_simple_memory_xhfs(capacity: usize) -> eyre::Result<XHFS> {
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
    let bfs = create_simple_memory_xhfs(128 * 1000 * 1000).await?;

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
    let bfs = create_simple_memory_xhfs(128 * 1000 * 1000).await?;

    bfs.mkdir("/many/entries/a", true).await?;
    bfs.mkdir("/many/entries/b", true).await?;
    bfs.mkdir("/many/entries/c", true).await?;
    bfs.fwrite(
        "/many/entries/d.txt",
        b"HELLO".to_vec(),
        WriteOption { overwrite: false },
    )
    .await?;

    {
        // extent 1 = HELLO
        // extent 2 = ABC
        bfs.fappend("/many/entries/d.txt", b"ABC".to_vec()).await?;
        assert_eq!(
            bfs.fread("/many/entries/d.txt").await?,
            b"HELLOABC",
            "fappend works"
        );
        assert_eq!(
            bfs.fseek("/many/entries/d.txt", 4, 7).await?,
            b"OAB",
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

#[tokio::test]
async fn test_fstream_from_no_file() -> eyre::Result<()> {
    let bfs = create_simple_memory_xhfs(128 * 1000 * 1000).await?;
    let mut memory_stream = Cursor::new(b"123456789".to_vec());
    let block_size = 4;
    bfs.fwrite_stream(
        "hello.txt",
        &mut memory_stream,
        block_size,
        WriteOption { overwrite: false },
    )
    .await?;

    assert_eq!(bfs.fread("hello.txt").await?, b"123456789");

    let inode = bfs.resolve_path("hello.txt").await?;

    let meta_exts = bfs
        .find_full_extent_metadata(inode.extent_addr, Some(10))
        .await?;
    let header = bfs.get_header().await?;

    assert_eq!(meta_exts.len(), 3, "each write shot should use 1 extent");
    // each burst of 4 bytes waste 4096 B (1 block) - 4B - 16B extent header space
    assert_eq!(meta_exts[0].size_span(), header.format.block_size_bytes);
    assert_eq!(meta_exts[1].size_span(), header.format.block_size_bytes);
    assert_eq!(meta_exts[2].size_span(), header.format.block_size_bytes);

    Ok(())
}

#[tokio::test]
async fn test_bfs_real_prelayout_deduction() -> eyre::Result<()> {
    {
        let bfs = create_simple_memory_xhfs(30 * 1000 * 1000).await?;
        let format = bfs.get_header().await?.format;
        assert_eq!(
            format,
            Format {
                param_data_block_count_per_group: 20480,
                param_inode_count_per_group: 4096,
                block_size_bytes: 4096,
                group_count: 1 // many to few
            }
        );
    }
    {
        let bfs = create_simple_memory_xhfs(128 * 1000 * 1000).await?;
        let format = bfs.get_header().await?.format;
        assert_eq!(
            format,
            Format {
                param_data_block_count_per_group: 20480,
                param_inode_count_per_group: 4096,
                block_size_bytes: 4096,
                group_count: 2
            }
        );
    }
    {
        let bfs = create_simple_memory_xhfs(800 * 1000 * 1000).await?;
        let format = bfs.get_header().await?.format;
        assert_eq!(
            format,
            Format {
                param_data_block_count_per_group: 20480,
                param_inode_count_per_group: 4096,
                block_size_bytes: 4096,
                group_count: 10
            }
        );
    }

    Ok(())
}
