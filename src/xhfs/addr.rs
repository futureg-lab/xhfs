#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddrKind {
    Some(u64),
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaybeU64 {
    inner: AddrKind,
}

impl Default for MaybeU64 {
    fn default() -> Self {
        Self {
            inner: AddrKind::None,
        }
    }
}

impl From<u64> for MaybeU64 {
    fn from(value: u64) -> Self {
        Self::from(value)
    }
}

impl From<usize> for MaybeU64 {
    fn from(value: usize) -> Self {
        Self::from(value as u64)
    }
}

impl Into<usize> for MaybeU64 {
    fn into(self) -> usize {
        self.get() as usize
    }
}

impl MaybeU64 {
    pub fn get(&self) -> u64 {
        match self.inner {
            AddrKind::Some(addr) => addr,
            AddrKind::None => 0,
        }
    }

    pub fn to_optional(&self) -> Option<u64> {
        match self.inner {
            AddrKind::Some(addr) => Some(addr),
            AddrKind::None => None,
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self.inner, AddrKind::None)
    }

    pub fn is_some(&self) -> bool {
        !self.is_none()
    }

    pub fn from(addr: u64) -> Self {
        Self {
            inner: match addr {
                0 => AddrKind::None,
                _ => AddrKind::Some(addr),
            },
        }
    }

    pub fn serialize(&self) -> eyre::Result<Vec<u8>> {
        Ok(match self.inner {
            AddrKind::Some(addr) => {
                if addr == 0 {
                    eyre::bail!("address cannot be both existing and be 0")
                }
                addr.to_le_bytes().to_vec()
            }
            AddrKind::None => 0u64.to_le_bytes().to_vec(),
        })
    }

    pub fn deserialize(data: [u8; 8]) -> Self {
        let addr = u64::from_le_bytes(data).try_into().unwrap();
        Self {
            inner: match addr {
                0 => AddrKind::None,
                _ => AddrKind::Some(addr),
            },
        }
    }

    pub fn serialized_size(&self) -> usize {
        8
    }
}
