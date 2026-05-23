use async_trait::async_trait;

pub mod disk;
pub mod fs_device;
pub mod http_device;
pub mod kv_device;
pub mod logical;

#[async_trait]
pub trait Device: Send + Sync {
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

#[derive(Clone)]
pub enum ConcreteDevice {
    FsDevice(fs_device::FsDevice),
    KVDevice(kv_device::KVDevice),
    // IDEA:
    // We could easily allow
    // KVLogical(Box<logical::LogicalDevice>),
}

/// This wrapper avoids lifetime type gymnastics
#[async_trait]
impl Device for ConcreteDevice {
    fn name(&self) -> String {
        match self {
            ConcreteDevice::FsDevice(fs_device) => fs_device.name(),
            ConcreteDevice::KVDevice(kvdevice) => kvdevice.name(),
        }
    }

    async fn capacity(&self) -> eyre::Result<usize> {
        match self {
            ConcreteDevice::FsDevice(fs_device) => fs_device.capacity().await,
            ConcreteDevice::KVDevice(kvdevice) => kvdevice.capacity().await,
        }
    }

    async fn write(&self, addr: usize, data: &[u8]) -> eyre::Result<()> {
        match self {
            ConcreteDevice::FsDevice(fs_device) => fs_device.write(addr, data).await,
            ConcreteDevice::KVDevice(kvdevice) => kvdevice.write(addr, data).await,
        }
    }

    async fn read(&self, addr: usize, size: usize) -> eyre::Result<Vec<u8>> {
        match self {
            ConcreteDevice::FsDevice(fs_device) => fs_device.read(addr, size).await,
            ConcreteDevice::KVDevice(kvdevice) => kvdevice.read(addr, size).await,
        }
    }
}
