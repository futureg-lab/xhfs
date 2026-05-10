use crate::block::Block;

#[derive(Clone)]
pub struct Disk {
    size: usize,
    blocks: Vec<Block>,
}

#[derive(Debug, Clone)]
pub enum INodeKind {
    File { addr: usize },
    INodes { inodes: Vec<INode> },
}

#[derive(Debug, Clone)]
pub struct INode {
    pub name: String,
    pub mtime: usize,
    pub ctime: usize,
    pub kind: INodeKind,
}

pub fn new() {
    // pub fn format(size: uszie) -> Self {}
}
