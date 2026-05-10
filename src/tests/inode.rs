use crate::disk::{INode, INodeValue};

#[test]
pub fn test_inode_name() -> eyre::Result<()> {
    assert!(
        INode {
            name: "a𝔘𝔱𝔣8混合テキスト💀null\0byte".to_string(),
            mtime: 12345678,
            ctime: 23456789,
            value: INodeValue::Directory { list_addr: 1234 },
        }
        .serialize()
        .is_err()
    );

    Ok(())
}

#[test]
pub fn binary_serialization_inode() -> eyre::Result<()> {
    let inode = INode {
        name: "UTF8_inode_test_🔥_file_名前_123_✔️".to_string(),
        mtime: 12345678,
        ctime: 23456789,
        value: INodeValue::Directory { list_addr: 1234 },
    };
    let data = inode.serialize()?;
    let inode_reconstr = INode::deserialize(&data)?;

    assert_eq!(inode, inode_reconstr, "utf8 example");

    Ok(())
}
