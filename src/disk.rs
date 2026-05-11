use crate::device::logical::LogicalDevice;
use eyre::Context;

#[derive(Clone)]
pub struct PinnedDevice {
    start: usize,
    end: usize,
    device: LogicalDevice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum INodeValue {
    File { data_addr: u64 },
    Directory { list_addr: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct INode {
    pub name: String,
    pub mtime: u64,
    pub ctime: u64,
    pub value: INodeValue,
}

impl INode {
    pub fn serialize(&self) -> eyre::Result<Vec<u8>> {
        if self.name.len() > 255 {
            eyre::bail!("Entry name cannot exceed 255 characters");
        }
        if self
            .name
            .bytes()
            .any(|b| matches!(b, b'\n' | b'\r' | b'\t' | 0x00))
        {
            eyre::bail!("Entry name contains invalid control characters");
        }

        let mut buf = Vec::with_capacity(256 + 1 + 1 + 8 + 8 + 8);

        let mut name_bytes = [0u8; 256];
        name_bytes[..self.name.len()].copy_from_slice(self.name.as_bytes());
        buf.push(self.name.len() as u8);
        buf.extend_from_slice(&name_bytes);

        let (kind, addr) = match &self.value {
            INodeValue::File { data_addr } => (0u8, *data_addr),
            INodeValue::Directory { list_addr } => (1u8, *list_addr),
        };
        buf.push(kind);
        buf.extend_from_slice(&addr.to_le_bytes());

        buf.extend_from_slice(&self.mtime.to_le_bytes());
        buf.extend_from_slice(&self.ctime.to_le_bytes());

        Ok(buf)
    }

    pub fn deserialize(data: &[u8]) -> eyre::Result<Self> {
        let expected_size = 256 + 1 + 1 + 8 + 8 + 8;
        if data.len() != expected_size {
            eyre::bail!(
                "Invalid inode size: expected {}, got {}",
                expected_size,
                data.len()
            );
        }

        let name_len = data[0] as usize;
        let name_bytes = &data[1..256];
        let name = std::str::from_utf8(&name_bytes[..name_len])
            .wrap_err("Invalid UTF-8 in inode name")?
            .to_string();

        let kind = data[257];
        let addr_start = 258;
        let addr_end = addr_start + 8;
        let addr = u64::from_le_bytes(data[addr_start..addr_end].try_into().unwrap());
        let value = match kind {
            0 => INodeValue::File { data_addr: addr },
            1 => INodeValue::Directory { list_addr: addr },
            _ => eyre::bail!("Invalid inode kind: {kind}"),
        };

        let mtime_start = addr_end;
        let mtime_end = mtime_start + 8;
        let mtime = u64::from_le_bytes(data[mtime_start..mtime_end].try_into().unwrap());

        let ctime_start = mtime_end;
        let ctime_end = ctime_start + 8;
        let ctime = u64::from_le_bytes(data[ctime_start..ctime_end].try_into().unwrap());

        Ok(INode {
            name,
            mtime,
            ctime,
            value,
        })
    }
}

#[derive(Clone)]
pub struct Controller {
    pinned_devices: Vec<PinnedDevice>,
}

// TODO:
// from yaml config
// chunks:
//   - type: fs
//     allocate: ...
//     path: A.bin
//   - type: kv # addr resolution should be fun
//     allocate: ..
//     backend: memory | http (makes request to remote kv like Cloudflare) | redis | s3
impl Controller {
    pub async fn from(devices: Vec<LogicalDevice>) -> eyre::Result<Self> {
        let mut logical_end = 0;
        let mut pinned_devices = vec![];

        for device in devices {
            let size = device.validate_layout().await?;
            let start = logical_end;
            logical_end += size;
            pinned_devices.push(PinnedDevice {
                start,
                end: logical_end,
                device,
            });
        }

        Ok(Self { pinned_devices })
    }

    pub fn total_capacity(&self) -> Option<usize> {
        if let Some((sd, ed)) = self.pinned_devices.first().zip(self.pinned_devices.last()) {
            return Some(ed.end - sd.start);
        }
        None
    }

    pub async fn write(&self, mut logical_addr: usize, data: &[u8]) -> eyre::Result<()> {
        let mut plan = vec![];
        let mut remaining = data;
        while !remaining.is_empty() {
            let Some(pinned) = self
                .pinned_devices
                .iter()
                .find(|d| logical_addr >= d.start && logical_addr < d.end)
            else {
                eyre::bail!("No device address range covers address {logical_addr}");
            };

            let local_offset = logical_addr - pinned.start;
            let max_len = (pinned.end - logical_addr).min(remaining.len());

            plan.push((&pinned.device, local_offset, remaining[..max_len].to_vec()));

            logical_addr += max_len;
            remaining = &remaining[max_len..];
        }

        for (dev, offset, chunk) in plan {
            dev.write(offset, &chunk).await?;
        }

        Ok(())
    }

    pub async fn read(&self, mut logical_addr: usize, mut size: usize) -> eyre::Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(size);

        while size > 0 {
            let Some(pinned) = self
                .pinned_devices
                .iter()
                .find(|d| logical_addr >= d.start && logical_addr < d.end)
            else {
                eyre::bail!("No device address range covers address {logical_addr}");
            };

            let local_offset = logical_addr - pinned.start;
            let available_in_device = pinned.end - logical_addr;
            let read_len = available_in_device.min(size);
            let chunk = pinned.device.read(local_offset, read_len).await?;

            buf.extend_from_slice(&chunk);

            logical_addr += read_len;
            size -= read_len;
        }

        Ok(buf)
    }
}
