use crate::device::KeyValue;
use async_trait::async_trait;
use std::collections::HashMap;

#[derive(Clone)]
pub struct HttpKV {
    pub url: String,
    pub key_prefix: String,
    pub headers: HashMap<String, String>,
}

impl HttpKV {
    pub fn to_internal(&self, k: u64) -> String {
        format!("xhfs-{}-{}", self.key_prefix, k)
    }
}

#[async_trait]
impl KeyValue for HttpKV {
    async fn set(&self, k: u64, v: Vec<u8>) -> eyre::Result<()> {
        let client = reqwest::Client::new();
        let k = self.to_internal(k);
        let mut req = client.put(format!("{}/xhfs/{}", self.url, k)).body(v);
        for (key, val) in &self.headers {
            req = req.header(key, val);
        }

        let res = req.send().await?;
        if !res.status().is_success() {
            eyre::bail!(
                "Failed to set key {}, remote server responded with {}",
                k,
                res.status()
            )
        }

        Ok(())
    }

    async fn get(&self, k: &u64) -> eyre::Result<Option<Vec<u8>>> {
        let client = reqwest::Client::new();
        let k = self.to_internal(*k);
        let mut req = client.get(format!("{}/xhfs/{}", self.url, k));
        for (key, val) in &self.headers {
            req = req.header(key, val);
        }

        let res = req.send().await?;
        match res.status() {
            reqwest::StatusCode::OK => {
                let bytes = res.bytes().await?.to_vec();
                Ok(Some(bytes))
            }
            reqwest::StatusCode::NOT_FOUND => Ok(None),
            status => eyre::bail!("Failed to get key {k}, remote server responded with {status}"),
        }
    }
}
