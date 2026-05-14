use std::{collections::HashMap, path::PathBuf, sync::Arc};

use crate::device::{
    Device,
    disk::Controller,
    fs_device::FsDevice,
    kv_device::{KVDevice, MemoryKV},
    logical::LogicalDevice,
};
use tokio::{fs, sync::RwLock};

macro_rules! test_read_write_simple {
    ($device:expr) => {{
        async {
            let device = $device;

            let data = "ABC D";
            let addr = 4;
            let size = data.len();

            device.write(addr, data.as_bytes()).await?;

            assert_eq!(
                data.as_bytes(),
                device.read(addr, size).await?,
                "write and read back same data"
            );

            Ok::<(), eyre::Report>(())
        }
    }};
}

macro_rules! test_read_write_complex {
    ($device:expr) => {{
        async {
            let device = $device;

            device.write(2, b"HELLOWORLD").await?;

            assert_eq!(
                device.read(2, 10).await?,
                b"HELLOWORLD",
                "verify exact readback"
            );

            assert_eq!(
                device.read(0, 16).await?,
                vec![
                    0, 0, b'H', b'E', b'L', b'L', b'O', b'W', b'O', b'R', b'L', b'D', 0, 0, 0, 0
                ],
                "verify surrounding zero-padding"
            );

            device.write(5, b"XYZ").await?;

            assert_eq!(
                device.read(2, 10).await?,
                b"HELXYZORLD",
                "partial overwrite inside existing slot chain"
            );

            assert_eq!(device.read(4, 5).await?, b"LXYZO", "mid-slot read");

            Ok::<(), eyre::Report>(())
        }
    }};
}

#[tokio::test]
async fn test_kv_device() -> eyre::Result<()> {
    let store = MemoryKV(HashMap::new());
    test_read_write_simple!(KVDevice {
        store: Arc::new(RwLock::new(store)),
        total_slots: 10,
        slot_capacity: 4,
    })
    .await?;

    // Layout:
    // slot0: [00 01 02]
    // slot1: [03 04 05]
    // slot2: [06 07 08]
    // slot3: [09 0A 0B]
    // slot4: [0C 0D 0E]
    // slot5: [0F 10 11]
    // slot6: [12 13 14]
    let store = MemoryKV(HashMap::new());
    test_read_write_complex!(KVDevice {
        store: Arc::new(RwLock::new(store)),
        total_slots: 7,
        slot_capacity: 3,
    })
    .await?;

    Ok(())
}

#[tokio::test]
async fn test_fs_device() -> eyre::Result<()> {
    let test_file = PathBuf::from("test.bin");
    if test_file.exists() {
        fs::remove_file(&test_file).await?;
    }

    let device = FsDevice::new(test_file, 20).await?;
    test_read_write_simple!(&device).await?;
    test_read_write_complex!(&device).await?;

    Ok(())
}

#[tokio::test]
async fn test_logical_device() -> eyre::Result<()> {
    let test_files = [
        PathBuf::from("testlogical.bin"),
        PathBuf::from("testlogical2.bin"),
    ];
    for test_file in &test_files {
        if test_file.exists() {
            fs::remove_file(test_file).await?;
        }
    }

    let dev1 = FsDevice::new(test_files[0].clone(), 20).await?;
    let dev2 = FsDevice::new(test_files[1].clone(), 20).await?;
    let dev3 = KVDevice {
        store: Arc::new(RwLock::new(MemoryKV(HashMap::new()))),
        total_slots: 6,
        slot_capacity: 7,
    };
    let dev4 = KVDevice {
        store: Arc::new(RwLock::new(MemoryKV(HashMap::new()))),
        total_slots: 4,
        slot_capacity: 5,
    };

    let dev1 = LogicalDevice::new(
        2,
        [
            Arc::from(dev1) as Arc<dyn Device>,
            Arc::from(dev4) as Arc<dyn Device>,
            Arc::from(dev2) as Arc<dyn Device>,
        ],
    )?;
    let dev2 = LogicalDevice::new(2, [Arc::from(dev3) as Arc<dyn Device>])?;
    test_read_write_complex!(&dev1).await?;
    test_read_write_complex!(&dev2).await?;

    let ctrl = Controller::from([dev1, dev2]).await?;
    assert_eq!(
        ctrl.total_capacity().unwrap(),
        20 + 6 * 7,
        "total block size accounting for addressing"
    );
    test_read_write_complex!(ctrl).await?;

    Ok(())
}
