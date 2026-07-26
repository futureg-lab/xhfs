use chacha20::{
    ChaCha20,
    cipher::{KeyIvInit, StreamCipher, StreamCipherSeek},
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod argon2_params {
    #[inline(always)]
    pub const fn default_memory_cost() -> u32 {
        65536
    }

    #[inline(always)]
    pub const fn default_time_cost() -> u32 {
        3
    }

    #[inline(always)]
    pub const fn default_parallelism() -> u32 {
        4
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "algorithm")]
pub enum KeyDerivation {
    #[serde(rename = "sha256")]
    #[default]
    Sha256,
    #[serde(rename = "argon2")]
    Argon2 {
        #[serde(default = "argon2_params::default_memory_cost")]
        memory_cost: u32,
        #[serde(default = "argon2_params::default_time_cost")]
        time_cost: u32,
        #[serde(default = "argon2_params::default_parallelism")]
        parallelism: u32,
    },
}

#[derive(Clone)]
pub struct Crypto {
    /// ChaCha20 standardizes on 256-bit key
    key: [u8; 32],
    /// ChaCha20 standardizes on 96-bit nonce
    pub nonce: [u8; 12],
}

impl Crypto {
    pub fn new(password: &str, nonce: [u8; 12], kd: &KeyDerivation) -> Self {
        let key = match kd {
            KeyDerivation::Sha256 => {
                let hash = Sha256::digest(password.as_bytes());
                let mut key = [0u8; 32];
                key.copy_from_slice(&hash);
                key
            }
            KeyDerivation::Argon2 {
                memory_cost,
                time_cost,
                parallelism,
            } => {
                let mut key = [0u8; 32];
                let params = argon2::Params::new(*memory_cost, *time_cost, *parallelism, Some(32))
                    .expect("invalid argon2 params");
                argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
                    .hash_password_into(password.as_bytes(), &nonce, &mut key)
                    .expect("argon2 hash failed");
                key
            }
        };

        Self { key, nonce }
    }

    pub fn apply(&self, addr: u64, data: &mut [u8]) {
        let mut cipher = ChaCha20::new(&self.key.into(), &self.nonce.into());
        cipher.seek(addr);
        cipher.apply_keystream(data);
    }

    pub fn gen_nonce() -> [u8; 12] {
        let mut nonce = [0u8; 12];
        let mut rng = rand::rng();
        rng.fill_bytes(&mut nonce);
        nonce
    }
}
