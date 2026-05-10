use std::path::PathBuf;

use crate::device::{Device, fs_device::FsDevice};
use tokio::fs;

#[tokio::test]
async fn test_fs_device() -> eyre::Result<()> {
    let test_file = PathBuf::from("test.bin");
    if test_file.exists() {
        fs::remove_file(&test_file).await?;
    }

    let blob = FsDevice {
        file: test_file,
        size: 10,
    };
    blob.init().await?;

    let data = "ABC D";
    let addr = 4;
    let size = data.len();
    blob.write(addr, data.as_bytes()).await?;

    assert_eq!(data.as_bytes(), blob.read(addr, size).await?);
    Ok(())
}
