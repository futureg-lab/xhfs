use crate::{
    bfs::BruteFS,
    device::{
        Device,
        disk::Controller,
        fs_device::FsDevice,
        kv_device::{KVDevice, MemoryKV},
        logical::LogicalDevice,
    },
    utils::normalize_path,
};
use bytesize::ByteSize;
use eyre::Context;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    collections::{HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    path::PathBuf,
    str::FromStr,
    sync::Arc,
};
use tokio::sync::RwLock;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub password: Option<String>,
    pub devices: Vec<DeviceConfig>,
    pub configuration: Configuration,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum DeviceConfig {
    #[serde(rename = "kvmemory")]
    KVMemory {
        name: String,
        slot_capacity_bytes: u64,
    },
    #[serde(rename = "file")]
    File { name: String, path: String },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub enum DeviceType {
    Memory,
    Fs,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Configuration {
    pub logical: Vec<LogicalDeviceConfig>,
    pub layout: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bytes(pub ByteSize);

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LogicalDeviceConfig {
    pub name: String,
    pub include: Vec<String>,
    pub capacity: Bytes,
    pub max_concurrent: u8,
}

impl DeviceConfig {
    pub fn name(&self) -> String {
        match self {
            DeviceConfig::KVMemory { name, .. } => name,
            DeviceConfig::File { name, .. } => name,
        }
        .to_owned()
    }

    pub fn payload_discriminator(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        match self {
            DeviceConfig::File { path, .. } => {
                let path = normalize_path(path);
                path.hash(&mut hasher);
            }
            DeviceConfig::KVMemory { .. } => {
                rand::random::<u64>().hash(&mut hasher);
            }
        };
        hasher.finish()
    }
}

impl Config {
    pub fn example() -> eyre::Result<Self> {
        let text = r#"
password: helloworld
devices:
    - type: file
      name: bloc1
      path: ./part1.bin
    - type: file
      name: bloc2
      path: ./part1-replica.bin
    - type: file
      name: bloc3
      path: ./part3.bin
configuration:
    logical:
        - name: dev1
          include: [bloc1, bloc2]
          capacity: "2 MiB"
          max_concurrent: 2
        - name: dev2
          include: [bloc3]
          capacity: "2 MiB"
          max_concurrent: 1
    # Final storage layout
    # [dev1: 0 - 2MB] [dev2: 2MiB - 4MiB]
    layout: [dev1, dev2]    
"#;
        serde_yaml::from_str(text).map_err(|e| e.into())
    }

    pub fn load<P: Into<PathBuf>>(path: P) -> eyre::Result<Config> {
        let path: PathBuf = path.into();
        let config;
        if path.exists() {
            let content = std::fs::read_to_string(&path)
                .wrap_err_with(|| format!("Loading {}", path.to_string_lossy()))?;
            config = serde_yaml::from_str(&content).map_err(|e| eyre::eyre!(e))?;
        } else {
            tracing::info!(
                "Creating configuration file at {}",
                std::env::current_dir().unwrap().to_string_lossy()
            );
            config = Config::example()?;
            std::fs::write(path, serde_yaml::to_string(&config).unwrap())?;
        }
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> eyre::Result<()> {
        let mut device_names = HashSet::new();
        let mut payloads = HashSet::new();
        for d in &self.devices {
            if !device_names.insert(d.name()) {
                eyre::bail!("Duplicate device name: {}", d.name());
            }
            if !payloads.insert(d.payload_discriminator()) {
                eyre::bail!("Already used payload used by {}", d.name());
            }
        }

        let mut logical_names = HashSet::new();
        for l in &self.configuration.logical {
            if !logical_names.insert(l.name.clone()) {
                eyre::bail!("Duplicate logical device: {}", l.name);
            }
            for dep in &l.include {
                if !device_names.contains(dep) {
                    eyre::bail!(
                        "Logical '{}' references unknown device {}, available {}",
                        l.name,
                        dep,
                        Vec::from_iter(device_names.into_iter()).join(", ")
                    );
                }
            }
        }

        let mut seen_device = HashSet::new();
        for name in &self.configuration.layout {
            if !logical_names.contains(name) {
                eyre::bail!(
                    "Layout references unknown logical device {name}, available {}",
                    Vec::from_iter(logical_names.into_iter()).join(", ")
                );
            }
            if !seen_device.insert(name) {
                eyre::bail!("Cannot use device {name} more than once in the layout");
            }
        }

        if self.devices.is_empty() {
            eyre::bail!("No devices configured");
        }
        if self.configuration.logical.is_empty() {
            eyre::bail!("No logical devices configured");
        }
        if self.configuration.layout.is_empty() {
            eyre::bail!("Layout cannot be empty");
        }

        Ok(())
    }

    pub async fn materialize(
        &self,
        format_new: bool,
        password_override: Option<String>,
    ) -> eyre::Result<BruteFS> {
        let mut logdev_instances = HashMap::new();

        for logdev in &self.configuration.logical {
            let mut group = vec![];
            for devname in &logdev.include {
                let dev = self
                    .devices
                    .iter()
                    .find(|d| d.name() == *devname)
                    .ok_or_else(|| eyre::eyre!("Could not find refered device {}", logdev.name))?;

                let capacity = logdev.capacity.0.as_u64();
                let instance = match dev {
                    DeviceConfig::KVMemory {
                        slot_capacity_bytes,
                        ..
                    } => Arc::new(KVDevice {
                        store: Arc::new(RwLock::new(MemoryKV(HashMap::new()))),
                        total_slots: (capacity / *slot_capacity_bytes) as usize,
                        slot_capacity: *slot_capacity_bytes as usize,
                    }) as Arc<dyn Device>,
                    DeviceConfig::File { path, .. } => {
                        Arc::new(FsDevice::new(path, capacity as usize).await?) as Arc<dyn Device>
                    }
                };
                group.push(instance);
            }

            logdev_instances.insert(
                logdev.name.clone(),
                LogicalDevice::new(logdev.max_concurrent as usize, group).wrap_err_with(|| {
                    eyre::eyre!("Creating logical devices out of {:?}", logdev.include)
                })?,
            );
        }

        let mut final_layout = vec![];
        for devname in &self.configuration.layout {
            let instance = logdev_instances.get(devname).ok_or_else(|| {
                eyre::eyre!("Could not find refered materialized  device instance {devname}")
            })?;
            final_layout.push(instance.to_owned());
        }

        let ctrl = Controller::from(final_layout).await.wrap_err_with(|| {
            eyre::eyre!(
                "Creating controller out of layout {:?}",
                self.configuration.layout
            )
        })?;

        let password = if password_override.is_some() {
            password_override
        } else {
            self.password.clone()
        };
        if format_new {
            BruteFS::format_new(ctrl, password).await
        } else {
            BruteFS::from_formatted(ctrl, password).await
        }
    }
}

impl Serialize for Bytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct V;

        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Bytes;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "byte size like '10 MB' or integer")
            }

            fn visit_u64<E>(self, v: u64) -> Result<Bytes, E>
            where
                E: serde::de::Error,
            {
                Ok(Bytes(ByteSize(v)))
            }

            fn visit_str<E>(self, v: &str) -> Result<Bytes, E>
            where
                E: serde::de::Error,
            {
                ByteSize::from_str(v)
                    .map(Bytes)
                    .map_err(|e| serde::de::Error::custom(e.to_string()))
            }
        }

        deserializer.deserialize_any(V)
    }
}
