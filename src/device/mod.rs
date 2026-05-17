use async_trait::async_trait;

pub mod disk;
pub mod fs_device;
pub mod http_device;
pub mod kv_device;
pub mod logical;

#[async_trait]
pub trait Device {
    fn name(&self) -> String;
    async fn capacity(&self) -> eyre::Result<usize>;
    async fn write(&self, addr: usize, data: &[u8]) -> eyre::Result<()>;
    async fn read(&self, addr: usize, size: usize) -> eyre::Result<Vec<u8>>;
}

#[async_trait]
pub trait KeyValue {
    async fn set(&self, k: u64, v: Vec<u8>) -> eyre::Result<()>;
    async fn get(&self, k: &u64) -> eyre::Result<Option<Vec<u8>>>;
}
