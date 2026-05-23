use crate::{
    device::{ConcreteDevice, disk::Controller, kv_device::*, logical::LogicalDevice},
    xhfs::{WriteOption, XHFS, ds::*},
};
use futures::StreamExt;
use std::{collections::HashMap, io::Cursor, sync::Arc};
use tokio::sync::RwLock;

async fn create_simple_memory_xhfs(capacity: usize) -> eyre::Result<XHFS> {
    if capacity % 8 != 0 {
        eyre::bail!("Test fs expects % 8");
    }
    let dev1 = ConcreteDevice::KVDevice(KVDevice {
        store: Arc::new(MemoryKV(RwLock::new(HashMap::new()))),
        total_slots: capacity / 8,
        slot_capacity: 8,
    });
    let dev1 = LogicalDevice::new(2, [dev1])?;
    let ctrl = Controller::from([dev1]).await?;
    XHFS::format_new(ctrl, Some("helloworld".to_string())).await
}

#[tokio::test]
async fn test_xhfs_core_ops() -> eyre::Result<()> {
    let xhfs = create_simple_memory_xhfs(128 * 1000 * 1000).await?;

    assert!(xhfs.mkdir("/", false).await? == false, "root is not new");

    xhfs.mkdir("/hello", true).await?;
    assert!(
        xhfs.mkdir("/hello/not/recursive", false).await.is_err(),
        "not recursive fail if many components"
    );
    xhfs.mkdir("/hello/foo", true).await?;
    xhfs.mkdir("/hello/bar", true).await?;
    xhfs.mkdir("/hello/bar", true).await?;
    xhfs.mkdir("/hello/baz/aaa", true).await?;
    xhfs.mkdir("/hello/baz/bbb", true).await?;
    xhfs.fwrite(
        "/hello/baz/123.bin",
        vec![1, 2, 3],
        WriteOption { overwrite: false },
    )
    .await?;
    xhfs.mkdir("/world", true).await?;

    {
        assert_eq!(
            xhfs.ls("/").await?,
            ["hello".to_string(), "world".to_string()]
        );
        assert_eq!(
            xhfs.ls("/hello").await?,
            ["foo".to_string(), "bar".to_string(), "baz".to_string()]
        );
        assert_eq!(xhfs.ls("/world").await?, [] as [String; 0]);
        assert_eq!(
            xhfs.ls("/hello/baz/").await?,
            ["aaa".to_string(), "bbb".to_string(), "123.bin".to_string()]
        );
    }

    {
        assert!(
            xhfs.unlink("/hello/baz").await.is_err(),
            "cannot unlink nested path"
        );
        assert!(
            xhfs.unlink("/hello/baz/123.bin").await.is_ok(),
            "unlink leaf file"
        );
        assert!(
            xhfs.unlink("/hello/baz/aaa").await.is_ok(),
            "unlink leaf folder"
        );
        assert_eq!(xhfs.ls("/hello/baz/").await?, ["bbb".to_string()]);
    }

    assert!(
        xhfs.fread("/hello/notexist.exe").await.is_err(),
        "file does not exist works"
    );

    let data1 = "This is the content";
    let data2 = "Another content";
    {
        xhfs.fwrite(
            "/hello/baz/bbb/content1.txt",
            data1.as_bytes().to_vec(),
            WriteOption { overwrite: false },
        )
        .await?;
        xhfs.fwrite(
            "/hello/baz/bbb/content2.txt",
            data2.as_bytes().to_vec(),
            WriteOption { overwrite: false },
        )
        .await?;
        assert_eq!(
            data1,
            String::from_utf8(xhfs.fread("/hello/baz/bbb/content1.txt").await?).unwrap(),
            "reading written content 1"
        );
        assert_eq!(
            data2,
            String::from_utf8(xhfs.fread("/hello/baz/bbb/content2.txt").await?).unwrap(),
            "reading written content 2"
        );
    }

    {
        let data1 = "Another content";
        xhfs.fwrite(
            "/hello/baz/bbb/content1.txt",
            data1.as_bytes().to_vec(),
            WriteOption { overwrite: true },
        )
        .await?;

        // also proves dir entry remap works fine
        assert_eq!(
            data1,
            String::from_utf8(xhfs.fread("/hello/baz/bbb/content1.txt").await?).unwrap(),
            "reading written content 1 HAS changed"
        );
        assert_eq!(
            data2,
            String::from_utf8(xhfs.fread("/hello/baz/bbb/content2.txt").await?).unwrap(),
            "reading written content 2 remains unchanged"
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_xhfs_ref_manips_and_stats() -> eyre::Result<()> {
    let xhfs = create_simple_memory_xhfs(128 * 1000 * 1000).await?;

    xhfs.mkdir("/many/entries/a", true).await?;
    xhfs.mkdir("/many/entries/b", true).await?;
    xhfs.mkdir("/many/entries/c", true).await?;
    xhfs.fwrite(
        "/many/entries/d.txt",
        b"HELLO".to_vec(),
        WriteOption { overwrite: false },
    )
    .await?;

    {
        // extent 1 = HELLO
        // extent 2 = ABC
        xhfs.fappend("/many/entries/d.txt", b"ABC".to_vec()).await?;
        assert_eq!(
            xhfs.fread("/many/entries/d.txt").await?,
            b"HELLOABC",
            "fappend works"
        );
        assert_eq!(
            xhfs.fseek("/many/entries/d.txt", 4, 7).await?,
            b"OAB",
            "extent boundaries are contiguous from user POV"
        );
    }

    // non-trivial ref manips
    {
        xhfs.fcopy(
            "/many/entries/d.txt",
            "/many/entries/other.txt",
            WriteOption { overwrite: false },
        )
        .await?;
        xhfs.fcopy_stream(
            "/many/entries/d.txt",
            "/many/entries/other-but-streamed.txt",
            3,
            WriteOption { overwrite: false },
        )
        .await?;
        xhfs.fmove("/many/entries", "/many/entries2").await?;
        xhfs.create_symlink(
            "/file.link",
            "/many/entries2/d.txt",
            WriteOption { overwrite: false },
        )
        .await?;
        xhfs.create_hardlink(
            "/file.hardlink",
            "/many/entries2/d.txt",
            WriteOption { overwrite: false },
        )
        .await?;
        xhfs.create_symlink(
            "/folder.link",
            "/many/entries2",
            WriteOption { overwrite: false },
        )
        .await?;
    }
    assert_eq!(
        xhfs.ls("/many/entries2").await?,
        xhfs.ls("/folder.link").await?,
        "folder symlink"
    );
    assert_eq!(
        xhfs.fread("/many/entries2/d.txt").await?,
        xhfs.fread("/file.link").await?,
        "file symlink"
    );
    assert_eq!(
        xhfs.fread("/many/entries2/d.txt").await?,
        xhfs.fread("/file.hardlink").await?,
        "file hardlink"
    );

    assert_eq!(
        xhfs.fseek("/file.link", 4, 7).await?,
        b"OAB",
        "Symlink + extent boundaries are contiguous from user POV"
    );
    assert_eq!(
        xhfs.fseek("/file.hardlink", 4, 7).await?,
        b"OAB",
        "Hardlink + extent boundaries are contiguous from user POV"
    );

    assert_eq!(
        xhfs.fread("/many/entries2/d.txt").await?,
        xhfs.fread("/many/entries2/other.txt").await?,
        "basic fcopy output"
    );
    assert_eq!(
        xhfs.fread("/many/entries2/other.txt").await?,
        xhfs.fread("/many/entries2/other-but-streamed.txt").await?,
        "chunked fcopy stream"
    );

    let dtxt_stats = xhfs.stats("/many/entries2/d.txt", false).await?.unwrap();
    let filelink_stats = xhfs.stats("/file.link", false).await?.unwrap();
    let folder_stats = xhfs.stats("/many/entries2", false).await?.unwrap();
    // basic stats
    {
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
    }
    // nlink
    {
        assert_eq!(folder_stats.nlink, 1, "basic nlink count should be 1");
        assert_eq!(dtxt_stats.nlink, 2, "hardlink should bump nlink");
    }

    Ok(())
}

#[tokio::test]
async fn test_fremove_and_hardlinks() -> eyre::Result<()> {
    let xhfs = create_simple_memory_xhfs(128 * 1000 * 1000).await?;
    xhfs.mkdir("/deeply/nested/folder/here", true).await?;

    let homework = vec![7; 160000];
    let og_path = "/deeply/nested/folder/here/original.bin";

    let space_before_any_writes = xhfs.total_remaining_capacity().await?;
    assert_eq!(space_before_any_writes, 83869696);

    xhfs.fwrite(og_path, homework.clone(), WriteOption { overwrite: false })
        .await?;
    let after_write_remaining = xhfs.total_remaining_capacity().await?;
    assert_eq!(after_write_remaining, 83701760);

    let stats = xhfs.stats(og_path, false).await?.unwrap();
    assert_eq!(stats.nlink, 1);

    xhfs.create_hardlink(
        "/deeply/nested/001.hardlink",
        og_path,
        WriteOption { overwrite: false },
    )
    .await?;
    assert_eq!(
        xhfs.total_remaining_capacity().await?,
        after_write_remaining - 4096,
        "Hardlink should spend only INode space + 1 block for storing refered inumber"
    );
    let stats = xhfs.stats(og_path, false).await?.unwrap();
    assert_eq!(stats.nlink, 2);

    xhfs.create_hardlink(
        "/deeply/nested/002.hardlink",
        og_path,
        WriteOption { overwrite: false },
    )
    .await?;
    assert_eq!(
        xhfs.total_remaining_capacity().await?,
        (after_write_remaining - 4096) - 4096,
        "Hardlink should spend only INode space + 1 block for storing refered inumber"
    );
    let stats = xhfs.stats(og_path, false).await?.unwrap();
    assert_eq!(stats.nlink, 3);

    xhfs.unlink("/deeply/nested/002.hardlink").await?;
    let stats = xhfs.stats(og_path, false).await?.unwrap();
    assert_eq!(stats.nlink, 2);

    xhfs.unlink(og_path).await?;
    // At this point, the original file is no more, but we still own a link to it
    let stats = xhfs
        .stats("/deeply/nested/001.hardlink", true /*/!\*/)
        .await?
        .unwrap();
    assert_eq!(stats.nlink, 1, "should still hold the physical file");

    // We can even read its content
    let content = xhfs.fread("/deeply/nested/001.hardlink").await?;
    assert_eq!(homework, content, "same as original");

    xhfs.unlink("/deeply/nested/001.hardlink").await?;
    let now_remaining = xhfs.total_remaining_capacity().await?;

    assert_eq!(
        space_before_any_writes, now_remaining,
        "nlink = 0 should free up everything"
    );

    Ok(())
}

#[tokio::test]
async fn test_fstream_from_no_file() -> eyre::Result<()> {
    let xhfs = create_simple_memory_xhfs(128 * 1000 * 1000).await?;
    let mut memory_stream = Cursor::new(b"123456789".to_vec());
    let block_size = 4;
    xhfs.fwrite_stream(
        "hello.txt",
        &mut memory_stream,
        block_size,
        WriteOption { overwrite: false },
    )
    .await?;

    assert_eq!(xhfs.fread("hello.txt").await?, b"123456789");

    let inode = xhfs.resolve_path("hello.txt").await?;

    let meta_exts = xhfs
        .find_full_extent_metadata(inode.extent_addr, Some(10))
        .await?;
    let header = xhfs.get_header().await?;

    assert_eq!(meta_exts.len(), 3, "each write shot should use 1 extent");
    // each burst of 4 bytes wastes 4096 B (1 block) - 4 B - 16 B extent header space
    assert_eq!(meta_exts[0].size_span(), header.format.block_size_bytes);
    assert_eq!(meta_exts[1].size_span(), header.format.block_size_bytes);
    assert_eq!(meta_exts[2].size_span(), header.format.block_size_bytes);

    Ok(())
}

#[tokio::test]
async fn test_xhfs_fread_stream() -> eyre::Result<()> {
    let xhfs = create_simple_memory_xhfs(128 * 1000 * 1000).await?;
    let part1 = vec![1; 72];
    let part2 = vec![2; 111];
    let part3 = vec![3; 1];

    xhfs.fwrite("/file.bin", vec![], WriteOption { overwrite: false })
        .await?;
    xhfs.fappend("/file.bin", part1.clone()).await?;
    xhfs.fappend("/file.bin", part2.clone()).await?;
    xhfs.fappend("/file.bin", part3.clone()).await?;

    let examples = [13, 17, 22, 100000];
    for chunk_size in examples {
        let mut stream = xhfs.fread_stream("/file.bin", chunk_size).await.unwrap();
        let mut result = vec![];
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            result.extend(chunk);
        }
        let mut c = 0;
        assert_eq!(
            part1,
            result[c..c + 72],
            "chunk size {chunk_size} for part 1"
        );
        c += 72;
        assert_eq!(
            part2,
            result[c..c + 111],
            "chunk size {chunk_size} for part 2"
        );
        c += 111;
        assert_eq!(
            part3,
            result[c..c + 1],
            "chunk size {chunk_size} for part 3"
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_xhfs_real_prelayout_deduction() -> eyre::Result<()> {
    let mod_8_required_by_kv = 7;
    {
        let xhfs = create_simple_memory_xhfs(84221009 + mod_8_required_by_kv)
            .await
            .unwrap();
        let format = xhfs.get_header().await?.format;
        assert_eq!(
            format,
            Format {
                param_data_block_count_per_group: 20480,
                param_inode_count_per_group: 4096,
                block_size_bytes: 4096,
                group_count: 1
            },
        );
    }
    {
        let xhfs = create_simple_memory_xhfs(2 * (84221009 + mod_8_required_by_kv))
            .await
            .unwrap();
        let format = xhfs.get_header().await?.format;
        assert_eq!(
            format,
            Format {
                param_data_block_count_per_group: 20480,
                param_inode_count_per_group: 4096,
                block_size_bytes: 4096,
                group_count: 2
            },
        );
    }
    {
        let xhfs = create_simple_memory_xhfs(800 * 1000 * 1000).await.unwrap();
        let format = xhfs.get_header().await?.format;
        assert_eq!(
            format,
            Format {
                param_data_block_count_per_group: 20480,
                param_inode_count_per_group: 4096,
                block_size_bytes: 4096,
                group_count: 9
            },
        );
    }

    Ok(())
}
