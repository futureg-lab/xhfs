use crate::device::logical::LogicalDevice;

#[derive(Clone)]
pub struct PinnedDevice {
    start: usize,
    end: usize,
    device: LogicalDevice,
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
    pub async fn from<D>(devices: D) -> eyre::Result<Self>
    where
        D: IntoIterator<Item = LogicalDevice>,
    {
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
        tracing::debug!(" Writting {} bytes at 0x{:x}", data.len(), logical_addr);
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
        tracing::debug!(" Reading {size} bytes at 0x{logical_addr:x}");
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
