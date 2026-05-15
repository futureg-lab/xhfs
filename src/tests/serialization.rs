use crate::bfs::{addr::MaybeU64, crypto::Crypto, ds::*};

#[test]
pub fn test_basic_binary_serialization() -> eyre::Result<()> {
    {
        let original = INode {
            kind: INodeKind::Symlink,
            extent_addr: MaybeU64::from(1234),
            total_file_size: 42,
            mtime: 567845678,
            ctime: 123457523,
        };
        let data = original.serialize()?;
        let reconstr = INode::deserialize(&data)?;
        assert_eq!(original, reconstr, "INode serde");
    }

    {
        let original = Extent {
            next: MaybeU64::from(1234),
            data: vec![],
        };
        let data = original.serialize()?;
        let reconstr = Extent::deserialize(&data)?;
        assert_eq!(original, reconstr, "Extent serde 1");

        let original = Extent {
            next: MaybeU64::from(1234),
            data: vec![1, 2, 3, 4, 5, 6, 7, 8, 9],
        };
        let data = original.serialize()?;
        let reconstr = Extent::deserialize(&data)?;
        assert_eq!(original, reconstr, "Extent serde 2");
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

    {
        let mut original = BruteFsHeader {
            version: 42,
            extent_freed: AddressVector::allocate(11),
            chacha20_nonce: Crypto::gen_nonce(),
        };
        original.extent_freed.items[5] = AddressSlot {
            addr: MaybeU64::from(1234),
            capacity: 444,
        };
        original.extent_freed.items[10] = AddressSlot {
            addr: MaybeU64::from(234567),
            capacity: 4567112234,
        };

        let data = original.serialize()?;
        let reconstr = BruteFsHeader::deserialize(&data)?;
        assert_eq!(original, reconstr, "BruteFsHeader serde");
    }
    Ok(())
}
