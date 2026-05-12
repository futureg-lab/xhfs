use crate::bfs::{Directory, Extent, INode, INodeKind, MaybeU64};

#[test]
pub fn test_basic_binary_serialization() -> eyre::Result<()> {
    {
        let original = INode {
            kind: INodeKind::Symlink,
            extent_addr: MaybeU64::Some(1234),
            mtime: 567845678,
            ctime: 123457523,
            utime: 444487844,
        };
        let data = original.serialize()?;
        let reconstr = INode::deserialize(&data)?;
        assert_eq!(original, reconstr, "INode serde");
    }

    {
        let original = Extent {
            next: MaybeU64::Some(1234),
            data: vec![1, 2, 3, 4, 5, 6, 7, 8, 9],
        };
        let data = original.serialize()?;
        let reconstr = Extent::deserialize(&data)?;
        assert_eq!(original, reconstr, "Extent serde");
    }

    {
        let original = Directory { entries: vec![] };
        let data = original.serialize()?;
        let reconstr = Directory::deserialize(&data)?;
        assert_eq!(original, reconstr, "Directory serde empty");

        let original = Directory {
            entries: vec![
                // ("a𝔘𝔱𝔣8混合テキスト💀null\0byte".to_string(), 1234), // \0 not allowed
                ("a𝔘𝔱𝔣8混合テキスト💀".to_string(), 1234),
                ("UTF8_inode_test_🔥_file_名前_123_✔️".to_string(), 4567),
            ],
        };
        let data = original.serialize()?;
        let reconstr = Directory::deserialize(&data)?;
        assert_eq!(original, reconstr, "Directory serde");
    }
    Ok(())
}
