use crate::xhfs::{addr::MaybeU64, crypto::Crypto, ds::*};

#[test]
pub fn test_basic_binary_serialization() -> eyre::Result<()> {
    {
        let original = INode {
            inumber: 66441234,
            nlink: 42,
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
        let original = XHFSHeader {
            version: 42,
            format: Format {
                block_size_bytes: 12348794,
                blocks_per_group: 234567,
                group_count: 4,
            },
            chacha20_nonce: Crypto::gen_nonce(),
        };
        let data = original.serialize()?;
        let reconstr = XHFSHeader::deserialize(&data)?;
        assert_eq!(original, reconstr, "XHFSHeader serde");
    }
    Ok(())
}

#[test]
pub fn test_bitmap_serialization() -> eyre::Result<()> {
    {
        let size = 100;
        let bitmap = Bitmap::new_from_bits_count(size);
        assert_eq!(bitmap.map.len(), size);
        for i in 0..size {
            assert_eq!(bitmap.get(i)?, false);
        }
    }

    {
        let mut bitmap = Bitmap::new_from_bits_count(64);
        bitmap.set(0, true)?;
        bitmap.set(31, true)?;
        bitmap.set(63, true)?;

        assert_eq!(bitmap.get(0)?, true);
        assert_eq!(bitmap.get(31)?, true);
        assert_eq!(bitmap.get(63)?, true);

        assert_eq!(bitmap.get(1)?, false, "untouched bits remain false");
        assert_eq!(bitmap.get(32)?, false, "untouched bits remain false");

        bitmap.set(31, false)?;
        assert_eq!(bitmap.get(31)?, false, "set existing true bit to false");
    }

    {
        let mut bitmap = Bitmap::new_from_bits_count(10);
        assert!(bitmap.get(10).is_err(), "out of bounds");
        assert!(bitmap.get(100).is_err(), "out of bounds");
        assert!(bitmap.set(10, true).is_err(), "out of bounds");
        assert!(bitmap.set(100, false).is_err(), "out of bounds");
    }

    {
        // odd bit length that doesn't align cleanly with 8-bit bytes
        let bit_size = 77;
        let mut bitmap = Bitmap::new_from_bits_count(bit_size);
        bitmap.set(0, true)?;
        bitmap.set(12, true)?;
        bitmap.set(76, true)?;

        let expected_size = bitmap.serialized_size();
        let serialized_data = bitmap.serialize()?;
        assert_eq!(serialized_data.len(), expected_size);

        let deserialized = Bitmap::deserialize(&serialized_data)?;
        assert_eq!(bitmap, deserialized, "ser-de");

        assert_eq!(deserialized.map.len(), bit_size);
        assert_eq!(deserialized.get(0)?, true);
        assert_eq!(deserialized.get(12)?, true);
        assert_eq!(deserialized.get(76)?, true);
        assert_eq!(deserialized.get(75)?, false);
        assert!(
            deserialized.get(77).is_err(),
            "original constraint len survived"
        );
    }

    Ok(())
}
