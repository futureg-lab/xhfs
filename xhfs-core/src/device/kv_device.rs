use crate::device::{Device, KeyValue};
use async_trait::async_trait;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;

pub struct MemoryKV(pub RwLock<HashMap<u64, Vec<u8>>>);

#[async_trait]
impl KeyValue for MemoryKV {
    async fn set(&self, k: u64, v: Vec<u8>) -> eyre::Result<()> {
        let mut kv = self.0.write().await;
        kv.insert(k, v);
        Ok(())
    }

    async fn get(&self, k: &u64) -> eyre::Result<Option<Vec<u8>>> {
        let kv = self.0.write().await;
        Ok(kv.get(k).cloned())
    }
}

#[derive(Clone)]
pub struct KVDevice {
    pub total_slots: usize,
    pub slot_capacity: usize,
    pub store: Arc<dyn KeyValue + Send + Sync>,
}

#[async_trait]
impl Device for KVDevice {
    fn name(&self) -> String {
        "kv-generic".to_string()
    }

    async fn capacity(&self) -> eyre::Result<usize> {
        Ok(self.total_slots * self.slot_capacity)
    }

    async fn write(&self, addr: usize, data: &[u8]) -> eyre::Result<()> {
        let mut remaining = data;
        let mut current_addr = addr;
        while !remaining.is_empty() {
            let slot = current_addr / self.slot_capacity;
            let offset = current_addr % self.slot_capacity;
            if slot >= self.total_slots {
                eyre::bail!("Write out of bounds");
            }

            let writable = (self.slot_capacity - offset).min(remaining.len());
            let key = slot as u64;
            let mut slot_buf = self
                .store
                .get(&key)
                .await?
                .unwrap_or(vec![0u8; self.slot_capacity]);

            if slot_buf.len() < self.slot_capacity {
                slot_buf.resize(self.slot_capacity, 0);
            }
            slot_buf[offset..offset + writable].copy_from_slice(&remaining[..writable]);
            self.store.set(key, slot_buf).await?;

            remaining = &remaining[writable..];
            current_addr += writable;
        }

        Ok(())
    }

    async fn read(&self, addr: usize, size: usize) -> eyre::Result<Vec<u8>> {
        let mut remaining = size;
        let mut current_addr = addr;
        let mut buf = Vec::with_capacity(size);
        while remaining > 0 {
            let slot = current_addr / self.slot_capacity;
            let offset = current_addr % self.slot_capacity;
            if slot >= self.total_slots {
                eyre::bail!("Read out of bounds");
            }
            let readable = (self.slot_capacity - offset).min(remaining);
            let key = slot as u64;
            let slot_buf = self
                .store
                .get(&key)
                .await?
                .unwrap_or(vec![0u8; self.slot_capacity]);

            buf.extend_from_slice(&slot_buf[offset..offset + readable]);

            remaining -= readable;
            current_addr += readable;
        }

        Ok(buf)
    }
}
