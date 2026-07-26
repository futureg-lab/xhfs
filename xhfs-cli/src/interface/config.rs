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
use url::Url;
use xhfs_core::{
    device::{
        ConcreteDevice, disk::Controller, fs_device::FsDevice, http_device::HttpKV, kv_device::*,
        logical::*,
    },
    utils::normalize_path,
    xhfs::{crypto::KeyDerivation, *},
};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub password: Option<String>,
    #[serde(default)]
    pub key_derivation: KeyDerivation,
    pub devices: Vec<DeviceConfig>,
    pub configuration: Configuration,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum DeviceConfig {
    #[serde(rename = "kvhttp")]
    KVHttp {
        name: String,
        slot_capacity_bytes: u64,
        url: Url,
        headers: Option<HashMap<String, String>>,
    },
    #[serde(rename = "file")]
    File { name: String, path: String },
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
            DeviceConfig::File { name, .. } => name,
            DeviceConfig::KVHttp { name, .. } => name,
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
            DeviceConfig::KVHttp { name, url, .. } => {
                name.hash(&mut hasher);
                url.hash(&mut hasher);
            }
        };
        hasher.finish()
    }
}

impl Config {
    pub fn example() -> eyre::Result<Self> {
        let text = r#"
password: helloworld
# key_derivation:
#   algorithm: argon2     # default: sha256
devices:
  - type: file
    name: blob1
    path: ./part1.bin
  - type: file
    name: blob2
    path: ./part1-replica.bin
  - type: file
    name: blob3
    path: ./part2.bin
configuration:
  logical:
    - name: dev1
      include: [blob1, blob2]
      capacity: "50 MiB"
      max_concurrent: 2
    - name: dev2
      include: [blob3]
      capacity: "50 MiB"
      max_concurrent: 1
  # Final storage layout
  # [dev1: 0 - 50MB] [dev2: 50MiB - 100MiB]
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
    ) -> eyre::Result<XHFS> {
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
                    DeviceConfig::KVHttp {
                        slot_capacity_bytes,
                        url,
                        headers,
                        name,
                    } => ConcreteDevice::KVDevice(KVDevice {
                        store: Arc::new(HttpKV {
                            url: url.clone(),
                            key_prefix: name.clone(),
                            headers: headers.clone().unwrap_or_default(),
                        }),
                        total_slots: (capacity / *slot_capacity_bytes) as usize,
                        slot_capacity: *slot_capacity_bytes as usize,
                    }),
                    DeviceConfig::File { path, .. } => ConcreteDevice::FsDevice(
                        FsDevice::new(path, capacity as usize, format_new).await?,
                    ),
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
            XHFS::format_new(ctrl, password, self.key_derivation.clone()).await
        } else {
            XHFS::from_formatted(ctrl, password, self.key_derivation.clone()).await
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
